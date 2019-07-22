
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


//! Aura (Authority-round) consensus in substrate.
//!
//! Aura works by having a list of authorities A who are expected to roughly
//! agree on the current time. Time is divided up into discrete slots of t
//! seconds each. For each slot s, the author of that slot is A[s % |A|].
//!
//! The author is allowed to issue one block but not more during that slot,
//! and it will be built upon the longest valid chain that has been seen.
//!
//! Blocks from future steps will be either deferred or rejected depending on how
//! far in the future they are.
//!
//! NOTE: Aura itself is designed to be generic over the crypto used.
#![forbid(missing_docs, unsafe_code)]
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

mod digest;

type AuthorityId<P> = <P as Pair>::Public;

/// A slot duration. Create with `get_or_compute`.
#[derive(Clone, Copy, Debug, Encode, Decode, Hash, PartialOrd, Ord, PartialEq, Eq)]
pub struct SlotDuration(slots::SlotDuration<u64>);

impl SlotDuration {
	/// Either fetch the slot duration from disk or compute it from the genesis
	/// state.
	pub fn get_or_compute<A, B, C>(client: &C) -> CResult<Self>
	where
		A: Codec,
		B: Block,
		C: AuxStore + ProvideRuntimeApi,
		C::Api: AuraApi<B, A>,
	{
		slots::SlotDuration::get_or_compute(client, |a, b| a.slot_duration(b)).map(Self)
	}

	/// Get the slot duration in milliseconds.
	pub fn get(&self) -> u64 {
		self.0.get()
	}
}

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

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
struct AuraSlotCompatible;

impl SlotCompatible for AuraSlotCompatible {
	fn extract_timestamp_and_slot(
		&self,
		data: &InherentData
	) -> Result<(TimestampInherent, AuraInherent, std::time::Duration), consensus_common::Error> {
		data.timestamp_inherent_data()
			.and_then(|t| data.aura_inherent_data().map(|a| (t, a)))
			.map_err(Into::into)
			.map_err(consensus_common::Error::InherentData)
			.map(|(x, y)| (x, y, Default::default()))
	}
}

/// Start the aura worker. The returned future should be run in a tokio runtime.
pub fn start_aura<B, C, SC, E, I, P, SO, Error, H>(
	slot_duration: SlotDuration,
	local_key: Arc<P>,
	client: Arc<C>,
	select_chain: SC,
	block_import: I,
	env: Arc<E>,
	sync_oracle: SO,
	inherent_data_providers: InherentDataProviders,
	force_authoring: bool,
) -> Result<impl Future<Item=(), Error=()>, consensus_common::Error> where
	B: Block<Header=H>,
	C: ProvideRuntimeApi + ProvideCache<B> + AuxStore + Send + Sync,
	C::Api: AuraApi<B, AuthorityId<P>>,
	SC: SelectChain<B>,
	E::Proposer: Proposer<B, Error=Error>,
	<<E::Proposer as Proposer<B>>::Create as IntoFuture>::Future: Send + 'static,
	P: Pair + Send + Sync + 'static,
	P::Public: Hash + Member + Encode + Decode,
	P::Signature: Hash + Member + Encode + Decode,
	H: Header<Hash=B::Hash>,
	E: Environment<B, Error=Error>,
	I: BlockImport<B> + Send + Sync + 'static,
	Error: ::std::error::Error + Send + From<::consensus_common::Error> + From<I::Error> + 'static,
	SO: SyncOracle + Send + Sync + Clone,
{
	let worker = AuraWorker {
		client: client.clone(),
		block_import: Arc::new(Mutex::new(block_import)),
		env,
		local_key,
		sync_oracle: sync_oracle.clone(),
		force_authoring,
	};
	register_aura_inherent_data_provider(
		&inherent_data_providers,
		slot_duration.0.slot_duration()
	)?;
	Ok(slots::start_slot_worker::<_, _, _, _, _, AuraSlotCompatible>(
		slot_duration.0,
		select_chain,
		worker,
		sync_oracle,
		inherent_data_providers,
		AuraSlotCompatible,
	))
}

struct AuraWorker<C, E, I, P, SO> {
	client: Arc<C>,
	block_import: Arc<Mutex<I>>,
	env: Arc<E>,
	local_key: Arc<P>,
	sync_oracle: SO,
	force_authoring: bool,
}

impl<H, B, C, E, I, P, Error, SO> SlotWorker<B> for AuraWorker<C, E, I, P, SO> where
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
	let seal = match header.digest_mut().pop() {
		Some(x) => x,
		None => return Err(format!("Header {:?} is unsealed", hash)),
	};

	let sig = seal.as_aura_seal().ok_or_else(|| {
		aura_err!("Header {:?} has a bad seal", hash)
	})?;

	let slot_num = find_pre_digest::<B, _>(&header)?;

	if slot_num > slot_now {
		header.digest_mut().push(seal);
		Ok(CheckedHeader::Deferred(header, slot_num))
	} else {
		// check the signature is valid under the expected authority and
		// chain state.
		let expected_author = match slot_author::<P>(slot_num, &authorities) {
			None => return Err("Slot Author not found".to_string()),
			Some(author) => author,
		};

		let pre_hash = header.hash();

		if P::verify(&sig, pre_hash.as_ref(), expected_author) {
			if let Some(equivocation_proof) = check_equivocation(
				client,
				slot_now,
				slot_num,
				&header,
				expected_author,
			).map_err(|e| e.to_string())? {
				info!(
					"Slot author is equivocating at slot {} with headers {:?} and {:?}",
					slot_num,
					equivocation_proof.fst_header().hash(),
					equivocation_proof.snd_header().hash(),
				);
			}

			Ok(CheckedHeader::Checked(header, (slot_num, seal)))
		} else {
			Err(format!("Bad signature on {:?}", hash))
		}
	}
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
fn register_aura_inherent_data_provider(
	inherent_data_providers: &InherentDataProviders,
	slot_duration: u64,
) -> Result<(), consensus_common::Error> {
	if !inherent_data_providers.has_provider(&srml_aura::INHERENT_IDENTIFIER) {
		inherent_data_providers
			.register_provider(srml_aura::InherentDataProvider::new(slot_duration))
			.map_err(Into::into)
			.map_err(consensus_common::Error::InherentData)
	} else {
		Ok(())
	}
}

/// Start an import queue for the Aura consensus algorithm.
pub fn import_queue<B, C, P>(
	slot_duration: SlotDuration,
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
	register_aura_inherent_data_provider(&inherent_data_providers, slot_duration.get())?;
	initialize_authorities_cache(&*client)?;

	let verifier = Arc::new(
		AuraVerifier {
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

#[cfg(test)]
mod tests {
	use super::*;
	use futures::{Async, stream::Stream as _};
	use consensus_common::NoNetwork as DummyOracle;
	use network::test::*;
	use network::test::{Block as TestBlock, PeersClient, PeersFullClient};
	use runtime_primitives::traits::{Block as BlockT, DigestFor};
	use network::config::ProtocolConfig;
	use parking_lot::Mutex;
	use tokio::runtime::current_thread;
	use keyring::sr25519::Keyring;
	use primitives::sr25519;
	use client::{LongestChain, BlockchainEvents};
	use test_client;

	type Error = client::error::Error;

	type TestClient = client::Client<
		test_client::Backend,
		test_client::Executor,
		TestBlock,
		test_client::runtime::RuntimeApi
	>;

	struct DummyFactory(Arc<TestClient>);
	struct DummyProposer(u64, Arc<TestClient>);

	impl Environment<TestBlock> for DummyFactory {
		type Proposer = DummyProposer;
		type Error = Error;

		fn init(&self, parent_header: &<TestBlock as BlockT>::Header)
			-> Result<DummyProposer, Error>
		{
			Ok(DummyProposer(parent_header.number + 1, self.0.clone()))
		}
	}

	impl Proposer<TestBlock> for DummyProposer {
		type Error = Error;
		type Create = Result<TestBlock, Error>;

		fn propose(
			&self,
			_: InherentData,
			digests: DigestFor<TestBlock>,
			_: Duration,
		) -> Result<TestBlock, Error> {
			self.1.new_block(digests).unwrap().bake().map_err(|e| e.into())
		}
	}

	const SLOT_DURATION: u64 = 1;

	pub struct AuraTestNet {
		peers: Vec<Peer<(), DummySpecialization>>,
	}

	impl TestNetFactory for AuraTestNet {
		type Specialization = DummySpecialization;
		type Verifier = AuraVerifier<PeersFullClient, sr25519::Pair>;
		type PeerData = ();

		/// Create new test network with peers and given config.
		fn from_config(_config: &ProtocolConfig) -> Self {
			AuraTestNet {
				peers: Vec::new(),
			}
		}

		fn make_verifier(&self, client: PeersClient, _cfg: &ProtocolConfig)
			-> Arc<Self::Verifier>
		{
			match client {
				PeersClient::Full(client) => {
					let slot_duration = SlotDuration::get_or_compute(&*client)
						.expect("slot duration available");
					let inherent_data_providers = InherentDataProviders::new();
					register_aura_inherent_data_provider(
						&inherent_data_providers,
						slot_duration.get()
					).expect("Registers aura inherent data provider");

					assert_eq!(slot_duration.get(), SLOT_DURATION);
					Arc::new(AuraVerifier {
						client,
						inherent_data_providers,
						phantom: Default::default(),
					})
				},
				PeersClient::Light(_) => unreachable!("No (yet) tests for light client + Aura"),
			}
		}

		fn peer(&mut self, i: usize) -> &mut Peer<Self::PeerData, DummySpecialization> {
			&mut self.peers[i]
		}

		fn peers(&self) -> &Vec<Peer<Self::PeerData, DummySpecialization>> {
			&self.peers
		}

		fn mut_peers<F: FnOnce(&mut Vec<Peer<Self::PeerData, DummySpecialization>>)>(&mut self, closure: F) {
			closure(&mut self.peers);
		}
	}

	#[test]
	#[allow(deprecated)]
	fn authoring_blocks() {
		let _ = ::env_logger::try_init();
		let net = AuraTestNet::new(3);

		let peers = &[
			(0, Keyring::Alice),
			(1, Keyring::Bob),
			(2, Keyring::Charlie),
		];

		let net = Arc::new(Mutex::new(net));
		let mut import_notifications = Vec::new();

		let mut runtime = current_thread::Runtime::new().unwrap();
		for (peer_id, key) in peers {
			let client = net.lock().peer(*peer_id).client().as_full().expect("full clients are created").clone();
			#[allow(deprecated)]
			let select_chain = LongestChain::new(
				client.backend().clone(),
			);
			let environ = Arc::new(DummyFactory(client.clone()));
			import_notifications.push(
				client.import_notification_stream()
					.take_while(|n| Ok(!(n.origin != BlockOrigin::Own && n.header.number() < &5)))
					.for_each(move |_| Ok(()))
			);

			let slot_duration = SlotDuration::get_or_compute(&*client)
				.expect("slot duration available");

			let inherent_data_providers = InherentDataProviders::new();
			register_aura_inherent_data_provider(
				&inherent_data_providers, slot_duration.get()
			).expect("Registers aura inherent data provider");

			let aura = start_aura::<_, _, _, _, _, sr25519::Pair, _, _, _>(
				slot_duration,
				Arc::new(key.clone().into()),
				client.clone(),
				select_chain,
				client,
				environ.clone(),
				DummyOracle,
				inherent_data_providers,
				false,
			).expect("Starts aura");

			runtime.spawn(aura);
		}

		// wait for all finalized on each.
		let wait_for = ::futures::future::join_all(import_notifications)
			.map(|_| ())
			.map_err(|_| ());

		let drive_to_completion = futures::future::poll_fn(|| { net.lock().poll(); Ok(Async::NotReady) });
		let _ = runtime.block_on(wait_for.select(drive_to_completion).map_err(|_| ())).unwrap();
	}

	#[test]
	fn authorities_call_works() {
		let client = test_client::new();

		assert_eq!(client.info().chain.best_number, 0);
		assert_eq!(authorities(&client, &BlockId::Number(0)).unwrap(), vec![
			Keyring::Alice.into(),
			Keyring::Bob.into(),
			Keyring::Charlie.into()
		]);
	}
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
	/// Grandpa message with round and set info.
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

struct Inner<Block: BlockT> {
	//local_view: Option<View<NumberFor<Block>>>,
	peers: Peers<NumberFor<Block>>,
	live_topics: KeepTopics<Block>,
	authorities: Vec<AuthorityId>,
	config: crate::Config,
	next_rebroadcast: Instant,
	pending_catch_up: PendingCatchUp,
}

type MaybeMessage<Block> = Option<(Vec<PeerId>, NeighborPacket<NumberFor<Block>>)>;

impl<Block: BlockT> Inner<Block> {
	fn new(config: crate::Config) -> Self {
		Inner {
			local_view: None,
			peers: Peers::default(),
			live_topics: KeepTopics::new(),
			next_rebroadcast: Instant::now() + REBROADCAST_AFTER,
			authorities: Vec::new(),
			pending_catch_up: PendingCatchUp::None,
			config,
		}
	}

	/// Note a round in the current set has started.
	fn note_round(&mut self, round: Round) -> MaybeMessage<Block> {
		{
			let local_view = match self.local_view {
				None => return None,
				Some(ref mut v) => if v.round == round {
					return None
				} else {
					v
				},
			};

			let set_id = local_view.set_id;

			debug!(target: "afg", "Voter {} noting beginning of round {:?} to network.",
				self.config.name(), (round,set_id));

			local_view.round = round;

			self.live_topics.push(round, set_id);
		}
		self.multicast_neighbor_packet()
	}

	/// Note that a voter set with given ID has started. Does nothing if the last
	/// call to the function was with the same `set_id`.
	fn note_set(&mut self, set_id: SetId, authorities: Vec<AuthorityId>) -> MaybeMessage<Block> {
		{
			let local_view = match self.local_view {
				ref mut x @ None => x.get_or_insert(View {
					round: Round(0),
					set_id,
					last_commit: None,
				}),
				Some(ref mut v) => if v.set_id == set_id {
					return None
				} else {
					v
				},
			};

			local_view.update_set(set_id);
			self.live_topics.push(Round(0), set_id);
			self.authorities = authorities;
		}
		self.multicast_neighbor_packet()
	}

	/// Note that we've imported a commit finalizing a given block.
	fn note_commit_finalized(&mut self, finalized: NumberFor<Block>) -> MaybeMessage<Block> {
		{
			match self.local_view {
				None => return None,
				Some(ref mut v) => if v.last_commit.as_ref() < Some(&finalized) {
					v.last_commit = Some(finalized);
				} else {
					return None
				},
			};
		}

		self.multicast_neighbor_packet()
	}

	

	fn consider_global(&self, set_id: SetId, number: NumberFor<Block>) -> Consider {
		self.local_view.as_ref().map(|v| v.consider_global(set_id, number))
			.unwrap_or(Consider::RejectOutOfScope)
	}



	fn validate_round_message(&self, who: &PeerId, full: &VoteOrPrecommitMessage<Block>)
		-> Action<Block::Hash>
	{
		match self.consider_vote(full.round, full.set_id) {
			Consider::RejectFuture => return Action::Discard(Misbehavior::FutureMessage.cost()),
			Consider::RejectOutOfScope => return Action::Discard(Misbehavior::OutOfScopeMessage.cost()),
			Consider::RejectPast =>
				return Action::Discard(self.cost_past_rejection(who, full.round, full.set_id)),
			Consider::Accept => {},
		}

		// ensure authority is part of the set.
		if !self.authorities.contains(&full.message.id) {
			telemetry!(CONSENSUS_DEBUG; "afg.bad_msg_signature"; "signature" => ?full.message.id);
			return Action::Discard(cost::UNKNOWN_VOTER);
		}

		if let Err(()) = super::check_message_sig::<Block>(
			&full.message.message,
			&full.message.id,
			&full.message.signature,
			full.round.0,
			full.set_id.0,
		) {
			debug!(target: "afg", "Bad message signature {}", full.message.id);
			telemetry!(CONSENSUS_DEBUG; "afg.bad_msg_signature"; "signature" => ?full.message.id);
			return Action::Discard(cost::BAD_SIGNATURE);
		}

		let topic = super::round_topic::<Block>(full.round.0, full.set_id.0);
		Action::Keep(topic, benefit::ROUND_MESSAGE)
	}

	fn validate_commit_message(&mut self, who: &PeerId, full: &FullCommitMessage<Block>)
		-> Action<Block::Hash>
	{

		if let Err(misbehavior) = self.peers.update_commit_height(who, full.message.target_number) {
			return Action::Discard(misbehavior.cost());
		}

		match self.consider_global(full.set_id, full.message.target_number) {
			Consider::RejectFuture => return Action::Discard(Misbehavior::FutureMessage.cost()),
			Consider::RejectPast =>
				return Action::Discard(self.cost_past_rejection(who, full.round, full.set_id)),
			Consider::RejectOutOfScope => return Action::Discard(Misbehavior::OutOfScopeMessage.cost()),
			Consider::Accept => {},

		}

		if full.message.precommits.len() != full.message.auth_data.len() || full.message.precommits.is_empty() {
			debug!(target: "afg", "Malformed compact commit");
			telemetry!(CONSENSUS_DEBUG; "afg.malformed_compact_commit";
				"precommits_len" => ?full.message.precommits.len(),
				"auth_data_len" => ?full.message.auth_data.len(),
				"precommits_is_empty" => ?full.message.precommits.is_empty(),
			);
			return Action::Discard(cost::MALFORMED_COMMIT);
		}

		// always discard commits initially and rebroadcast after doing full
		// checking.
		let topic = super::global_topic::<Block>(full.set_id.0);
		Action::ProcessAndDiscard(topic, benefit::BASIC_VALIDATED_COMMIT)
	}

	fn validate_catch_up_message(&mut self, who: &PeerId, full: &FullCatchUpMessage<Block>)
		-> Action<Block::Hash>
	{
		match &self.pending_catch_up {
			PendingCatchUp::Requesting { who: peer, request, instant } => {
				if peer != who {
					return Action::Discard(Misbehavior::OutOfScopeMessage.cost());
				}

				if request.set_id != full.set_id {
					return Action::Discard(cost::MALFORMED_CATCH_UP);
				}

				if request.round.0 > full.message.round_number {
					return Action::Discard(cost::MALFORMED_CATCH_UP);
				}

				if full.message.prevotes.is_empty() || full.message.precommits.is_empty() {
					return Action::Discard(cost::MALFORMED_CATCH_UP);
				}

				// move request to pending processing state, we won't push out
				// any catch up requests until we import this one (either with a
				// success or failure).
				self.pending_catch_up = PendingCatchUp::Processing {
					instant: instant.clone(),
				};

				// always discard catch up messages, they're point-to-point
				let topic = super::global_topic::<Block>(full.set_id.0);
				Action::ProcessAndDiscard(topic, benefit::BASIC_VALIDATED_CATCH_UP)
			},
			_ => Action::Discard(Misbehavior::OutOfScopeMessage.cost()),
		}
	}

	fn note_catch_up_message_processed(&mut self) {
		match &self.pending_catch_up {
			PendingCatchUp::Processing { .. } => {
				self.pending_catch_up = PendingCatchUp::None;
			},
			state => trace!(target: "afg",
				"Noted processed catch up message when state was: {:?}",
				state,
			),
		}
	}

	fn handle_catch_up_request(
		&mut self,
		who: &PeerId,
		request: CatchUpRequestMessage,
		set_state: &environment::SharedVoterSetState<Block>,
	) -> (Option<GossipMessage<Block>>, Action<Block::Hash>) {
		let local_view = match self.local_view {
			None => return (None, Action::Discard(Misbehavior::OutOfScopeMessage.cost())),
			Some(ref view) => view,
		};

		if request.set_id != local_view.set_id {
			// NOTE: When we're close to a set change there is potentially a
			// race where the peer sent us the request before it observed that
			// we had transitioned to a new set. In this case we charge a lower
			// cost.
			if local_view.round.0.saturating_sub(CATCH_UP_THRESHOLD) == 0 {
				return (None, Action::Discard(cost::HONEST_OUT_OF_SCOPE_CATCH_UP));
			}

			return (None, Action::Discard(Misbehavior::OutOfScopeMessage.cost()));
		}

		match self.peers.peer(who) {
			None =>
				return (None, Action::Discard(Misbehavior::OutOfScopeMessage.cost())),
			Some(peer) if peer.view.round >= request.round =>
				return (None, Action::Discard(Misbehavior::OutOfScopeMessage.cost())),
			_ => {},
		}

		let last_completed_round = set_state.read().last_completed_round();
		if last_completed_round.number < request.round.0 {
			return (None, Action::Discard(Misbehavior::OutOfScopeMessage.cost()));
		}

		trace!(target: "afg", "Replying to catch-up request for round {} from {} with round {}",
			request.round.0,
			who,
			last_completed_round.number,
		);

		let mut prevotes = Vec::new();
		let mut precommits = Vec::new();

		// NOTE: the set of votes stored in `LastCompletedRound` is a minimal
		// set of votes, i.e. at most one equivocation is stored per voter. The
		// code below assumes this invariant is maintained when creating the
		// catch up reply since peers won't accept catch-up messages that have
		// too many equivocations (we exceed the fault-tolerance bound).
		for vote in last_completed_round.votes {
			match vote.message {
				grandpa::Message::Prevote(prevote) => {
					prevotes.push(grandpa::SignedPrevote {
						prevote,
						signature: vote.signature,
						id: vote.id,
					});
				},
				grandpa::Message::Precommit(precommit) => {
					precommits.push(grandpa::SignedPrecommit {
						precommit,
						signature: vote.signature,
						id: vote.id,
					});
				},
				_ => {},
			}
		}

		let (base_hash, base_number) = last_completed_round.base;

		let catch_up = CatchUp::<Block> {
			round_number: last_completed_round.number,
			prevotes,
			precommits,
			base_hash,
			base_number,
		};

		let full_catch_up = GossipMessage::CatchUp::<Block>(FullCatchUpMessage {
			set_id: request.set_id,
			message: catch_up,
		});

		(Some(full_catch_up), Action::Discard(cost::CATCH_UP_REPLY))
	}

	fn try_catch_up(&mut self, who: &PeerId) -> (Option<GossipMessage<Block>>, Option<Report>) {
		let mut catch_up = None;
		let mut report = None;

		// if the peer is on the same set and ahead of us by a margin bigger
		// than `CATCH_UP_THRESHOLD` then we should ask it for a catch up
		// message.
		if let (Some(peer), Some(local_view)) = (self.peers.peer(who), &self.local_view) {
			if peer.view.set_id == local_view.set_id &&
				peer.view.round.0.saturating_sub(CATCH_UP_THRESHOLD) > local_view.round.0
			{
				// send catch up request if allowed
				let round = peer.view.round.0 - 1; // peer.view.round is > 0
				let request = CatchUpRequestMessage {
					set_id: peer.view.set_id,
					round: Round(round),
				};

				let (catch_up_allowed, catch_up_report) = self.note_catch_up_request(who, &request);

				if catch_up_allowed {
					trace!(target: "afg", "Sending catch-up request for round {} to {}",
						   round,
						   who,
					);

					catch_up = Some(GossipMessage::<Block>::CatchUpRequest(request));
				}

				report = catch_up_report;
			}
		}

		(catch_up, report)
	}

	fn import_neighbor_message(&mut self, who: &PeerId, update: NeighborPacket<NumberFor<Block>>)
		-> (Vec<Block::Hash>, Action<Block::Hash>, Option<GossipMessage<Block>>, Option<Report>)
	{
		let update_res = self.peers.update_peer_state(who, update);

		let (cost_benefit, topics) = match update_res {
			Ok(view) =>
				(benefit::NEIGHBOR_MESSAGE, view.map(|view| neighbor_topics::<Block>(view))),
			Err(misbehavior) =>
				(misbehavior.cost(), None),
		};

		let (catch_up, report) = match update_res {
			Ok(_) => self.try_catch_up(who),
			_ => (None, None),
		};

		let neighbor_topics = topics.unwrap_or_default();

		// always discard neighbor messages, it's only valid for one hop.
		let action = Action::Discard(cost_benefit);

		(neighbor_topics, action, catch_up, report)
	}

	fn multicast_neighbor_packet(&self) -> MaybeMessage<Block> {
		self.local_view.as_ref().map(|local_view| {
			let packet = NeighborPacket {
				round: local_view.round,
				set_id: local_view.set_id,
				commit_finalized_height: local_view.last_commit.unwrap_or(Zero::zero()),
			};

			let peers = self.peers.inner.keys().cloned().collect();
			(peers, packet)
		})
	}

	fn note_catch_up_request(
		&mut self,
		who: &PeerId,
		catch_up_request: &CatchUpRequestMessage,
	) -> (bool, Option<Report>) {
		let report = match &self.pending_catch_up {
			PendingCatchUp::Requesting { who: peer, instant, .. } =>
				if instant.elapsed() <= CATCH_UP_REQUEST_TIMEOUT {
					return (false, None);
				} else {
					// report peer for timeout
					Some((peer.clone(), cost::CATCH_UP_REQUEST_TIMEOUT))
				},
			PendingCatchUp::Processing { instant, .. } =>
				if instant.elapsed() < CATCH_UP_PROCESS_TIMEOUT {
					return (false, None);
				} else {
					None
				},
			_ => None,
		};

		self.pending_catch_up = PendingCatchUp::Requesting {
			who: who.clone(),
			request: catch_up_request.clone(),
			instant: Instant::now(),
		};

		(true, report)
	}
}

/// A validator for GRANDPA gossip messages.
pub(super) struct GossipValidator<Block: BlockT> {
	inner: parking_lot::RwLock<Inner<Block>>,
	set_state: environment::SharedVoterSetState<Block>,
	report_sender: mpsc::UnboundedSender<PeerReport>,
}

impl<Block: BlockT> GossipValidator<Block> {
	/// Create a new gossip-validator. This initialized the current set to 0.
	pub(super) fn new(config: crate::Config, set_state: environment::SharedVoterSetState<Block>)
		-> (GossipValidator<Block>, ReportStream)
	{
		let (tx, rx) = mpsc::unbounded();
		let val = GossipValidator {
			inner: parking_lot::RwLock::new(Inner::new(config)),
			set_state,
			report_sender: tx,
		};

		(val, ReportStream { reports: rx })
	}

	/// Note a round in the current set has started.
	pub(super) fn note_round<F>(&self, round: Round, send_neighbor: F)
		where F: FnOnce(Vec<PeerId>, NeighborPacket<NumberFor<Block>>)
	{
		let maybe_msg = self.inner.write().note_round(round);
		if let Some((to, msg)) = maybe_msg {
			send_neighbor(to, msg);
		}
	}

	/// Note that a voter set with given ID has started. Updates the current set to given
	/// value and initializes the round to 0.
	pub(super) fn note_set<F>(&self, set_id: SetId, authorities: Vec<AuthorityId>, send_neighbor: F)
		where F: FnOnce(Vec<PeerId>, NeighborPacket<NumberFor<Block>>)
	{
		let maybe_msg = self.inner.write().note_set(set_id, authorities);
		if let Some((to, msg)) = maybe_msg {
			send_neighbor(to, msg);
		}
	}

	/// Note that we've imported a commit finalizing a given block.
	pub(super) fn note_commit_finalized<F>(&self, finalized: NumberFor<Block>, send_neighbor: F)
		where F: FnOnce(Vec<PeerId>, NeighborPacket<NumberFor<Block>>)
	{
		let maybe_msg = self.inner.write().note_commit_finalized(finalized);
		if let Some((to, msg)) = maybe_msg {
			send_neighbor(to, msg);
		}
	}

	/// Note that we've processed a catch up message.
	pub(super) fn note_catch_up_message_processed(&self)	{
		self.inner.write().note_catch_up_message_processed();
	}

	fn report(&self, who: PeerId, cost_benefit: i32) {
		let _ = self.report_sender.unbounded_send(PeerReport { who, cost_benefit });
	}

	pub(super) fn do_validate(&self, who: &PeerId, mut data: &[u8])
		-> (Action<Block::Hash>, Vec<Block::Hash>, Option<GossipMessage<Block>>)
	{
		let mut broadcast_topics = Vec::new();
		let mut peer_reply = None;

		let action = {
			match GossipMessage::<Block>::decode(&mut data) {
				Some(GossipMessage::VoteOrPrecommit(ref message))
					=> self.inner.write().validate_round_message(who, message),
				Some(GossipMessage::Commit(ref message)) => self.inner.write().validate_commit_message(who, message),
				Some(GossipMessage::Neighbor(update)) => {
					let (topics, action, catch_up, report) = self.inner.write().import_neighbor_message(
						who,
						update.into_neighbor_packet(),
					);

					if let Some((peer, cost_benefit)) = report {
						self.report(peer, cost_benefit);
					}

					broadcast_topics = topics;
					peer_reply = catch_up;
					action
				}
				Some(GossipMessage::CatchUp(ref message))
					=> self.inner.write().validate_catch_up_message(who, message),
				Some(GossipMessage::CatchUpRequest(request)) => {
					let (reply, action) = self.inner.write().handle_catch_up_request(
						who,
						request,
						&self.set_state,
					);

					peer_reply = reply;
					action
				}
				None => {
					debug!(target: "afg", "Error decoding message");
					telemetry!(CONSENSUS_DEBUG; "afg.err_decoding_msg"; "" => "");

					let len = std::cmp::min(i32::max_value() as usize, data.len()) as i32;
					Action::Discard(Misbehavior::UndecodablePacket(len).cost())
				}
			}
		};

		(action, broadcast_topics, peer_reply)
	}
}

impl<Block: BlockT> network_gossip::Validator<Block> for GossipValidator<Block> {
	fn new_peer(&self, context: &mut dyn ValidatorContext<Block>, who: &PeerId, _roles: Roles) {
		let packet = {
			let mut inner = self.inner.write();
			inner.peers.new_peer(who.clone());

			inner.local_view.as_ref().map(|v| {
				NeighborPacket {
					round: v.round,
					set_id: v.set_id,
					commit_finalized_height: v.last_commit.unwrap_or(Zero::zero()),
				}
			})
		};

		if let Some(packet) = packet {
			let packet_data = GossipMessage::<Block>::from(packet).encode();
			context.send_message(who, packet_data);
		}
	}

	fn peer_disconnected(&self, _context: &mut dyn ValidatorContext<Block>, who: &PeerId) {
		self.inner.write().peers.peer_disconnected(who);
	}

	fn validate(&self, context: &mut dyn ValidatorContext<Block>, who: &PeerId, data: &[u8])
		-> network_gossip::ValidationResult<Block::Hash>
	{
		let (action, broadcast_topics, peer_reply) = self.do_validate(who, data);

		// not with lock held!
		if let Some(msg) = peer_reply {
			context.send_message(who, msg.encode());
		}

		for topic in broadcast_topics {
			context.send_topic(who, topic, false);
		}

		match action {
			Action::Keep(topic, cb) => {
				self.report(who.clone(), cb);
				context.broadcast_message(topic, data.to_vec(), false);
				network_gossip::ValidationResult::ProcessAndKeep(topic)
			}
			Action::ProcessAndDiscard(topic, cb) => {
				self.report(who.clone(), cb);
				network_gossip::ValidationResult::ProcessAndDiscard(topic)
			}
			Action::Discard(cb) => {
				self.report(who.clone(), cb);
				network_gossip::ValidationResult::Discard
			}
		}
	}

	fn message_allowed<'a>(&'a self)
		-> Box<dyn FnMut(&PeerId, MessageIntent, &Block::Hash, &[u8]) -> bool + 'a>
	{
		let (inner, do_rebroadcast) = {
			use parking_lot::RwLockWriteGuard;

			let mut inner = self.inner.write();
			let now = Instant::now();
			let do_rebroadcast = if now >= inner.next_rebroadcast {
				inner.next_rebroadcast = now + REBROADCAST_AFTER;
				true
			} else {
				false
			};

			// downgrade to read-lock.
			(RwLockWriteGuard::downgrade(inner), do_rebroadcast)
		};

		Box::new(move |who, intent, topic, mut data| {
			if let MessageIntent::PeriodicRebroadcast = intent {
				return do_rebroadcast;
			}

			let peer = match inner.peers.peer(who) {
				None => return false,
				Some(x) => x,
			};

			// if the topic is not something we're keeping at the moment,
			// do not send.
			let (maybe_round, set_id) = match inner.live_topics.topic_info(&topic) {
				None => return false,
				Some(x) => x,
			};

			// if the topic is not something the peer accepts, discard.
			if let Some(round) = maybe_round {
				return peer.view.consider_vote(round, set_id) == Consider::Accept
			}

			// global message.
			let local_view = match inner.local_view {
				Some(ref v) => v,
				None => return false, // cannot evaluate until we have a local view.
			};

			let our_best_commit = local_view.last_commit;
			let peer_best_commit = peer.view.last_commit;

			match GossipMessage::<Block>::decode(&mut data) {
				None => false,
				Some(GossipMessage::Commit(full)) => {
					// we only broadcast our best commit and only if it's
					// better than last received by peer.
					Some(full.message.target_number) == our_best_commit
					&& Some(full.message.target_number) > peer_best_commit
				}
				Some(GossipMessage::Neighbor(_)) => false,
				Some(GossipMessage::CatchUpRequest(_)) => false,
				Some(GossipMessage::CatchUp(_)) => false,
				Some(GossipMessage::VoteOrPrecommit(_)) => false, // should not be the case.
			}
		})
	}

	fn message_expired<'a>(&'a self) -> Box<dyn FnMut(Block::Hash, &[u8]) -> bool + 'a> {
		let inner = self.inner.read();
		Box::new(move |topic, mut data| {
			// if the topic is not one of the ones that we are keeping at the moment,
			// it is expired.
			match inner.live_topics.topic_info(&topic) {
				None => return true,
				Some((Some(_), _)) => return false, // round messages don't require further checking.
				Some((None, _)) => {},
			};

			let local_view = match inner.local_view {
				Some(ref v) => v,
				None => return true, // no local view means we can't evaluate or hold any topic.
			};

			// global messages -- only keep the best commit.
			let best_commit = local_view.last_commit;

			match GossipMessage::<Block>::decode(&mut data) {
				None => true,
				Some(GossipMessage::Commit(full))
					=> Some(full.message.target_number) != best_commit,
				Some(_) => true,
			}
		})
	}
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

use grandpa::Error as GrandpaError;
use grandpa::{voter, round::State as RoundState, BlockNumberOps, voter_set::VoterSet};

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

/// A primary propose message for this chain's block type.
pub type PrimaryPropose<Block> = grandpa::PrimaryPropose<<Block as BlockT>::Hash, NumberFor<Block>>;
/// A prevote message for this chain's block type.
pub type Prevote<Block> = grandpa::Prevote<<Block as BlockT>::Hash, NumberFor<Block>>;
/// A precommit message for this chain's block type.
pub type Precommit<Block> = grandpa::Precommit<<Block as BlockT>::Hash, NumberFor<Block>>;
/// A catch up message for this chain's block type.
pub type CatchUp<Block> = grandpa::CatchUp<
	<Block as BlockT>::Hash,
	NumberFor<Block>,
	AuthoritySignature,
	AuthorityId,
>;
/// A commit message for this chain's block type.
pub type Commit<Block> = grandpa::Commit<
	<Block as BlockT>::Hash,
	NumberFor<Block>,
	AuthoritySignature,
	AuthorityId,
>;
/// A compact commit message for this chain's block type.
pub type CompactCommit<Block> = grandpa::CompactCommit<
	<Block as BlockT>::Hash,
	NumberFor<Block>,
	AuthoritySignature,
	AuthorityId,
>;
/// A global communication input stream for commits and catch up messages. Not
/// exposed publicly, used internally to simplify types in the communication
/// layer.
type CommunicationIn<Block> = grandpa::voter::CommunicationIn<
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
	/// The expected duration for a message to be gossiped across the network.
	pub gossip_duration: Duration,

	/// The local signing key.
	pub local_key: Option<Arc<ed25519::Pair>>,
	/// Some local identifier of the voter.
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
				.grandpa_authorities(&BlockId::number(Zero::zero()))?;
			telemetry!(CONSENSUS_DEBUG; "afg.loading_authorities";
				"authorities_len" => ?genesis_authorities.len()
			);
			Ok(genesis_authorities)
		}
	)?;

	let (voter_commands_tx, voter_commands_rx) = mpsc::unbounded();

	Ok((
		GrandpaBlockImport::new(
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
	local_key: Option<&Arc<AuthorityPair>>,
	set_id: u64,
	voters: &Arc<VoterSet<AuthorityId>>,
	client: &Arc<Client<B, E, Block, RA>>,
	network: &NetworkBridge<Block, N>,
) -> (
	impl Stream<
		Item = CommunicationInH<Block, H256>,
		Error = CommandOrError<H256, NumberFor<Block>>,
	>,
	impl Sink<
		SinkItem = CommunicationOutH<Block, H256>,
		SinkError = CommandOrError<H256, NumberFor<Block>>,
	>,
) where
	B: Backend<Block, Blake2Hasher>,
	E: CallExecutor<Block, Blake2Hasher> + Send + Sync,
	N: Network<Block>,
	RA: Send + Sync,
	NumberFor<Block>: BlockNumberOps,
{

	let is_voter = local_key
		.map(|pair| voters.contains_key(&pair.public().into()))
		.unwrap_or(false);

	// verification stream
	let (global_in, global_out) = network.global_communication(
		communication::SetId(set_id),
		voters.clone(),
		is_voter,
	);

	// block commit and catch up messages until relevant blocks are imported.
	let global_in = UntilGlobalMessageBlocksImported::new(
		client.import_notification_stream(),
		client.clone(),
		global_in,
	);

	let global_in = global_in.map_err(CommandOrError::from);
	let global_out = global_out.sink_map_err(CommandOrError::from);

	(global_in, global_out)
}

/// Register the finality tracker inherent data provider (which is used by
/// GRANDPA), if not registered already.
fn register_finality_tracker_inherent_data_provider<B, E, Block: BlockT<Hash=H256>, RA>(
	client: Arc<Client<B, E, Block, RA>>,
	inherent_data_providers: &InherentDataProviders,
) -> Result<(), consensus_common::Error> where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + Send + Sync + 'static,
	RA: Send + Sync + 'static,
{
	if !inherent_data_providers.has_provider(&srml_finality_tracker::INHERENT_IDENTIFIER) {
		inherent_data_providers
			.register_provider(srml_finality_tracker::InherentDataProvider::new(move || {
				#[allow(deprecated)]
				{
					let info = client.backend().blockchain().info();
					telemetry!(CONSENSUS_INFO; "afg.finalized";
						"finalized_number" => ?info.finalized_number,
						"finalized_hash" => ?info.finalized_hash,
					);
					Ok(info.finalized_number)
				}
			}))
			.map_err(|err| consensus_common::Error::InherentData(err.into()))
	} else {
		Ok(())
	}
}

/// Parameters used to run Grandpa.
pub struct GrandpaParams<B, E, Block: BlockT<Hash=H256>, N, RA, SC, X> {
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

/// Run a GRANDPA voter as a task. Provide configuration and a link to a
/// block import worker that has already been instantiated with `block_import`.
pub fn run_grandpa_voter<B, E, Block: BlockT<Hash=H256>, N, RA, SC, X>(
	grandpa_params: GrandpaParams<B, E, Block, N, RA, SC, X>,
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
	let GrandpaParams {
		config,
		link,
		network,
		inherent_data_providers,
		on_exit,
		telemetry_on_connect,
	} = grandpa_params;

	use futures::future::{self, Loop as FutureLoop};

	let LinkHalf {
		client,
		select_chain,
		persistent_data,
		voter_commands_rx,
	} = link;

	let PersistentData { authority_set, set_state, consensus_changes } = persistent_data;

	let (network, network_startup) = NetworkBridge::new(
		network,
		config.clone(),
		set_state.clone(),
		on_exit.clone(),
	);

	register_finality_tracker_inherent_data_provider(client.clone(), &inherent_data_providers)?;

	let telemetry_task = if let Some(telemetry_on_connect) = telemetry_on_connect {
		let authorities = authority_set.clone();
		let events = telemetry_on_connect.telemetry_connection_sinks
			.for_each(move |_| {
				telemetry!(CONSENSUS_INFO; "afg.authority_set";
					 "authority_set_id" => ?authorities.set_id(),
					 "authorities" => {
						let curr = authorities.current_authorities();
						let voters = curr.voters();
						let authorities: Vec<String> =
							voters.iter().map(|(id, _)| id.to_string()).collect();
						serde_json::to_string(&authorities)
							.expect("authorities is always at least an empty vector; elements are always of type string")
					 }
				);
				Ok(())
			})
			.then(|_| -> Result<(), ()> { Ok(()) });
		futures::future::Either::A(events)
	} else {
		futures::future::Either::B(futures::future::empty())
	};

	let voters = authority_set.current_authorities();
	let initial_environment = Arc::new(Environment {
		inner: client.clone(),
		config: config.clone(),
		select_chain: select_chain.clone(),
		voters: Arc::new(voters),
		network: network.clone(),
		set_id: authority_set.set_id(),
		authority_set: authority_set.clone(),
		consensus_changes: consensus_changes.clone(),
		voter_set_state: set_state.clone(),
	});

	initial_environment.update_voter_set_state(|voter_set_state| {
		match voter_set_state {
			VoterSetState::Live { current_round: HasVoted::Yes(id, _), completed_rounds } => {
				let local_id = config.local_key.clone().map(|pair| pair.public());
				let has_voted = match local_id {
					Some(local_id) => if *id == local_id {
						// keep the previous votes
						return Ok(None);
					} else {
						HasVoted::No
					},
					_ => HasVoted::No,
				};

				// NOTE: only updated on disk when the voter first
				// proposes/prevotes/precommits or completes a round.
				Ok(Some(VoterSetState::Live {
					current_round: has_voted,
					completed_rounds: completed_rounds.clone(),
				}))
			},
			_ => Ok(None),
		}
	}).expect("operation inside closure cannot fail; qed");

	let initial_state = (initial_environment, voter_commands_rx.into_future());
	let voter_work = future::loop_fn(initial_state, move |params| {
		let (env, voter_commands_rx) = params;
		debug!(target: "afg", "{}: Starting new voter with set ID {}", config.name(), env.set_id);
		telemetry!(CONSENSUS_DEBUG; "afg.starting_new_voter";
			"name" => ?config.name(), "set_id" => ?env.set_id
		);

		let mut maybe_voter = match &*env.voter_set_state.read() {
			VoterSetState::Live { completed_rounds, .. } => {
				let chain_info = client.info();

				let last_finalized = (
					chain_info.chain.finalized_hash,
					chain_info.chain.finalized_number,
				);

				let global_comms = global_communication(
					config.local_key.as_ref(),
					env.set_id,
					&env.voters,
					&client,
					&network,
				);

				let voters = (*env.voters).clone();

				let last_completed_round = completed_rounds.last();

				Some(voter::Voter::new(
					env.clone(),
					voters,
					global_comms,
					last_completed_round.number,
					last_completed_round.state.clone(),
					last_finalized,
				))
			},
			VoterSetState::Paused { .. } => None,
		};

		// needs to be combined with another future otherwise it can deadlock.
		let poll_voter = future::poll_fn(move || match maybe_voter {
			Some(ref mut voter) => voter.poll(),
			None => Ok(Async::NotReady),
		});

		let client = client.clone();
		let config = config.clone();
		let network = network.clone();
		let select_chain = select_chain.clone();
		let authority_set = authority_set.clone();
		let consensus_changes = consensus_changes.clone();

		let handle_voter_command = move |command: VoterCommand<_, _>, voter_commands_rx| {
			match command {
				VoterCommand::ChangeAuthorities(new) => {
					let voters: Vec<String> = new.authorities.iter().map(move |(a, _)| {
						format!("{}", a)
					}).collect();
					telemetry!(CONSENSUS_INFO; "afg.voter_command_change_authorities";
						"number" => ?new.canon_number,
						"hash" => ?new.canon_hash,
						"voters" => ?voters,
						"set_id" => ?new.set_id,
					);

					// start the new authority set using the block where the
					// set changed (not where the signal happened!) as the base.
					let genesis_state = RoundState::genesis((new.canon_hash, new.canon_number));

					let set_state = VoterSetState::Live {
						// always start at round 0 when changing sets.
						completed_rounds: CompletedRounds::new(
							CompletedRound {
								number: 0,
								state: genesis_state,
								base: (new.canon_hash, new.canon_number),
								votes: Vec::new(),
							},
							new.set_id,
							&*authority_set.inner().read(),
						),
						current_round: HasVoted::No,
					};

					#[allow(deprecated)]
					aux_schema::write_voter_set_state(&**client.backend(), &set_state)?;

					let set_state: SharedVoterSetState<_> = set_state.into();

					let env = Arc::new(Environment {
						inner: client,
						select_chain,
						config,
						voters: Arc::new(new.authorities.into_iter().collect()),
						set_id: new.set_id,
						network,
						authority_set,
						consensus_changes,
						voter_set_state: set_state,
					});

					Ok(FutureLoop::Continue((env, voter_commands_rx)))
				}
				VoterCommand::Pause(reason) => {
					info!(target: "afg", "Pausing old validator set: {}", reason);

					// not racing because old voter is shut down.
					env.update_voter_set_state(|voter_set_state| {
						let completed_rounds = voter_set_state.completed_rounds();
						let set_state = VoterSetState::Paused { completed_rounds };

						#[allow(deprecated)]
						aux_schema::write_voter_set_state(&**client.backend(), &set_state)?;
						Ok(Some(set_state))
					})?;

					Ok(FutureLoop::Continue((env, voter_commands_rx)))
				},
			}
		};

		poll_voter.select2(voter_commands_rx).then(move |res| match res {
			Ok(future::Either::A(((), _))) => {
				// voters don't conclude naturally; this could reasonably be an error.
				Ok(FutureLoop::Break(()))
			},
			Err(future::Either::B(_)) => {
				// the `voter_commands_rx` stream should not fail.
				Ok(FutureLoop::Break(()))
			},
			Ok(future::Either::B(((None, _), _))) => {
				// the `voter_commands_rx` stream should never conclude since it's never closed.
				Ok(FutureLoop::Break(()))
			},
			Err(future::Either::A((CommandOrError::Error(e), _))) => {
				// return inner voter error
				Err(e)
			}
			Ok(future::Either::B(((Some(command), voter_commands_rx), _))) => {
				// some command issued externally.
				handle_voter_command(command, voter_commands_rx.into_future())
			}
			Err(future::Either::A((CommandOrError::VoterCommand(command), voter_commands_rx))) => {
				// some command issued internally.
				handle_voter_command(command, voter_commands_rx)
			},
		})
	});

	let voter_work = voter_work
		.map(|_| ())
		.map_err(|e| {
			warn!("GRANDPA Voter failed: {:?}", e);
			telemetry!(CONSENSUS_WARN; "afg.voter_failed"; "e" => ?e);
		});

	let voter_work = network_startup.and_then(move |()| voter_work);

	// Make sure that `telemetry_task` doesn't accidentally finish and kill grandpa.
	let telemetry_task = telemetry_task
		.then(|_| futures::future::empty::<(), ()>());

	Ok(voter_work.select(on_exit).select2(telemetry_task).then(|_| Ok(())))
}

#[deprecated(since = "1.1", note = "Please switch to run_grandpa_voter.")]
pub fn run_grandpa<B, E, Block: BlockT<Hash=H256>, N, RA, SC, X>(
	grandpa_params: GrandpaParams<B, E, Block, N, RA, SC, X>,
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
	run_grandpa_voter(grandpa_params)
}
