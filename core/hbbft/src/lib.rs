
// Copyright 2018-2019 Parity Technologies (UK) Ltd.
// This file is part of Substrate.

// Substrate is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Substrate is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Substrate.  If not, see <http://www.gnu.org/licenses/>.

//! Integration of the GRANDPA finality gadget into substrate.
//!
//! This crate is unstable and the API and usage may change.
//!
//! This crate provides a long-running future that produces finality notifications.
//!
//! # Usage
//!
//! First, create a block-import wrapper with the `block_import` function.
//! The GRANDPA worker needs to be linked together with this block import object,
//! so a `LinkHalf` is returned as well. All blocks imported (from network or consensus or otherwise)
//! must pass through this wrapper, otherwise consensus is likely to break in
//! unexpected ways.
//!
//! Next, use the `LinkHalf` and a local configuration to `run_grandpa_voter`.
//! This requires a `Network` implementation. The returned future should be
//! driven to completion and will finalize blocks in the background.
//!
//! # Changing authority sets
//!
//! The rough idea behind changing authority sets in GRANDPA is that at some point,
//! we obtain agreement for some maximum block height that the current set can
//! finalize, and once a block with that height is finalized the next set will
//! pick up finalization from there.
//!
//! Technically speaking, this would be implemented as a voting rule which says,
//! "if there is a signal for a change in N blocks in block B, only vote on
//! chains with length NUM(B) + N if they contain B". This conditional-inclusion
//! logic is complex to compute because it requires looking arbitrarily far
//! back in the chain.
//!
//! Instead, we keep track of a list of all signals we've seen so far (across
//! all forks), sorted ascending by the block number they would be applied at.
//! We never vote on chains with number higher than the earliest handoff block
//! number (this is num(signal) + N). When finalizing a block, we either apply
//! or prune any signaled changes based on whether the signaling block is
//! included in the newly-finalized chain.
use runtime_primitives::traits::{NumberFor, Block as BlockT, Zero};
use network::consensus_gossip::{self as network_gossip, MessageIntent, ValidatorContext};
use network::{config::Roles, PeerId};
use parity_codec::{Encode, Decode};
use fg_primitives::{AuthorityId,AuthoritySignature};

use substrate_telemetry::{telemetry, CONSENSUS_DEBUG};
use log::{trace, debug, warn};
use futures::prelude::*;
use futures::sync::mpsc;
use serde_json as json;
use hex;
/// Proposer factory.
pub struct ProposerFactory<C, A> where A: txpool::ChainApi {
	/// The client instance.
	pub client: Arc<C>,
	/// The transaction pool.
	pub transaction_pool: Arc<TransactionPool<A>>,
}

impl<C, A> consensus_common::Environment<<C as AuthoringApi>::Block> for ProposerFactory<C, A> where
	C: AuthoringApi,
	<C as ProvideRuntimeApi>::Api: BlockBuilderApi<<C as AuthoringApi>::Block>,
	A: txpool::ChainApi<Block=<C as AuthoringApi>::Block>,
	client::error::Error: From<<C as AuthoringApi>::Error>,
	Proposer<<C as AuthoringApi>::Block, C, A>: consensus_common::Proposer<<C as AuthoringApi>::Block>,
{
	type Proposer = Proposer<<C as AuthoringApi>::Block, C, A>;
	type Error = error::Error;

	fn init(
		&self,
		parent_header: &<<C as AuthoringApi>::Block as BlockT>::Header,
	) -> Result<Self::Proposer, error::Error> {
		let parent_hash = parent_header.hash();

		let id = BlockId::hash(parent_hash);

		info!("Starting consensus session on top of parent {:?}", parent_hash);

		let proposer = Proposer {
			client: self.client.clone(),
			parent_hash,
			parent_id: id,
			parent_number: *parent_header.number(),
			transaction_pool: self.transaction_pool.clone(),
			now: Box::new(time::Instant::now),
		};

		Ok(proposer)
	}
}



/// The proposer logic.
pub struct Proposer<Block: BlockT, C, A: txpool::ChainApi> {
	client: Arc<C>,
	parent_hash: <Block as BlockT>::Hash,
	parent_id: BlockId<Block>,
	parent_number: <<Block as BlockT>::Header as HeaderT>::Number,
	transaction_pool: Arc<TransactionPool<A>>,
	now: Box<dyn Fn() -> time::Instant>,
}

impl<Block, C, A> consensus_common::Proposer<<C as AuthoringApi>::Block> for Proposer<Block, C, A> where
	Block: BlockT,
	C: AuthoringApi<Block=Block>,
	<C as ProvideRuntimeApi>::Api: BlockBuilderApi<Block>,
	A: txpool::ChainApi<Block=Block>,
	client::error::Error: From<<C as AuthoringApi>::Error>
{
	type Create = Result<<C as AuthoringApi>::Block, error::Error>;
	type Error = error::Error;

	fn propose(
		&self,
		inherent_data: InherentData,
		inherent_digests: DigestFor<Block>,
		max_duration: time::Duration,
	) -> Result<<C as AuthoringApi>::Block, error::Error>
	{
		// leave some time for evaluation and block finalization (33%)
		let deadline = (self.now)() + max_duration - max_duration / 3;
		self.propose_with(inherent_data, inherent_digests, deadline)
	}
}



impl<C, A> consensus_common::Environment<<C as AuthoringApi>::Block> for ProposerFactory<C, A> where
	C: AuthoringApi,
	<C as ProvideRuntimeApi>::Api: BlockBuilderApi<<C as AuthoringApi>::Block>,
	A: txpool::ChainApi<Block=<C as AuthoringApi>::Block>,
	client::error::Error: From<<C as AuthoringApi>::Error>,
	Proposer<<C as AuthoringApi>::Block, C, A>: consensus_common::Proposer<<C as AuthoringApi>::Block>,
{
	type Proposer = Proposer<<C as AuthoringApi>::Block, C, A>;
	type Error = error::Error;

	fn init(
		&self,
		parent_header: &<<C as AuthoringApi>::Block as BlockT>::Header,
	) -> Result<Self::Proposer, error::Error> {
		let parent_hash = parent_header.hash();

		let id = BlockId::hash(parent_hash);

		info!("Starting consensus session on top of parent {:?}", parent_hash);

		let proposer = Proposer {
			client: self.client.clone(),
			parent_hash,
			parent_id: id,
			parent_number: *parent_header.number(),
			transaction_pool: self.transaction_pool.clone(),
			now: Box::new(time::Instant::now),
		};

		Ok(proposer)
	}
}



/// The proposer logic.
pub struct Proposer<Block: BlockT, C, A: txpool::ChainApi> {
	client: Arc<C>,
	parent_hash: <Block as BlockT>::Hash,
	parent_id: BlockId<Block>,
	parent_number: <<Block as BlockT>::Header as HeaderT>::Number,
	transaction_pool: Arc<TransactionPool<A>>,
	now: Box<dyn Fn() -> time::Instant>,
}

impl<Block, C, A> consensus_common::Proposer<<C as AuthoringApi>::Block> for Proposer<Block, C, A> where
	Block: BlockT,
	C: AuthoringApi<Block=Block>,
	<C as ProvideRuntimeApi>::Api: BlockBuilderApi<Block>,
	A: txpool::ChainApi<Block=Block>,
	client::error::Error: From<<C as AuthoringApi>::Error>
{
	type Create = Result<<C as AuthoringApi>::Block, error::Error>;
	type Error = error::Error;

	fn propose(
		&self,
		inherent_data: InherentData,
		inherent_digests: DigestFor<Block>,
		max_duration: time::Duration,
	) -> Result<<C as AuthoringApi>::Block, error::Error>
	{
		// leave some time for evaluation and block finalization (33%)
		let deadline = (self.now)() + max_duration - max_duration / 3;
		self.propose_with(inherent_data, inherent_digests, deadline)
	}
}

impl<Block, C, A> Proposer<Block, C, A>	where
	Block: BlockT,
	C: AuthoringApi<Block=Block>,
	<C as ProvideRuntimeApi>::Api: BlockBuilderApi<Block>,
	A: txpool::ChainApi<Block=Block>,
	client::error::Error: From<<C as AuthoringApi>::Error>,
{
	fn propose_with(
		&self,
		inherent_data: InherentData,
		inherent_digests: DigestFor<Block>,
		deadline: time::Instant,
	) -> Result<<C as AuthoringApi>::Block, error::Error>
	{
		use runtime_primitives::traits::BlakeTwo256;

		/// If the block is full we will attempt to push at most
		/// this number of transactions before quitting for real.
		/// It allows us to increase block utilization.
		const MAX_SKIPPED_TRANSACTIONS: usize = 8;

		let block = self.client.build_block(
			&self.parent_id,
			inherent_data,
			inherent_digests.clone(),
			|block_builder| {
				// proceed with transactions
				let mut is_first = true;
				let mut skipped = 0;
				let mut unqueue_invalid = Vec::new();
				let pending_iterator = self.transaction_pool.ready();

				debug!("Attempting to push transactions from the pool.");
				for pending in pending_iterator {
					if (self.now)() > deadline {
						debug!("Consensus deadline reached when pushing block transactions, proceeding with proposing.");
						break;
					}

					trace!("[{:?}] Pushing to the block.", pending.hash);
					match block_builder.push_extrinsic(pending.data.clone()) {
						Ok(()) => {
							debug!("[{:?}] Pushed to the block.", pending.hash);
						}
						Err(error::Error::ApplyExtrinsicFailed(ApplyError::FullBlock)) => {
							if is_first {
								debug!("[{:?}] Invalid transaction: FullBlock on empty block", pending.hash);
								unqueue_invalid.push(pending.hash.clone());
							} else if skipped < MAX_SKIPPED_TRANSACTIONS {
								skipped += 1;
								debug!(
									"Block seems full, but will try {} more transactions before quitting.",
									MAX_SKIPPED_TRANSACTIONS - skipped
								);
							} else {
								debug!("Block is full, proceed with proposing.");
								break;
							}
						}
						Err(e) => {
							debug!("[{:?}] Invalid transaction: {}", pending.hash, e);
							unqueue_invalid.push(pending.hash.clone());
						}
					}

					is_first = false;
				}

				self.transaction_pool.remove_invalid(&unqueue_invalid);
			})?;

		info!("Prepared block for proposing at {} [hash: {:?}; parent_hash: {}; extrinsics: [{}]]",
			block.header().number(),
			<<C as AuthoringApi>::Block as BlockT>::Hash::from(block.header().hash()),
			block.header().parent_hash(),
			block.extrinsics()
				.iter()
				.map(|xt| format!("{}", BlakeTwo256::hash_of(xt)))
				.collect::<Vec<_>>()
				.join(", ")
		);
		telemetry!(CONSENSUS_INFO; "prepared_block_for_proposing";
			"number" => ?block.header().number(),
			"hash" => ?<<C as AuthoringApi>::Block as BlockT>::Hash::from(block.header().hash()),
		);

		let substrate_block = Decode::decode(&mut block.encode().as_slice())
			.expect("blocks are defined to serialize to substrate blocks correctly; qed");

		assert!(evaluation::evaluate_initial(
			&substrate_block,
			&self.parent_hash,
			self.parent_number,
		).is_ok());

		Ok(substrate_block)
	}
}


// Aura (Authority-round) consensus in substrate.
//
// Aura works by having a list of authorities A who are expected to roughly
// agree on the current time. Time is divided up into discrete slots of t
// seconds each. For each slot s, the author of that slot is A[s % |A|].
//
// The author is allowed to issue one block but not more during that slot,
// and it will be built upon the longest valid chain that has been seen.
//
// Blocks from future steps will be either deferred or rejected depending on how
// far in the future they are.
//
// NOTE: Aura itself is designed to be generic over the crypto used.
// #![forbid(missing_docs, unsafe_code)]
use std::{sync::Arc, time::Duration, thread, marker::PhantomData, hash::Hash, fmt::Debug};

use parity_codec::{Encode, Decode, Codec};
use consensus_common::{self, BlockImport, Environment, Proposer,
	ForkChoiceStrategy, ImportBlock, BlockOrigin, Error as ConsensusError,
	SelectChain, well_known_cache_keys::{self, Id as CacheKeyId}
};
use consensus_common::import_queue::{
	Verifier, BasicQueue, BoxBlockImport, BoxJustificationImport, BoxFinalityProofImport,
	BoxFinalityProofRequestBuilder,
};
use client::{
	block_builder::api::BlockBuilder as BlockBuilderApi,
	blockchain::ProvideCache,
	runtime_api::ApiExt,
	error::Result as CResult,
	backend::AuxStore,
};

use runtime_primitives::{generic::{self, BlockId, OpaqueDigestItemId}, Justification};
use runtime_primitives::traits::{Block, Header, DigestItemFor, ProvideRuntimeApi, Zero, Member};

use primitives::Pair;
use inherents::{InherentDataProviders, InherentData};

use futures::{Future, IntoFuture, future};
use parking_lot::Mutex;
use tokio_timer::Timeout;
use log::{error, warn, debug, info, trace};

use srml_aura::{
	InherentType as AuraInherent, AuraInherentData,
	timestamp::{TimestampInherentData, InherentType as TimestampInherent, InherentError as TIError}
};
use substrate_telemetry::{telemetry, CONSENSUS_TRACE, CONSENSUS_DEBUG, CONSENSUS_WARN, CONSENSUS_INFO};

use slots::{CheckedHeader, SlotData, SlotWorker, SlotInfo, SlotCompatible};
use slots::{SignedDuration, check_equivocation};

pub use aura_primitives::*;
pub use consensus_common::SyncOracle;
pub use digest::CompatibleDigestItem;


type AuthorityId<P> = <P as Pair>::Public;



pub const INHERENT_IDENTIFIER: InherentIdentifier = *b"snakeshroom";

/// Get slot author for given block along with authorities.
fn slot_author<P: Pair>(slot_num: u64, authorities: &[AuthorityId<P>]) -> Option<&AuthorityId<P>> {
	if authorities.is_empty() { return None }

	let idx = slot_num % (authorities.len() as u64);
	assert!(idx <= usize::max_value() as u64,
		"It is impossible to have a vector with length beyond the address space; qed");

	let current_author = authorities.get(idx as usize)
		.expect("authorities not empty; index constrained to list length;\
				this is a valid index; qed");

	Some(current_author)
}



pub fn start_slot_worker<B, C, W, T, SO, SC>(
	slot_duration: SlotDuration<T>,
	client: C,
	worker: W,
	sync_oracle: SO,
	inherent_data_providers: InherentDataProviders,
	timestamp_extractor: SC,
) -> impl Future<Item = (), Error = ()>
where
	B: BlockT,
	C: SelectChain<B> + Clone,
	W: SlotWorker<B>,
	SO: SyncOracle + Send + Clone,
	SC: SlotCompatible,
	T: SlotData + Clone,
{
	let SlotDuration(slot_duration) = slot_duration;

	// rather than use a timer interval, we schedule our waits ourselves
	let mut authorship = Slots::<SC>::new(
		slot_duration.slot_duration(),
		inherent_data_providers,
		timestamp_extractor,
	).map_err(|e| debug!(target: "slots", "Faulty timer: {:?}", e))
		.for_each(move |slot_info| {
			// only propose when we are not syncing.
			if sync_oracle.is_major_syncing() {
				debug!(target: "slots", "Skipping proposal slot due to sync.");
				return Either::B(future::ok(()));
			}

			let slot_num = slot_info.number;
			let chain_head = match client.best_chain() {
				Ok(x) => x,
				Err(e) => {
					warn!(target: "slots", "Unable to author block in slot {}. \
					no best block header: {:?}", slot_num, e);
					return Either::B(future::ok(()));
				}
			};

			Either::A(worker.on_slot(chain_head, slot_info).into_future().map_err(
				|e| warn!(target: "slots", "Encountered consensus error: {:?}", e),
			))
		});

	future::poll_fn(move ||
		loop {
			let mut authorship = std::panic::AssertUnwindSafe(&mut authorship);
			match std::panic::catch_unwind(move || authorship.poll()) {
				Ok(Ok(Async::Ready(()))) =>
					warn!(target: "slots", "Slots stream has terminated unexpectedly."),
				Ok(Ok(Async::NotReady)) => break Ok(Async::NotReady),
				Ok(Err(())) => warn!(target: "slots", "Authorship task terminated unexpectedly. Restarting"),
				Err(e) => {
					if let Some(s) = e.downcast_ref::<&'static str>() {
						warn!(target: "slots", "Authorship task panicked at {:?}", s);
					}

					warn!(target: "slots", "Restarting authorship task");
				}
			}
		}
	)
}

pub type BadgerImportQueue<B> = BasicQueue<B>;

/// Start the badger worker. The returned future should be run in a exec? runtime.
pub fn start_badger<Block, C,  E, I, P, SO, Error, H, A,N, X>(
						client : Arc<C>,
						t_pool: Arc<TransactionPool<A>>,
						pub network: N,
                        config: crate::Config,
                        sync_oracle: SO, 
					    on_exit: X,) -> ::client::error::Result<impl Future<Item=(),Error=()> + Send + 'static> where
	A: txpool::ChainApi
	Block::Hash: Ord,
	SO: SyncOracle + Send + Sync + Clone,
	E: CallExecutor<Block, Blake2Hasher> + Send + Sync + 'static,
	Error: ::std::error::Error + Send + From<::consensus_common::Error> + From<I::Error> + 'static,
	C: ProvideRuntimeApi + ProvideCache<B> + AuxStore + Send + Sync,
	N: Network<Block> + Send + Sync + 'static,
{


	let pending_iterator = self.transaction_pool.ready();
	debug!("Attempting to push transactions from the pool.");
	for pending in pending_iterator {}

	let worker = BadgerWorker {
		client: client.clone(),
		sync_oracle: sync_oracle.clone(),
		network: network_bridge,
		transaction_pool: t_pool.clone()
	};

	Ok( worker)
}

struct BadgerWorker<C, E, I, P, SO,Inbound> {
	client: Arc<C>,
	block_import: Arc<Mutex<I>>,
	network: NetworkBridge,
	transaction_pool: Arc<TransactionPool<A>>,
	sync_oracle: SO,
	blocks_in:Inbound 
}

pub struct BadgerVerifier<C, P> {
	client: Arc<C>,
	phantom: PhantomData<P>,
	inherent_data_providers: inherents::InherentDataProviders,
}

impl<C, P> BadgerVerifier<C, P>
	where P: Send + Sync + 'static
{
	fn check_inherents<B: BlockT>(
		&self,
		block: B,
		block_id: BlockId<B>,
		inherent_data: InherentData,
		timestamp_now: u64,
	) -> Result<(), String>
		where C: ProvideRuntimeApi, C::Api: BlockBuilderApi<B>
	{
		//empty for now
			Ok(())
		
	}
}

impl<B: BlockT, C, P> Verifier<B> for BadgerVerifier<C, P> where
	C: ProvideRuntimeApi + Send + Sync + client::backend::AuxStore + ProvideCache<B>,
	C::Api: BlockBuilderApi<B> + AuraApi<B, AuthorityId<P>>,
	DigestItemFor<B>: CompatibleDigestItem<P>,
	P: Pair + Send + Sync + 'static,
	P::Public: Send + Sync + Hash + Eq + Clone + Decode + Encode + Debug + AsRef<P::Public> + 'static,
	P::Signature: Encode + Decode,
{
	fn verify(
		&self,
		origin: BlockOrigin,
		header: B::Header,
		justification: Option<Justification>,
		mut body: Option<Vec<B::Extrinsic>>,
	) -> Result<(ImportBlock<B>, Option<Vec<(CacheKeyId, Vec<u8>)>>), String> {
        // dummy for the moment
     	let hash = header.hash();
		let parent_hash = *header.parent_hash();
		let import_block = ImportBlock {
					origin,
					header: pre_header,
					post_digests: vec![],
					body,
					finalized: true,
					justification,
					auxiliary: Vec::new(),
					fork_choice: ForkChoiceStrategy::LongestChain,
				};

				Ok((import_block, None))
		
	}
}



impl<H, B, C, E, I, P, Error, SO> SlotWorker<B> for BadgerWorker<C, E, I, P, SO> where
	B: Block<Header=H>,
	C: ProvideRuntimeApi + ProvideCache<B> + Sync,
	C::Api: AuraApi<B, AuthorityId<P>>,
	E: Environment<B, Error=Error>,
	E::Proposer: Proposer<B, Error=Error>,
	<<E::Proposer as Proposer<B>>::Create as IntoFuture>::Future: Send + 'static,
	H: Header<Hash=B::Hash>,
	I: BlockImport<B> + Send + Sync + 'static,
	P: Pair + Send + Sync + 'static,
	P::Public: Member + Encode + Decode + Hash,
	P::Signature: Member + Encode + Decode + Hash + Debug,
	SO: SyncOracle + Send + Clone,
	Error: ::std::error::Error + Send + From<::consensus_common::Error> + From<I::Error> + 'static,
{
	type OnSlot = Box<dyn Future<Item=(), Error=consensus_common::Error> + Send>;

	fn on_slot(
		&self,
		chain_head: B::Header,
		slot_info: SlotInfo,
	) -> Self::OnSlot {
		let pair = self.local_key.clone();
		let public_key = self.local_key.public();
		let client = self.client.clone();
		let block_import = self.block_import.clone();
		let env = self.env.clone();

		let (timestamp, slot_num, slot_duration) =
			(slot_info.timestamp, slot_info.number, slot_info.duration);

		let authorities = match authorities(client.as_ref(), &BlockId::Hash(chain_head.hash())) {
			Ok(authorities) => authorities,
			Err(e) => {
				warn!(
					"Unable to fetch authorities at block {:?}: {:?}",
					chain_head.hash(),
					e
				);
				telemetry!(CONSENSUS_WARN; "aura.unable_fetching_authorities";
					"slot" => ?chain_head.hash(), "err" => ?e
				);
				return Box::new(future::ok(()));
			}
		};

		if !self.force_authoring && self.sync_oracle.is_offline() && authorities.len() > 1 {
			debug!(target: "aura", "Skipping proposal slot. Waiting for the network.");
			telemetry!(CONSENSUS_DEBUG; "aura.skipping_proposal_slot";
				"authorities_len" => authorities.len()
			);
			return Box::new(future::ok(()));
		}
		let maybe_author = slot_author::<P>(slot_num, &authorities);
		let proposal_work = match maybe_author {
			None => return Box::new(future::ok(())),
			Some(author) => if author == &public_key {
				debug!(
					target: "aura", "Starting authorship at slot {}; timestamp = {}",
					slot_num,
					timestamp
				);
				telemetry!(CONSENSUS_DEBUG; "aura.starting_authorship";
					"slot_num" => slot_num, "timestamp" => timestamp
				);

				// we are the slot author. make a block and sign it.
				let proposer = match env.init(&chain_head) {
					Ok(p) => p,
					Err(e) => {
						warn!("Unable to author block in slot {:?}: {:?}", slot_num, e);
						telemetry!(CONSENSUS_WARN; "aura.unable_authoring_block";
							"slot" => slot_num, "err" => ?e
						);
						return Box::new(future::ok(()))
					}
				};

				let remaining_duration = slot_info.remaining_duration();
				// deadline our production to approx. the end of the
				// slot
				Timeout::new(
					proposer.propose(
						slot_info.inherent_data,
						generic::Digest {
							logs: vec![
								<DigestItemFor<B> as CompatibleDigestItem<P>>::aura_pre_digest(slot_num),
							],
						},
						remaining_duration,
					).into_future(),
					remaining_duration,
				)
			} else {
				return Box::new(future::ok(()));
			}
		};

		Box::new(proposal_work.map(move |b| {
			// minor hack since we don't have access to the timestamp
			// that is actually set by the proposer.
			let slot_after_building = SignedDuration::default().slot_now(slot_duration);
			if slot_after_building != slot_num {
				info!(
					"Discarding proposal for slot {}; block production took too long",
					slot_num
				);
				telemetry!(CONSENSUS_INFO; "aura.discarding_proposal_took_too_long";
					"slot" => slot_num
				);
				return
			}

			let (header, body) = b.deconstruct();
			let pre_digest: Result<u64, String> = find_pre_digest::<B, P>(&header);
			if let Err(e) = pre_digest {
				error!(target: "aura", "FATAL ERROR: Invalid pre-digest: {}!", e);
				return
			} else {
				trace!(target: "aura", "Got correct number of seals.  Good!")
			};

			let header_num = header.number().clone();
			let parent_hash = header.parent_hash().clone();

			// sign the pre-sealed hash of the block and then
			// add it to a digest item.
			let header_hash = header.hash();
			let signature = pair.sign(header_hash.as_ref());
			let signature_digest_item = <DigestItemFor<B> as CompatibleDigestItem<P>>::aura_seal(signature);

			let import_block: ImportBlock<B> = ImportBlock {
				origin: BlockOrigin::Own,
				header,
				justification: None,
				post_digests: vec![signature_digest_item],
				body: Some(body),
				finalized: false,
				auxiliary: Vec::new(),
				fork_choice: ForkChoiceStrategy::LongestChain,
			};

			info!("Pre-sealed block for proposal at {}. Hash now {:?}, previously {:?}.",
					header_num,
					import_block.post_header().hash(),
					header_hash
			);
			telemetry!(CONSENSUS_INFO; "aura.pre_sealed_block";
				"header_num" => ?header_num,
				"hash_now" => ?import_block.post_header().hash(),
				"hash_previously" => ?header_hash
			);

			if let Err(e) = block_import.lock().import_block(import_block, Default::default()) {
				warn!(target: "aura", "Error with block built on {:?}: {:?}",
						parent_hash, e);
				telemetry!(CONSENSUS_WARN; "aura.err_with_block_built_on";
					"hash" => ?parent_hash, "err" => ?e
				);
			}
		}).map_err(|e| consensus_common::Error::ClientImport(format!("{:?}", e)).into()))
	}
}

macro_rules! aura_err {
	($($i: expr),+) => {
		{ debug!(target: "aura", $($i),+)
		; format!($($i),+)
		}
	};
}

fn find_pre_digest<B: Block, P: Pair>(header: &B::Header) -> Result<u64, String>
	where DigestItemFor<B>: CompatibleDigestItem<P>,
		P::Signature: Decode,
		P::Public: Encode + Decode + PartialEq + Clone,
{
	let mut pre_digest: Option<u64> = None;
	for log in header.digest().logs() {
		trace!(target: "aura", "Checking log {:?}", log);
		match (log.as_aura_pre_digest(), pre_digest.is_some()) {
			(Some(_), true) => Err(aura_err!("Multiple AuRa pre-runtime headers, rejecting!"))?,
			(None, _) => trace!(target: "aura", "Ignoring digest not meant for us"),
			(s, false) => pre_digest = s,
		}
	}
	pre_digest.ok_or_else(|| aura_err!("No AuRa pre-runtime digest found"))
}


/// check a header has been signed by the right key. If the slot is too far in the future, an error will be returned.
/// if it's successful, returns the pre-header and the digest item containing the seal.
///
/// This digest item will always return `Some` when used with `as_aura_seal`.
//
// FIXME #1018 needs misbehavior types
fn check_header<C, B: Block, P: Pair>(
	client: &C,
	slot_now: u64,
	mut header: B::Header,
	hash: B::Hash,
	authorities: &[AuthorityId<P>],
) -> Result<CheckedHeader<B::Header, (u64, DigestItemFor<B>)>, String> where
	DigestItemFor<B>: CompatibleDigestItem<P>,
	P::Signature: Decode,
	C: client::backend::AuxStore,
	P::Public: AsRef<P::Public> + Encode + Decode + PartialEq + Clone,
{
	

			Ok(CheckedHeader::Checked(header, (0, seal)))
		
	
}

/// A verifier for Aura blocks.
pub struct AuraVerifier<C, P> {
	client: Arc<C>,
	phantom: PhantomData<P>,
	inherent_data_providers: inherents::InherentDataProviders,
}

impl<C, P> AuraVerifier<C, P>
	where P: Send + Sync + 'static
{
	fn check_inherents<B: Block>(
		&self,
		block: B,
		block_id: BlockId<B>,
		inherent_data: InherentData,
		timestamp_now: u64,
	) -> Result<(), String>
		where C: ProvideRuntimeApi, C::Api: BlockBuilderApi<B>
	{
		const MAX_TIMESTAMP_DRIFT_SECS: u64 = 60;

		let inherent_res = self.client.runtime_api().check_inherents(
			&block_id,
			block,
			inherent_data,
		).map_err(|e| format!("{:?}", e))?;

		if !inherent_res.ok() {
			inherent_res
				.into_errors()
				.try_for_each(|(i, e)| match TIError::try_from(&i, &e) {
					Some(TIError::ValidAtTimestamp(timestamp)) => {
						// halt import until timestamp is valid.
						// reject when too far ahead.
						if timestamp > timestamp_now + MAX_TIMESTAMP_DRIFT_SECS {
							return Err("Rejecting block too far in future".into());
						}

						let diff = timestamp.saturating_sub(timestamp_now);
						info!(
							target: "aura",
							"halting for block {} seconds in the future",
							diff
						);
						telemetry!(CONSENSUS_INFO; "aura.halting_for_future_block";
							"diff" => ?diff
						);
						thread::sleep(Duration::from_secs(diff));
						Ok(())
					},
					Some(TIError::Other(e)) => Err(e.into()),
					None => Err(self.inherent_data_providers.error_to_string(&i, &e)),
				})
		} else {
			Ok(())
		}
	}
}

#[forbid(deprecated)]
impl<B: Block, C, P> Verifier<B> for AuraVerifier<C, P> where
	C: ProvideRuntimeApi + Send + Sync + client::backend::AuxStore + ProvideCache<B>,
	C::Api: BlockBuilderApi<B> + AuraApi<B, AuthorityId<P>>,
	DigestItemFor<B>: CompatibleDigestItem<P>,
	P: Pair + Send + Sync + 'static,
	P::Public: Send + Sync + Hash + Eq + Clone + Decode + Encode + Debug + AsRef<P::Public> + 'static,
	P::Signature: Encode + Decode,
{
	fn verify(
		&self,
		origin: BlockOrigin,
		header: B::Header,
		justification: Option<Justification>,
		mut body: Option<Vec<B::Extrinsic>>,
	) -> Result<(ImportBlock<B>, Option<Vec<(CacheKeyId, Vec<u8>)>>), String> {
		let mut inherent_data = self.inherent_data_providers.create_inherent_data().map_err(String::from)?;
		let (timestamp_now, slot_now, _) = AuraSlotCompatible.extract_timestamp_and_slot(&inherent_data)
			.map_err(|e| format!("Could not extract timestamp and slot: {:?}", e))?;
		let hash = header.hash();
		let parent_hash = *header.parent_hash();
		let authorities = authorities(self.client.as_ref(), &BlockId::Hash(parent_hash))
			.map_err(|e| format!("Could not fetch authorities at {:?}: {:?}", parent_hash, e))?;

		// we add one to allow for some small drift.
		// FIXME #1019 in the future, alter this queue to allow deferring of
		// headers
		let checked_header = check_header::<C, B, P>(
			&self.client,
			slot_now + 1,
			header,
			hash,
			&authorities[..],
		)?;
		match checked_header {
			CheckedHeader::Checked(pre_header, (slot_num, seal)) => {
				// if the body is passed through, we need to use the runtime
				// to check that the internally-set timestamp in the inherents
				// actually matches the slot set in the seal.
				if let Some(inner_body) = body.take() {
					inherent_data.aura_replace_inherent_data(slot_num);
					let block = B::new(pre_header.clone(), inner_body);

					// skip the inherents verification if the runtime API is old.
					if self.client
						.runtime_api()
						.has_api_with::<dyn BlockBuilderApi<B>, _>(&BlockId::Hash(parent_hash), |v| v >= 2)
						.map_err(|e| format!("{:?}", e))?
					{
						self.check_inherents(
							block.clone(),
							BlockId::Hash(parent_hash),
							inherent_data,
							timestamp_now,
						)?;
					}

					let (_, inner_body) = block.deconstruct();
					body = Some(inner_body);
				}

				trace!(target: "aura", "Checked {:?}; importing.", pre_header);
				telemetry!(CONSENSUS_TRACE; "aura.checked_and_importing"; "pre_header" => ?pre_header);

				// Look for an authorities-change log.
				let maybe_keys = pre_header.digest()
					.logs()
					.iter()
					.filter_map(|l| l.try_to::<ConsensusLog<AuthorityId<P>>>(
						OpaqueDigestItemId::Consensus(&AURA_ENGINE_ID)
					))
					.find_map(|l| match l {
						ConsensusLog::AuthoritiesChange(a) => Some(
							vec![(well_known_cache_keys::AUTHORITIES, a.encode())]
						),
						_ => None,
					});

				let import_block = ImportBlock {
					origin,
					header: pre_header,
					post_digests: vec![seal],
					body,
					finalized: false,
					justification,
					auxiliary: Vec::new(),
					fork_choice: ForkChoiceStrategy::LongestChain,
				};

				Ok((import_block, maybe_keys))
			}
			CheckedHeader::Deferred(a, b) => {
				debug!(target: "aura", "Checking {:?} failed; {:?}, {:?}.", hash, a, b);
				telemetry!(CONSENSUS_DEBUG; "aura.header_too_far_in_future";
					"hash" => ?hash, "a" => ?a, "b" => ?b
				);
				Err(format!("Header {:?} rejected: too far in the future", hash))
			}
		}
	}
}

fn initialize_authorities_cache<A, B, C>(client: &C) -> Result<(), ConsensusError> where
	A: Codec,
	B: Block,
	C: ProvideRuntimeApi + ProvideCache<B>,
	C::Api: AuraApi<B, A>,
{
	// no cache => no initialization
	let cache = match client.cache() {
		Some(cache) => cache,
		None => return Ok(()),
	};

	// check if we already have initialized the cache
	let genesis_id = BlockId::Number(Zero::zero());
	let genesis_authorities: Option<Vec<A>> = cache
		.get_at(&well_known_cache_keys::AUTHORITIES, &genesis_id)
		.and_then(|v| Decode::decode(&mut &v[..]));
	if genesis_authorities.is_some() {
		return Ok(());
	}

	let map_err = |error| consensus_common::Error::from(consensus_common::Error::ClientImport(
		format!(
			"Error initializing authorities cache: {}",
			error,
		)));
	let genesis_authorities = authorities(client, &genesis_id)?;
	cache.initialize(&well_known_cache_keys::AUTHORITIES, genesis_authorities.encode())
		.map_err(map_err)?;

	Ok(())
}

#[allow(deprecated)]
fn authorities<A, B, C>(client: &C, at: &BlockId<B>) -> Result<Vec<A>, ConsensusError> where
	A: Codec,
	B: Block,
	C: ProvideRuntimeApi + ProvideCache<B>,
	C::Api: AuraApi<B, A>,
{
	client
		.cache()
		.and_then(|cache| cache
			.get_at(&well_known_cache_keys::AUTHORITIES, at)
			.and_then(|v| Decode::decode(&mut &v[..]))
		)
		.or_else(|| AuraApi::authorities(&*client.runtime_api(), at).ok())
		.ok_or_else(|| consensus_common::Error::InvalidAuthoritiesSet.into())
}

/// The Aura import queue type.
pub type AuraImportQueue<B> = BasicQueue<B>;

/// Register the aura inherent data provider, if not registered already.
fn register_badger_inherent_data_provider(
	inherent_data_providers: &InherentDataProviders,
	slot_duration: u64,
) -> Result<(), consensus_common::Error> {
	Ok(())
/*	if !inherent_data_providers.has_provider(&hbbft::INHERENT_IDENTIFIER) {
		inherent_data_providers
			.register_provider(srml_aura::InherentDataProvider::new(slot_duration))
			.map_err(Into::into)
			.map_err(consensus_common::Error::InherentData)
	} else {
		Ok(())
	}*/
}

/// Start an import queue for the Aura consensus algorithm.
pub fn badger_import_queue<B, C, P>(
	block_import: BoxBlockImport<B>,
	justification_import: Option<BoxJustificationImport<B>>,
	finality_proof_import: Option<BoxFinalityProofImport<B>>,
	finality_proof_request_builder: Option<BoxFinalityProofRequestBuilder<B>>,
	client: Arc<C>,
	inherent_data_providers: InherentDataProviders,
) -> Result<AuraImportQueue<B>, consensus_common::Error> where
	B: Block,
	C: 'static + ProvideRuntimeApi + ProvideCache<B> + Send + Sync + AuxStore,
	C::Api: BlockBuilderApi<B> + AuraApi<B, AuthorityId<P>>,
	DigestItemFor<B>: CompatibleDigestItem<P>,
	P: Pair + Send + Sync + 'static,
	P::Public: Clone + Eq + Send + Sync + Hash + Debug + Encode + Decode + AsRef<P::Public>,
	P::Signature: Encode + Decode,
{
	register_badger_inherent_data_provider(&inherent_data_providers, slot_duration.get())?;
	//initialize_authorities_cache(&*client)?;

	let verifier = Arc::new(
		BadgerVerifier {
			client: client.clone(),
			inherent_data_providers,
			phantom: PhantomData,
		}
	);
	Ok(BasicQueue::new(
		verifier,
		block_import,
		justification_import,
		finality_proof_import,
		finality_proof_request_builder,
	))
}


use crate::{environment, CatchUp, CompactCommit, SignedMessage};
use super::{cost, benefit, Round, SetId};

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

const REBROADCAST_AFTER: Duration = Duration::from_secs(60 * 5);
const CATCH_UP_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const CATCH_UP_PROCESS_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum number of rounds we are behind a peer before issuing a
/// catch up request.
const CATCH_UP_THRESHOLD: u64 = 2;



/// A view of protocol state.
#[derive(Debug)]
struct View<N> {
	round: Round, // the current round we are at.
	set_id: SetId, // the current voter set id.
	last_commit: Option<N>, // commit-finalized block height, if any.
}

impl<N> Default for View<N> {
	fn default() -> Self {
		View {
			round: Round(0),
			set_id: SetId(0),
			last_commit: None,
		}
	}
}

impl<N: Ord> View<N> {
	/// Update the set ID. implies a reset to round 0.
	fn update_set(&mut self, set_id: SetId) {
		if set_id != self.set_id {
			self.set_id = set_id;
			self.round = Round(0);
		}
	}

	/// Consider a round and set ID combination under a current view.
	fn consider_vote(&self, round: Round, set_id: SetId) -> Consider {
		// only from current set
		if set_id < self.set_id { return Consider::RejectPast }
		if set_id > self.set_id { return Consider::RejectFuture }

		// only r-1 ... r+1
		if round.0 > self.round.0.saturating_add(1) { return Consider::RejectFuture }
		if round.0 < self.round.0.saturating_sub(1) { return Consider::RejectPast }

		Consider::Accept
	}

	/// Consider a set-id global message. Rounds are not taken into account, but are implicitly
	/// because we gate on finalization of a further block than a previous commit.
	fn consider_global(&self, set_id: SetId, number: N) -> Consider {
		// only from current set
		if set_id < self.set_id { return Consider::RejectPast }
		if set_id > self.set_id { return Consider::RejectFuture }

		// only commits which claim to prove a higher block number than
		// the one we're aware of.
		match self.last_commit {
			None => Consider::Accept,
			Some(ref num) => if num < &number {
				Consider::Accept
			} else {
				Consider::RejectPast
			}
		}
	}
}

const KEEP_RECENT_ROUNDS: usize = 3;

const BADGER_TOPIC: &str = "itsasnake";
/// Tracks topics we keep messages for.
struct KeepTopics<B: BlockT> {
	current_set: SetId,
	rounds: VecDeque<(Round, SetId)>,
	reverse_map: HashMap<B::Hash, (Option<Round>, SetId)>
}

impl<B: BlockT> KeepTopics<B> {
	fn new() -> Self {
		KeepTopics {
			current_set: SetId(0),
			rounds: VecDeque::with_capacity(KEEP_RECENT_ROUNDS + 1),
			reverse_map: HashMap::new(),
		}
	}

	fn push(&mut self, round: Round, set_id: SetId) {
		self.current_set = std::cmp::max(self.current_set, set_id);
		self.rounds.push_back((round, set_id));

		// the 1 is for the current round.
		while self.rounds.len() > KEEP_RECENT_ROUNDS + 1 {
			let _ = self.rounds.pop_front();
		}

		let mut map = HashMap::with_capacity(KEEP_RECENT_ROUNDS + 2);
		map.insert(super::global_topic::<B>(self.current_set.0), (None, self.current_set));

		for &(round, set) in &self.rounds {
			map.insert(
				super::round_topic::<B>(round.0, set.0),
				(Some(round), set)
			);
		}

		self.reverse_map = map;
	}

	fn topic_info(&self, topic: &B::Hash) -> Option<(Option<Round>, SetId)> {
		self.reverse_map.get(topic).cloned()
	}
}

// topics to send to a neighbor based on their view.
fn neighbor_topics<B: BlockT>(view: &View<NumberFor<B>>) -> Vec<B::Hash> {
	let s = view.set_id;
	let mut topics = vec![
		super::global_topic::<B>(s.0),
		super::round_topic::<B>(view.round.0, s.0),
	];

	if view.round.0 != 0 {
		let r = Round(view.round.0 - 1);
		topics.push(super::round_topic::<B>(r.0, s.0))
	}

	topics
}

/// HB gossip message type.
/// This is the root type that gets encoded and sent on the network.
#[derive(Debug, Encode, Decode)]
pub(super) enum GossipMessage<Block: BlockT> {
	Greeting(GreetingMessage),
	/// Raw Badger data
	BadgerData(Vec<u8>),

	RequestGreeting(),
	/// A neighbor packet. Not repropagated.
    //	Neighbor(VersionedNeighborPacket<NumberFor<Block>>),
}


#[derive(Debug, Encode, Decode)]
pub(super) struct GreetingMessage {
	/// the badgr ID of the peer
	pub(super) myId: AuthorityId,
	/// Signature to verify id
	pub(super) mySig: AuthoritySignature,

}

impl<Block: BlockT> From<GreetingMessage> for GossipMessage<Block> {
	fn from(greet: GreetingMessage) -> Self {
		GossipMessage::Greeting(greet)
	}
}

/// Network level message with topic information.
#[derive(Debug, Encode, Decode)]
pub(super) struct VoteOrPrecommitMessage<Block: BlockT> {
	/// The round this message is from.
	pub(super) round: Round,
	/// The voter set ID this message is from.
	pub(super) set_id: SetId,
	/// The message itself.
	pub(super) message: SignedMessage<Block>,
}

/// Network level commit message with topic information.
#[derive(Debug, Encode, Decode)]
pub(super) struct FullCommitMessage<Block: BlockT> {
	/// The round this message is from.
	pub(super) round: Round,
	/// The voter set ID this message is from.
	pub(super) set_id: SetId,
	/// The compact commit message.
	pub(super) message: CompactCommit<Block>,
}

/// V1 neighbor packet. Neighbor packets are sent from nodes to their peers
/// and are not repropagated. These contain information about the node's state.
#[derive(Debug, Encode, Decode, Clone)]
pub(super) struct NeighborPacket<N> {
	/// The round the node is currently at.
	pub(super) round: Round,
	/// The set ID the node is currently at.
	pub(super) set_id: SetId,
	/// The highest finalizing commit observed.
	pub(super) commit_finalized_height: N,
}

/// A versioned neighbor packet.
#[derive(Debug, Encode, Decode)]
pub(super) enum VersionedNeighborPacket<N> {
	#[codec(index = "1")]
	V1(NeighborPacket<N>),
}

impl<N> VersionedNeighborPacket<N> {
	fn into_neighbor_packet(self) -> NeighborPacket<N> {
		match self {
			VersionedNeighborPacket::V1(p) => p,
		}
	}
}

/// A catch up request for a given round (or any further round) localized by set id.
#[derive(Clone, Debug, Encode, Decode)]
pub(super) struct CatchUpRequestMessage {
	/// The round that we want to catch up to.
	pub(super) round: Round,
	/// The voter set ID this message is from.
	pub(super) set_id: SetId,
}

/// Network level catch up message with topic information.
#[derive(Debug, Encode, Decode)]
pub(super) struct FullCatchUpMessage<Block: BlockT> {
	/// The voter set ID this message is from.
	pub(super) set_id: SetId,
	/// The compact commit message.
	pub(super) message: CatchUp<Block>,
}

/// Misbehavior that peers can perform.
///
/// `cost` gives a cost that can be used to perform cost/benefit analysis of a
/// peer.


struct PeerInfo<N> {
	//view: View<N>,
	id: Option<AuthorityId> //public key
}

impl<N> PeerInfo<N> {
	fn new() -> Self {
		PeerInfo {
			id: None,
		}
	}
    fn new_id(id: AuthorityId) -> Self {
		PeerInfo {
			id: Some(id),
		}
	}
}

/// The peers we're connected do in gossip.
struct Peers<N> {
	inner: HashMap<PeerId, PeerInfo<N>>,
}

impl<N> Default for Peers<N> {
	fn default() -> Self {
		Peers { inner: HashMap::new() }
	}
}

impl<N: Ord> Peers<N> {
	fn new_peer(&mut self, who: PeerId) {
		self.inner.insert(who, PeerInfo::new());
	}

	fn peer_disconnected(&mut self, who: &PeerId) {
		self.inner.remove(who);
	}

	// returns a reference to the new view, if the peer is known.
	fn update_peer_state(&mut self, who: &PeerId, update: NeighborPacket<N>)
		-> Result<Option<&View<N>>, Misbehavior>
	{
		let peer = match self.inner.get_mut(who) {
			None => return Ok(None),
			Some(p) => p,
		};

		let invalid_change = peer.view.set_id > update.set_id
			|| peer.view.round > update.round && peer.view.set_id == update.set_id
			|| peer.view.last_commit.as_ref() > Some(&update.commit_finalized_height);

		if invalid_change {
			return Err(Misbehavior::InvalidViewChange);
		}

		peer.view = View {
			round: update.round,
			set_id: update.set_id,
			last_commit: Some(update.commit_finalized_height),
		};

		trace!(target: "afg", "Peer {} updated view. Now at {:?}, {:?}",
			who, peer.view.round, peer.view.set_id);

		Ok(Some(&peer.view))
	}

	pub fn update_id(&mut self, who: &PeerId, authId: AuthorityId)  {
		let peer = match self.inner.get_mut(who) {
		    None =>  {
				 self.inner.insert(who, PeerInfo::new_id(authId));
				 return
			     }
			Some(p) => p,
		};
        p.id=Some(authId);
	}

	fn peer<'a>(&'a self, who: &PeerId) -> Option<&'a PeerInfo<N>> {
		self.inner.get(who)
	}
}

#[derive(Debug, PartialEq)]
pub(super) enum Action<H>  {
	// repropagate under given topic, to the given peers, applying cost/benefit to originator.
	Keep(H, i32),
	// discard and process.
	ProcessAndDiscard(H, i32),
	// discard, applying cost/benefit to originator.
	Discard(i32),
}

/// State of catch up request handling.
#[derive(Debug)]
enum PendingCatchUp {
	/// No pending catch up requests.
	None,
	/// Pending catch up request which has not been answered yet.
	Requesting {
		who: PeerId,
		request: CatchUpRequestMessage,
		instant: Instant,
	},
	/// Pending catch up request that was answered and is being processed.
	Processing {
		instant: Instant,
	},
}


struct PeerReport {
	who: PeerId,
	cost_benefit: i32,
}

// wrapper around a stream of reports.
#[must_use = "The report stream must be consumed"]
pub(super) struct ReportStream {
	reports: mpsc::UnboundedReceiver<PeerReport>,
}

impl ReportStream {
	/// Consume the report stream, converting it into a future that
	/// handles all reports.
	pub(super) fn consume<B, N>(self, net: N)
		-> impl Future<Item=(),Error=()> + Send + 'static
	where
		B: BlockT,
		N: super::Network<B> + Send + 'static,
	{
		ReportingTask {
			reports: self.reports,
			net,
			_marker: Default::default(),
		}
	}
}

/// A future for reporting peers.
#[must_use = "Futures do nothing unless polled"]
struct ReportingTask<B, N> {
	reports: mpsc::UnboundedReceiver<PeerReport>,
	net: N,
	_marker: std::marker::PhantomData<B>,
}

impl<B: BlockT, N: super::Network<B>> Future for ReportingTask<B, N> {
	type Item = ();
	type Error = ();

	fn poll(&mut self) -> Poll<(), ()> {
		loop {
			match self.reports.poll() {
				Err(_) => {
					warn!(target: "afg", "Report stream terminated unexpectedly");
					return Ok(Async::Ready(()))
				}
				Ok(Async::Ready(None)) => return Ok(Async::Ready(())),
				Ok(Async::Ready(Some(PeerReport { who, cost_benefit }))) =>
					self.net.report(who, cost_benefit),
				Ok(Async::NotReady) => return Ok(Async::NotReady),
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use super::environment::SharedVoterSetState;
	use network_gossip::Validator as GossipValidatorT;
	use network::test::Block;

	// some random config (not really needed)
	fn config() -> crate::Config {
		crate::Config {
			gossip_duration: Duration::from_millis(10),
			justification_period: 256,
			local_key: None,
			name: None,
		}
	}

	// dummy voter set state
	fn voter_set_state() -> SharedVoterSetState<Block> {
		use crate::authorities::AuthoritySet;
		use crate::environment::{CompletedRound, CompletedRounds, HasVoted, VoterSetState};
		use grandpa::round::State as RoundState;
		use substrate_primitives::H256;

		let state = RoundState::genesis((H256::zero(), 0));
		let base = state.prevote_ghost.unwrap();
		let voters = AuthoritySet::genesis(Vec::new());
		let set_state = VoterSetState::Live {
			completed_rounds: CompletedRounds::new(
				CompletedRound {
					state,
					number: 0,
					votes: Vec::new(),
					base,
				},
				0,
				&voters,
			),
			current_round: HasVoted::No,
		};

		set_state.into()
	}

	#[test]
	fn view_vote_rules() {
		let view = View { round: Round(100), set_id: SetId(1), last_commit: Some(1000u64) };

		assert_eq!(view.consider_vote(Round(98), SetId(1)), Consider::RejectPast);
		assert_eq!(view.consider_vote(Round(1), SetId(0)), Consider::RejectPast);
		assert_eq!(view.consider_vote(Round(1000), SetId(0)), Consider::RejectPast);

		assert_eq!(view.consider_vote(Round(99), SetId(1)), Consider::Accept);
		assert_eq!(view.consider_vote(Round(100), SetId(1)), Consider::Accept);
		assert_eq!(view.consider_vote(Round(101), SetId(1)), Consider::Accept);

		assert_eq!(view.consider_vote(Round(102), SetId(1)), Consider::RejectFuture);
		assert_eq!(view.consider_vote(Round(1), SetId(2)), Consider::RejectFuture);
		assert_eq!(view.consider_vote(Round(1000), SetId(2)), Consider::RejectFuture);
	}

	#[test]
	fn view_global_message_rules() {
		let view = View { round: Round(100), set_id: SetId(2), last_commit: Some(1000u64) };

		assert_eq!(view.consider_global(SetId(3), 1), Consider::RejectFuture);
		assert_eq!(view.consider_global(SetId(3), 1000), Consider::RejectFuture);
		assert_eq!(view.consider_global(SetId(3), 10000), Consider::RejectFuture);

		assert_eq!(view.consider_global(SetId(1), 1), Consider::RejectPast);
		assert_eq!(view.consider_global(SetId(1), 1000), Consider::RejectPast);
		assert_eq!(view.consider_global(SetId(1), 10000), Consider::RejectPast);

		assert_eq!(view.consider_global(SetId(2), 1), Consider::RejectPast);
		assert_eq!(view.consider_global(SetId(2), 1000), Consider::RejectPast);
		assert_eq!(view.consider_global(SetId(2), 1001), Consider::Accept);
		assert_eq!(view.consider_global(SetId(2), 10000), Consider::Accept);
	}

	#[test]
	fn unknown_peer_cannot_be_updated() {
		let mut peers = Peers::default();
		let id = PeerId::random();

		let update = NeighborPacket {
			round: Round(5),
			set_id: SetId(10),
			commit_finalized_height: 50,
		};

		let res = peers.update_peer_state(&id, update.clone());
		assert!(res.unwrap().is_none());

		// connect & disconnect.
		peers.new_peer(id.clone());
		peers.peer_disconnected(&id);

		let res = peers.update_peer_state(&id, update.clone());
		assert!(res.unwrap().is_none());
	}

	#[test]
	fn update_peer_state() {
		let update1 = NeighborPacket {
			round: Round(5),
			set_id: SetId(10),
			commit_finalized_height: 50u32,
		};

		let update2 = NeighborPacket {
			round: Round(6),
			set_id: SetId(10),
			commit_finalized_height: 60,
		};

		let update3 = NeighborPacket {
			round: Round(2),
			set_id: SetId(11),
			commit_finalized_height: 61,
		};

		let update4 = NeighborPacket {
			round: Round(3),
			set_id: SetId(11),
			commit_finalized_height: 80,
		};

		let mut peers = Peers::default();
		let id = PeerId::random();

		peers.new_peer(id.clone());

		let mut check_update = move |update: NeighborPacket<_>| {
			let view = peers.update_peer_state(&id, update.clone()).unwrap().unwrap();
			assert_eq!(view.round, update.round);
			assert_eq!(view.set_id, update.set_id);
			assert_eq!(view.last_commit, Some(update.commit_finalized_height));
		};

		check_update(update1);
		check_update(update2);
		check_update(update3);
		check_update(update4);
	}

	#[test]
	fn invalid_view_change() {
		let mut peers = Peers::default();

		let id = PeerId::random();
		peers.new_peer(id.clone());

		peers.update_peer_state(&id, NeighborPacket {
			round: Round(10),
			set_id: SetId(10),
			commit_finalized_height: 10,
		}).unwrap().unwrap();

		let mut check_update = move |update: NeighborPacket<_>| {
			let err = peers.update_peer_state(&id, update.clone()).unwrap_err();
			assert_eq!(err, Misbehavior::InvalidViewChange);
		};

		// round moves backwards.
		check_update(NeighborPacket {
			round: Round(9),
			set_id: SetId(10),
			commit_finalized_height: 10,
		});
		// commit finalized height moves backwards.
		check_update(NeighborPacket {
			round: Round(10),
			set_id: SetId(10),
			commit_finalized_height: 9,
		});
		// set ID moves backwards.
		check_update(NeighborPacket {
			round: Round(10),
			set_id: SetId(9),
			commit_finalized_height: 10,
		});
	}

	#[test]
	fn messages_not_expired_immediately() {
		let (val, _) = GossipValidator::<Block>::new(
			config(),
			voter_set_state(),
		);

		let set_id = 1;

		val.note_set(SetId(set_id), Vec::new(), |_, _| {});

		for round_num in 1u64..10 {
			val.note_round(Round(round_num), |_, _| {});
		}

		{
			let mut is_expired = val.message_expired();
			let last_kept_round = 10u64 - KEEP_RECENT_ROUNDS as u64 - 1;

			// messages from old rounds are expired.
			for round_num in 1u64..last_kept_round {
				let topic = crate::communication::round_topic::<Block>(round_num, 1);
				assert!(is_expired(topic, &[1, 2, 3]));
			}

			// messages from not-too-old rounds are not expired.
			for round_num in last_kept_round..10 {
				let topic = crate::communication::round_topic::<Block>(round_num, 1);
				assert!(!is_expired(topic, &[1, 2, 3]));
			}
		}
	}

	#[test]
	fn message_from_unknown_authority_discarded() {
		assert!(cost::UNKNOWN_VOTER != cost::BAD_SIGNATURE);

		let (val, _) = GossipValidator::<Block>::new(
			config(),
			voter_set_state(),
		);
		let set_id = 1;
		let auth = AuthorityId::from_raw([1u8; 32]);
		let peer = PeerId::random();

		val.note_set(SetId(set_id), vec![auth.clone()], |_, _| {});
		val.note_round(Round(0), |_, _| {});

		let inner = val.inner.read();
		let unknown_voter = inner.validate_round_message(&peer, &VoteOrPrecommitMessage {
			round: Round(0),
			set_id: SetId(set_id),
			message: SignedMessage::<Block> {
				message: grandpa::Message::Prevote(grandpa::Prevote {
					target_hash: Default::default(),
					target_number: 10,
				}),
				signature: Default::default(),
				id: AuthorityId::from_raw([2u8; 32]),
			}
		});

		let bad_sig = inner.validate_round_message(&peer, &VoteOrPrecommitMessage {
			round: Round(0),
			set_id: SetId(set_id),
			message: SignedMessage::<Block> {
				message: grandpa::Message::Prevote(grandpa::Prevote {
					target_hash: Default::default(),
					target_number: 10,
				}),
				signature: Default::default(),
				id: auth.clone(),
			}
		});

		assert_eq!(unknown_voter, Action::Discard(cost::UNKNOWN_VOTER));
		assert_eq!(bad_sig, Action::Discard(cost::BAD_SIGNATURE));
	}

	#[test]
	fn unsolicited_catch_up_messages_discarded() {
		let (val, _) = GossipValidator::<Block>::new(
			config(),
			voter_set_state(),
		);

		let set_id = 1;
		let auth = AuthorityId::from_raw([1u8; 32]);
		let peer = PeerId::random();

		val.note_set(SetId(set_id), vec![auth.clone()], |_, _| {});
		val.note_round(Round(0), |_, _| {});

		let validate_catch_up = || {
			let mut inner = val.inner.write();
			inner.validate_catch_up_message(&peer, &FullCatchUpMessage {
				set_id: SetId(set_id),
				message: grandpa::CatchUp {
					round_number: 10,
					prevotes: Default::default(),
					precommits: Default::default(),
					base_hash: Default::default(),
					base_number: Default::default(),
				}
			})
		};

		// the catch up is discarded because we have no pending request
		assert_eq!(validate_catch_up(), Action::Discard(cost::OUT_OF_SCOPE_MESSAGE));

		let noted = val.inner.write().note_catch_up_request(
			&peer,
			&CatchUpRequestMessage {
				set_id: SetId(set_id),
				round: Round(10),
			}
		);

		assert!(noted.0);

		// catch up is allowed because we have requested it, but it's rejected
		// because it's malformed (empty prevotes and precommits)
		assert_eq!(validate_catch_up(), Action::Discard(cost::MALFORMED_CATCH_UP));
	}

	#[test]
	fn unanswerable_catch_up_requests_discarded() {
		// create voter set state with round 1 completed
		let set_state: SharedVoterSetState<Block> = {
			let mut completed_rounds = voter_set_state().read().completed_rounds();

			assert!(completed_rounds.push(environment::CompletedRound {
				number: 1,
				state: grandpa::round::State::genesis(Default::default()),
				base: Default::default(),
				votes: Default::default(),
			}));

			let set_state = environment::VoterSetState::<Block>::Live {
				completed_rounds,
				current_round: environment::HasVoted::No,
			};

			set_state.into()
		};

		let (val, _) = GossipValidator::<Block>::new(
			config(),
			set_state.clone(),
		);

		let set_id = 1;
		let auth = AuthorityId::from_raw([1u8; 32]);
		let peer = PeerId::random();

		val.note_set(SetId(set_id), vec![auth.clone()], |_, _| {});
		val.note_round(Round(2), |_, _| {});

		// add the peer making the request to the validator,
		// otherwise it is discarded
		let mut inner = val.inner.write();
		inner.peers.new_peer(peer.clone());

		let res = inner.handle_catch_up_request(
			&peer,
			CatchUpRequestMessage {
				set_id: SetId(set_id),
				round: Round(10),
			},
			&set_state,
		);

		// we're at round 2, a catch up request for round 10 is out of scope
		assert!(res.0.is_none());
		assert_eq!(res.1, Action::Discard(cost::OUT_OF_SCOPE_MESSAGE));

		let res = inner.handle_catch_up_request(
			&peer,
			CatchUpRequestMessage {
				set_id: SetId(set_id),
				round: Round(1),
			},
			&set_state,
		);

		// a catch up request for round 1 should be answered successfully
		match res.0.unwrap() {
			GossipMessage::CatchUp(catch_up) => {
				assert_eq!(catch_up.set_id, SetId(set_id));
				assert_eq!(catch_up.message.round_number, 1);

				assert_eq!(res.1, Action::Discard(cost::CATCH_UP_REPLY));
			},
			_ => panic!("expected catch up message"),
		};
	}
}

use futures::prelude::*;
use log::{debug, info, warn};
use futures::sync::mpsc;
use client::{BlockchainEvents, CallExecutor, Client, backend::Backend, error::Error as ClientError};
use client::blockchain::HeaderBackend;
use parity_codec::Encode;
use runtime_primitives::traits::{
	NumberFor, Block as BlockT, DigestFor, ProvideRuntimeApi,
};
use fg_primitives::HbbftApi;
use fg_primitives::{SecretKeyShareWrap,SecretKeyWrap,PublickeySetWrap};

use inherents::InherentDataProviders;
use runtime_primitives::generic::BlockId;
use consensus_common::SelectChain;
use substrate_primitives::{ed25519, H256, Pair, Blake2Hasher};
use substrate_telemetry::{telemetry, CONSENSUS_INFO, CONSENSUS_DEBUG, CONSENSUS_WARN};
use serde_json;

use srml_finality_tracker;


use std::fmt;
use std::sync::Arc;
use std::time::Duration;

mod authorities;
mod aux_schema;
mod communication;
mod consensus_changes;
mod environment;
mod finality_proof;
mod import;
mod justification;
mod light_import;
mod observer;
mod until_imported;

#[cfg(feature="service-integration")]
mod service_integration;
#[cfg(feature="service-integration")]
pub use service_integration::{LinkHalfForService, BlockImportForService, BlockImportForLightService};
pub use communication::Network;
pub use finality_proof::FinalityProofProvider;
pub use light_import::light_block_import;
pub use observer::run_grandpa_observer;

use aux_schema::PersistentData;
use environment::{CompletedRound, CompletedRounds, Environment, HasVoted, SharedVoterSetState, VoterSetState};
use import::GrandpaBlockImport;
use until_imported::UntilGlobalMessageBlocksImported;
use communication::NetworkBridge;
use service::TelemetryOnConnect;
use fg_primitives::AuthoritySignature;

// Re-export these two because it's just so damn convenient.
pub use fg_primitives::{AuthorityId, AuthorityPair,ScheduledChange};

#[cfg(test)]
mod tests;

pub enum Message<H, N> {
	/// A prevote message.
	#[cfg_attr(feature = "derive-codec", codec(index = "0"))]
	Prevote(Prevote<H, N>),
	/// A precommit message.
	#[cfg_attr(feature = "derive-codec", codec(index = "1"))]
	Precommit(Precommit<H, N>),
	// Primary proposed block.
	#[cfg_attr(feature = "derive-codec", codec(index = "2"))]
	PrimaryPropose(PrimaryPropose<H, N>),
}


/// A GRANDPA message for a substrate chain.
pub type Message<Block> = grandpa::Message<<Block as BlockT>::Hash, NumberFor<Block>>;
/// A signed message.
pub type SignedMessage<Block> = grandpa::SignedMessage<
	<Block as BlockT>::Hash,
	NumberFor<Block>,
	AuthoritySignature,
	AuthorityId,
>;


/// A catch up message for this chain's block type.
pub type CatchUp<Block> = grandpa::CatchUp<
	<Block as BlockT>::Hash,
	NumberFor<Block>,
	AuthoritySignature,
	AuthorityId,
>;


/// Global communication input stream for commits and catch up messages, with
/// the hash type not being derived from the block, useful for forcing the hash
/// to some type (e.g. `H256`) when the compiler can't do the inference.
type CommunicationInH<Block, H> = grandpa::voter::CommunicationIn<
	H,
	NumberFor<Block>,
	AuthoritySignature,
	AuthorityId,
>;

/// A global communication sink for commits. Not exposed publicly, used
/// internally to simplify types in the communication layer.
type CommunicationOut<Block> = grandpa::voter::CommunicationOut<
	<Block as BlockT>::Hash,
	NumberFor<Block>,
	AuthoritySignature,
	AuthorityId,
>;

/// Global communication sink for commits with the hash type not being derived
/// from the block, useful for forcing the hash to some type (e.g. `H256`) when
/// the compiler can't do the inference.
type CommunicationOutH<Block, H> = grandpa::voter::CommunicationOut<
	H,
	NumberFor<Block>,
	AuthoritySignature,
	AuthorityId,
>;

/// Configuration for the Badger service.
#[derive(Clone)]
pub struct Config {
	/// The local signing key.
	pub local_key: Option<Arc<ed25519::Pair>>,
	/// Some local identifier of the node.
	pub name: Option<String>,
	pub num_validators: usize,
	pub secret_key_share: Option<Arc<SecretKeyShareWrap>>,
	pub node_id: Arc<AuthorityPair>,
	pub public_key_set: Arc<PublickeySetWrap>,
    pub initial_validators:  BTreeMap<PeerId, AuthorityId>,
	pub batch_size:u32,

}

impl Config {
	fn name(&self) -> &str {
		self.name.as_ref().map(|s| s.as_str()).unwrap_or("<unknown>")
	}
	pub fn from_json_file_with_name(path: PathBuf, name: &str) -> Result<Self, String> {
		let file = File::open(&path).map_err(|e| format!("Error opening config file: {}", e))?;
		let spec = json::from_reader(file).map_err(|e| format!("Error parsing spec file: {}", e))?;
		let nodedata= match spec["nodes"]
		   {
           Object(map) =>
		    {
		      match map.get(name)
			  {
				  Some(dat) =>dat,
				  None => return Err("Could not find node name"),
			  }
      
		    }
		    _: return Err("Nodes object should be present"),
		   }  

        let ret = Config
		{
         name: Some(name.clone());
         num_validators: match spec["num_validators"]
		   {
			   Number(x) => x as usize,
               String(st) => st.parse::<usize>()?;
			   _ => return Err("Invalid num_validators");
		   }
		 secret_key_share: match nodedata["secret_key_share"]
		            {
                      String(st) => {
						  let data=hex::decode(st)?
						  match bincode::deserialize(&data)
                              {
 							 Ok(val) => Some(SecretKeyShareWrap { 0: val}),
	                          Err(_)  => return Err("secret key share binary invalid")
                              }
						   },
					  _ => return Err("secret key share not string"),
					}

		}
		//todo: finish parsing json? Peerid? 
		ret
	}
}

/// Errors that can occur while voting in GRANDPA.
#[derive(Debug)]
pub enum Error {
	/// An error within grandpa.
	Badger(GrandpaError),
	/// A network error.
	Network(String),
	/// A blockchain error.
	Blockchain(String),
	/// Could not complete a round on disk.
	Client(ClientError),
	/// An invariant has been violated (e.g. not finalizing pending change blocks in-order)
	Safety(String),
	/// A timer failed to fire.
	Timer(tokio_timer::Error),
}

impl From<GrandpaError> for Error {
	fn from(e: GrandpaError) -> Self {
		Error::Grandpa(e)
	}
}

impl From<ClientError> for Error {
	fn from(e: ClientError) -> Self {
		Error::Client(e)
	}
}

/// Something which can determine if a block is known.
pub trait BlockStatus<Block: BlockT> {
	/// Return `Ok(Some(number))` or `Ok(None)` depending on whether the block
	/// is definitely known and has been imported.
	/// If an unexpected error occurs, return that.
	fn block_number(&self, hash: Block::Hash) -> Result<Option<NumberFor<Block>>, Error>;
}

impl<B, E, Block: BlockT<Hash=H256>, RA> BlockStatus<Block> for Arc<Client<B, E, Block, RA>> where
	B: Backend<Block, Blake2Hasher>,
	E: CallExecutor<Block, Blake2Hasher> + Send + Sync,
	RA: Send + Sync,
	NumberFor<Block>: BlockNumberOps,
{
	fn block_number(&self, hash: Block::Hash) -> Result<Option<NumberFor<Block>>, Error> {
		self.block_number_from_id(&BlockId::Hash(hash))
			.map_err(|e| Error::Blockchain(format!("{:?}", e)))
	}
}

/// A new authority set along with the canonical block it changed at.
#[derive(Debug)]
pub(crate) struct NewAuthoritySet<H, N> {
	pub(crate) canon_number: N,
	pub(crate) canon_hash: H,
	pub(crate) set_id: u64,
	pub(crate) authorities: Vec<(AuthorityId, u64)>,
}

/// Commands issued to the voter.
#[derive(Debug)]
pub(crate) enum VoterCommand<H, N> {
	/// Pause the voter for given reason.
	Pause(String),
	/// New authorities.
	ChangeAuthorities(NewAuthoritySet<H, N>)
}

impl<H, N> fmt::Display for VoterCommand<H, N> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		match *self {
			VoterCommand::Pause(ref reason) => write!(f, "Pausing voter: {}", reason),
			VoterCommand::ChangeAuthorities(_) => write!(f, "Changing authorities"),
		}
	}
}

/// Signals either an early exit of a voter or an error.
#[derive(Debug)]
pub(crate) enum CommandOrError<H, N> {
	/// An error occurred.
	Error(Error),
	/// A command to the voter.
	VoterCommand(VoterCommand<H, N>),
}

impl<H, N> From<Error> for CommandOrError<H, N> {
	fn from(e: Error) -> Self {
		CommandOrError::Error(e)
	}
}

impl<H, N> From<ClientError> for CommandOrError<H, N> {
	fn from(e: ClientError) -> Self {
		CommandOrError::Error(Error::Client(e))
	}
}

impl<H, N> From<grandpa::Error> for CommandOrError<H, N> {
	fn from(e: grandpa::Error) -> Self {
		CommandOrError::Error(Error::from(e))
	}
}

impl<H, N> From<VoterCommand<H, N>> for CommandOrError<H, N> {
	fn from(e: VoterCommand<H, N>) -> Self {
		CommandOrError::VoterCommand(e)
	}
}

impl<H: fmt::Debug, N: fmt::Debug> ::std::error::Error for CommandOrError<H, N> { }

impl<H, N> fmt::Display for CommandOrError<H, N> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		match *self {
			CommandOrError::Error(ref e) => write!(f, "{:?}", e),
			CommandOrError::VoterCommand(ref cmd) => write!(f, "{}", cmd),
		}
	}
}

pub struct LinkHalf<B, E, Block: BlockT<Hash=H256>, RA, SC> {
	client: Arc<Client<B, E, Block, RA>>,
	select_chain: SC,
	persistent_data: PersistentData<Block>,
	voter_commands_rx: mpsc::UnboundedReceiver<VoterCommand<Block::Hash, NumberFor<Block>>>,
}

/// Make block importer and link half necessary to tie the background voter
/// to it.
pub fn block_import<B, E, Block: BlockT<Hash=H256>, RA, PRA, SC>(
	client: Arc<Client<B, E, Block, RA>>,
	api: Arc<PRA>,
	select_chain: SC,
) -> Result<(
		GrandpaBlockImport<B, E, Block, RA, PRA, SC>,
		LinkHalf<B, E, Block, RA, SC>
	), ClientError>
where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + 'static + Clone + Send + Sync,
	RA: Send + Sync,
	PRA: ProvideRuntimeApi,
	PRA::Api: HbbftApi<Block>,
	SC: SelectChain<Block>,
{
	use runtime_primitives::traits::Zero;

	let chain_info = client.info();
	let genesis_hash = chain_info.chain.genesis_hash;

	let persistent_data = aux_schema::load_persistent(
		#[allow(deprecated)]
		&**client.backend(),
		genesis_hash,
		<NumberFor<Block>>::zero(),
		|| {
			let genesis_authorities = api.runtime_api()
				.badger_authorities(&BlockId::number(Zero::zero()))?;
			telemetry!(CONSENSUS_DEBUG; "afg.loading_authorities";
				"authorities_len" => ?genesis_authorities.len()
			);
			Ok(genesis_authorities)
		}
	)?;

	let (voter_commands_tx, voter_commands_rx) = mpsc::unbounded();

	Ok((
		BadgerBlockImport::new(
			client.clone(),
			select_chain.clone(),
			persistent_data.authority_set.clone(),
			voter_commands_tx,
			persistent_data.consensus_changes.clone(),
			api,
		),
		LinkHalf {
			client,
			select_chain,
			persistent_data,
			voter_commands_rx,
		},
	))
}

fn global_communication<Block: BlockT<Hash=H256>, B, E, N, RA>(
	client: &Arc<Client<B, E, Block, RA>>,
	network: &NetworkBridge<Block, N>,
) -> (
		impl Stream<Item = D::Output, Error = Error>,
		impl Sink<Item = TransactionSet, Error = Error>,
) where
	B: Backend<Block, Blake2Hasher>,
	E: CallExecutor<Block, Blake2Hasher> + Send + Sync,
	N: Network<Block>,
	RA: Send + Sync,
	NumberFor<Block>: BlockNumberOps,
{

	let is_voter = network.is_validator();

	// verification stream
	let (global_in, global_out) = network.global_communication(
		voters.clone(),
		is_voter,
	);

	// block commit and catch up messages until relevant blocks are imported.
/*let global_in = UntilGlobalMessageBlocksImported::new(
		client.import_notification_stream(),
		client.clone(),
		global_in,
	);*/ //later

	(global_in, global_out)
}



/// Parameters used to run Grandpa.
pub struct BadgerStartParams<B, E, Block: BlockT<Hash=H256>, N, RA, SC, X> {
	/// Configuration for the GRANDPA service.
	pub config: Config,
	/// A link to the block import worker.
	pub link: LinkHalf<B, E, Block, RA, SC>,
	/// The Network instance.
	pub network: N,
	/// The inherent data providers.
	pub inherent_data_providers: InherentDataProviders,
	/// Handle to a future that will resolve on exit.
	pub on_exit: X,
	/// If supplied, can be used to hook on telemetry connection established events.
	pub telemetry_on_connect: Option<TelemetryOnConnect>,
}

pub struct TxStream<A>
{
 pub transaction_pool: Arc<TransactionPool<A>>,
}

impl Stream for TxStream
{
	type Item=TransactionSet;
	fn poll_next(
        self: Pin<&mut Self>, 
        cx: &mut Context
    ) -> Poll<Option<Self::Item>>
	{
     let pending_iterator = self.transaction_pool.ready();
	  let batch=pending_iterator.into_iter().collect();
	  if batch.len()==0
	  {
		  return Poll::Pending;
	  }
	  Poll::Ready(Some(batch))
	}
}

pub struct BadgerProposerWorker<D: ConsensusProtocol, S, N, Block, TF,C>
where
S : Stream<Item = D::Output, Error = Error> ,
TF: Sink<Item = TransactionSet, Error = Error>,
//N : Network<Block> + Send + Sync + 'static,
NumberFor<Block>: BlockNumberOps,
{
 pub block_out: S,
 pub transaction_in: TF,
 pub transaction_pool: Arc<TransactionPool<A>>,
 pub network: N,
 pub client:C,
 pub block_import: Arc<Mutex<I>>,
 pub inherent_data_providers: InherentDataProviders,
}
impl<D,S,N,Block,TF>   BadgerProposerWorker<D,S,N,Block,TF>
{
	pub fn make_sink(&'a mut self) -> SendAll<'a, Self, TxStream> 
	{
		self.transaction_in.send_all(TxStream{transaction_pool:self.transaction_pool.clone()})
	}
	pub fn make_block_spitter(&'a mut self) ->
	{
		Box::pin( self.block_out.for_each(move |batch|
		  {
			let inherent_data = match self.inherent_data_providers.create_inherent_data() {
				Ok(id) => id,
				Err(err) => return Err(()),
			};
			//empty for now?
			let inherent_digests= generic::Digest {
							logs: vec![],
						}
           	let imp_blocks=self.make_import_blocks(batch, inherent_data,inherent_digests);
			  for import_block in imp_blocks.into_iter().drain()
			  { 
			if let Err(e) = self.block_import.lock().import_block(import_block, Default::default()) {
				warn!(target: "badger", "Error with block built on {:?}: {:?}",
						parent_hash, e);
				telemetry!(CONSENSUS_WARN; "mushroom.err_with_block_built_on";
					"hash" => ?parent_hash, "err" => ?e
				);
			}
			  } 
		
		  }
		).map_err(|e| consensus_common::Error::ClientImport(format!("{:?}", e)).into())  )
	
	}


	pub fn make_import_blocks(
		&self,
		batch: &D::Output
		inherent_data: InherentData,
		inherent_digests: DigestFor<Block>,
	) -> Result<Vec<ImportBlock<Block>>, error::Error> {

		 info!("Processing batch with epoch {:?} of {:?} transactions into blocks",batch.epoch,batch.contributions.len())
         let mut ret=Vec::new();
   		let mut chain_head = match client.best_chain() {
				Ok(x) => x,
				Err(e) => {
					warn!(target: "slots", "Unable to author block. \
					no best block header: {:?}", e);
					return Err(())
				}
			};
        let mut parent_hash = chain_head.hash();
		let mut pnumber= chain_head.number();
		let mut parent_id=BlockId::hash(parent_hash);
		let mut block_builder = self.client.new_block_at(&parent_id, inherent_digests)?;

		// We don't check the API versions any further here since the dispatch compatibility
		// check should be enough. 
		// do this only once? 
		for extrinsic in self.client.runtime_api()
			.inherent_extrinsics_with_context(
				&parent_id,
				ExecutionContext::BlockConstruction,
				inherent_data
			)?
		{
			block_builder.push(extrinsic)?;
		}

		// proceed with transactions
		let mut is_first = true;
		let mut skipped = 0;
		let mut unqueue_invalid = Vec::new();
		let mut unqueue_valid = Vec::new();
		let pending_iterator = self.transaction_pool.ready();

		debug!("Attempting to push transactions from the batch.");
		for (nid , pending) in batch.contributions.into_iter() {

			trace!("[{:?}] Pushing to the block.", pending.hash);
			match client::block_builder::BlockBuilder::push(&mut block_builder, *pending.data.clone()) {
				Ok(()) => {
					debug!("[{:?}] Pushed to the block.", pending.hash);
				}
				Err(error::Error::ApplyExtrinsicFailed(ApplyError::FullBlock)) => {
					if is_first {
						debug!("[{:?}] Invalid transaction: FullBlock on empty block", pending.hash);
						unqueue_invalid.push(pending.hash.clone());
					  }  else 
					   {
						debug!("Block is full, proceed with proposing.");
                        let block = block_builder.bake()?;
						info!("Prepared block for proposing at {} [hash: {:?}; parent_hash: {}; extrinsics: [{}]]",
							block.header().number(),
							<Block as BlockT>::Hash::from(block.header().hash()),
							block.header().parent_hash(),
							block.extrinsics()
								.iter()
								.map(|xt| format!("{}", BlakeTwo256::hash_of(xt)))
								.collect::<Vec<_>>()
								.join(", ")
						);
						telemetry!(CONSENSUS_INFO; "prepared_block_for_proposing";
							"number" => ?block.header().number(),
							"hash" => ?<Block as BlockT>::Hash::from(block.header().hash()),
							);
						if Decode::decode(&mut block.encode().as_slice()).as_ref() != Some(&block) 
						 {
    	                	error!("Failed to verify block encoding/decoding");
		                    }

						if let Err(err) = evaluation::evaluate_initial(&block, &parent_hash, pnumber) {
							error!("Failed to evaluate authored block: {:?}", err);
						}
						let (header, body) = block.deconstruct();

						let header_num = header.number().clone();
						let parent_hash = header.parent_hash().clone();

						// sign the pre-sealed hash of the block and then
						// add it to a digest item.
						let header_hash = header.hash();

						let import_block: ImportBlock<B> = ImportBlock {
							origin: BlockOrigin::Own,
							header,
							justification: None,
							post_digests: vec![],
							body: Some(body),
							finalized: true,
							auxiliary: Vec::new(),
							fork_choice: ForkChoiceStrategy::LongestChain,
						};

						info!("Pre-sealed block for proposal at {}. Hash now {:?}, previously {:?}.",
								header_num,
								import_block.post_header().hash(),
								header_hash
						);
						telemetry!(CONSENSUS_INFO; "badger.pre_sealed_block";
							"header_num" => ?header_num,
							"hash_now" => ?import_block.post_header().hash(),
							"hash_previously" => ?header_hash
						);
							parent_hash = import_block.post_header().hash();
							pnumber = import_block.post_header().number();
							parent_id = BlockId::hash(parent_hash);
							// go on to next block
							ret.push(import_block)
							block_builder = self.client.new_block_at(&parent_id, inherent_digests)?;
							is_first=true;
							continue;
					//	break;
					}
				}
				Err(e) => {
					debug!("[{:?}] Invalid transaction: {}", pending.hash, e);
					unqueue_invalid.push(pending.hash.clone());
				}
			}

			is_first = false;
		}

		self.transaction_pool.remove_invalid(&unqueue_invalid);

		Ok(ret)
	}



}
/// Run a GRANDPA voter as a task. Provide configuration and a link to a
/// block import worker that has already been instantiated with `block_import`.
pub fn run_honey_badger<B, E, Block: BlockT<Hash=H256>, N, RA, SC, X, I>(
	client : Arc<C>,
	t_pool: Arc<TransactionPool<A>>,
	config: Config,
    network:N,
    on_exit: X,
	block_import: Arc<Mutex<I>,
	inherent_data_providers: InherentDataProviders,
) -> ::client::error::Result<impl Future<Item=(),Error=()> + Send + 'static> where
	Block::Hash: Ord,
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + Send + Sync + 'static,
	N: Network<Block> + Send + Sync + 'static,
	N::In: Send + 'static,
	SC: SelectChain<Block> + 'static,
	NumberFor<Block>: BlockNumberOps,
	DigestFor<Block>: Encode,
	RA: Send + Sync + 'static,
	X: Future<Item=(),Error=()> + Clone + Send + 'static,
{
	let (network_bridge, network_startup) = NetworkBridge::new(
		network,
		config.clone(),
		on_exit.clone(),
	);

	use futures::future::{self, Loop as FutureLoop};



	let PersistentData { authority_set, set_state, consensus_changes } = persistent_data;



	register_finality_tracker_inherent_data_provider(client.clone(), &inherent_data_providers)?;
	let (blk_out,tx_in) = global_communication(
					&client,
					&network,
				);


    let bworker= BadgerProposerWorker
                {
                 block_out: blk_out,
                 transaction_in: tx_in,
                 transaction_pool: t_pool.clone(),
                 network: network,
                 client: c.clone(),
                 block_import: block_import.clone(),
                 inherent_data_providers: inherent_data_providers,
                };
    let  aggregate=bworker.make_sink().select(bworker.make_block_spitter()).map(|_| ()).map_err(|e| 
	    {
			warn!("BADGER failed: {:?}", e);
			telemetry!(CONSENSUS_WARN; "afg.badger_failed"; "e" => ?e);
		});

	let with_start = network_startup.and_then(move |()| aggregate);

	// Make sure that `telemetry_task` doesn't accidentally finish and kill grandpa.

	Ok(with_start.select(on_exit).then(|_| Ok(())))

}

