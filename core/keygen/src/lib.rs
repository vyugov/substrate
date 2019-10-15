#![feature(async_closure)]

use std::{
	collections::{BTreeMap, VecDeque},
	fmt::Debug,
	marker::Unpin,
	pin::Pin,
	sync::Arc,
	time::{Duration, Instant},
};

use codec::{Decode, Encode};
use curv::cryptographic_primitives::proofs::sigma_dlog::DLogProof;
use curv::cryptographic_primitives::secret_sharing::feldman_vss::VerifiableSS;
use curv::{FE, GE};

use futures03::channel::oneshot::{self, Canceled};
use futures03::compat::{Compat, Compat01As03};
use futures03::future::{FutureExt, TryFutureExt};
use futures03::prelude::{Future, Sink, Stream, TryStream};
use futures03::sink::SinkExt;
use futures03::stream::{FilterMap, StreamExt, TryStreamExt};
use futures03::task::{Context, Poll};

use keystore::KeyStorePtr;
use log::{debug, error, info};
use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2018::party_i::{
	KeyGenBroadcastMessage1 as KeyGenCommit, KeyGenDecommitMessage1 as KeyGenDecommit, Keys,
	Parameters, PartyPrivate, SharedKeys, SignKeys,
};
use parking_lot::RwLock;
use rand::prelude::Rng;
use tokio02::timer::Interval;

use client::blockchain::HeaderBackend;
use client::{
	backend::Backend, error::Error as ClientError, error::Result as ClientResult, BlockchainEvents,
	CallExecutor, Client,
};
use consensus_common::SelectChain;
use inherents::InherentDataProviders;
use network::{self, PeerId};
use primitives::{Blake2Hasher, H256};
use sr_primitives::generic::BlockId;
use sr_primitives::traits::{Block as BlockT, DigestFor, NumberFor, ProvideRuntimeApi};

mod communication;
mod periodic_stream;
mod shared_state;
mod signer;

use communication::{
	gossip::{GossipMessage, MessageWithSender},
	message::{ConfirmPeersMessage, KeyGenMessage, PeerIndex, SignMessage},
	Network, NetworkBridge,
};
use periodic_stream::PeriodicStream;
use shared_state::{load_persistent, set_signers, SharedState};
use signer::Signer;

type Count = u16;

const REBUILD_COOLDOWN: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub enum Error {
	Network(String),
	Periodic,
	Client(ClientError),
	Rebuild,
}

#[derive(Clone)]
pub struct NodeConfig {
	pub threshold: Count,
	pub players: Count,
	pub keystore: Option<KeyStorePtr>,
}

impl NodeConfig {
	pub fn get_params(&self) -> Parameters {
		Parameters {
			threshold: self.threshold,
			share_count: self.players,
		}
	}
}

#[derive(Debug)]
pub struct KeyGenState {
	pub complete: bool,
	pub local_key: Option<Keys>,
	pub commits: BTreeMap<PeerIndex, KeyGenCommit>,
	pub decommits: BTreeMap<PeerIndex, KeyGenDecommit>,
	pub vsss: BTreeMap<PeerIndex, VerifiableSS>,
	pub secret_shares: BTreeMap<PeerIndex, FE>,
	pub proofs: BTreeMap<PeerIndex, DLogProof>,
	pub shared_keys: Option<SharedKeys>,
}

impl KeyGenState {
	pub fn shared_public_key(&self) -> Option<GE> {
		self.shared_keys.clone().map(|sk| sk.y)
	}

	pub fn reset(&mut self) {
		*self = Self::default();
	}
}

impl Default for KeyGenState {
	fn default() -> Self {
		Self {
			complete: false,
			local_key: None,
			commits: BTreeMap::new(),
			decommits: BTreeMap::new(),
			vsss: BTreeMap::new(),
			secret_shares: BTreeMap::new(),
			proofs: BTreeMap::new(),
			shared_keys: None,
		}
	}
}

pub struct SigGenState {
	pub complete: bool,
	pub sign_key: Option<SignKeys>,
}

fn global_comm<Block, N>(
	bridge: &NetworkBridge<Block, N>,
) -> (
	impl Stream<Item = MessageWithSender>,
	impl Sink<MessageWithSender, Error = Error>,
)
where
	Block: BlockT<Hash = H256>,
	N: Network<Block> + Unpin,
	N::In: Send,
{
	let (global_in, global_out) = bridge.global();
	let global_in = PeriodicStream::<_, MessageWithSender>::new(global_in);

	(global_in, global_out)
}

pub(crate) struct Environment<B, E, Block: BlockT, N: Network<Block>, RA> {
	pub client: Arc<Client<B, E, Block, RA>>,
	pub config: NodeConfig,
	pub bridge: NetworkBridge<Block, N>,
	pub state: Arc<RwLock<KeyGenState>>,
}

struct KeyGenWork<B, E, Block: BlockT, N: Network<Block>, RA> {
	key_gen: Pin<Box<dyn Future<Output = Result<(), Error>> + Send + Unpin + 'static>>,
	env: Arc<Environment<B, E, Block, N, RA>>,
}

impl<B, E, Block, N, RA> KeyGenWork<B, E, Block, N, RA>
where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + Send + Sync + 'static,
	Block: BlockT<Hash = H256>,
	Block::Hash: Ord,
	N: Network<Block> + Unpin + Sync,
	N::In: Send + 'static,
	RA: Send + Sync + 'static,
{
	fn new(
		client: Arc<Client<B, E, Block, RA>>,
		config: NodeConfig,
		bridge: NetworkBridge<Block, N>,
	) -> Self {
		let state = KeyGenState::default();

		let env = Arc::new(Environment {
			client,
			config,
			bridge,
			state: Arc::new(RwLock::new(state)),
		});

		let mut work = Self {
			key_gen: Box::pin(futures03::future::pending()),
			env,
		};
		work.rebuild(true); // init should be okay
		work
	}

	fn rebuild(&mut self, last_message_ok: bool) {
		let (incoming, outgoing) = global_comm(&self.env.bridge);
		let signer = Signer::new(self.env.clone(), incoming, outgoing, last_message_ok);
		self.key_gen = Box::pin(signer);
	}
}

impl<B, E, Block, N, RA> Future for KeyGenWork<B, E, Block, N, RA>
where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + 'static + Send + Sync,
	Block: BlockT<Hash = H256>,
	Block::Hash: Ord,
	N: Network<Block> + Send + Sync + Unpin + 'static,
	N::In: Send + 'static,
	RA: Send + Sync + 'static,
{
	type Output = Result<(), Error>;

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
		println!("POLLLLLLLLLLLLLLLLLLLL");
		match self.key_gen.poll_unpin(cx) {
			Poll::Pending => {
				let (is_complete, is_canceled, commits_len) = {
					let state = self.env.state.read();
					let validator = self.env.bridge.validator.inner.read();

					println!(
						"INDEX {:?} state: commits {:?} decommits {:?} vss {:?} ss {:?}  proof {:?} has key {:?} complete {:?} peers hash {:?}",
						validator.get_local_index(),
						state.commits.len(),
						state.decommits.len(),
						state.vsss.len(),
						state.secret_shares.len(),
						state.proofs.len(),
						state.local_key.is_some(),
						state.complete,
						validator.get_peers_hash()
					);
					(
						validator.is_local_complete(),
						validator.is_local_canceled(),
						state.commits.len(),
					)
				};
				return Poll::Pending;
			}
			Poll::Ready(Ok(())) => {
				return Poll::Ready(Ok(()));
			}
			Poll::Ready(Err(e)) => {
				match e {
					Error::Rebuild => {
						println!("inner keygen rebuilt");
						self.rebuild(false);
						futures::task::current().notify();
						return Poll::Pending;
					}
					_ => {}
				}
				return Poll::Ready(Err(e));
			}
		}
	}
}

// pub fn init_shared_state<B, E, Block, RA>(client: Arc<Client<B, E, Block, RA>>) -> SharedState
// where
// 	B: Backend<Block, Blake2Hasher> + 'static,
// 	E: CallExecutor<Block, Blake2Hasher> + 'static + Clone + Send + Sync,
// 	Block: BlockT<Hash = H256>,
// 	Block::Hash: Ord,
// 	RA: Send + Sync + 'static,
// {
// 	let persistent_data: SharedState = load_persistent(&**client.backend()).unwrap();
// 	persistent_data
// }

pub fn run_key_gen<B, E, Block, N, RA>(
	local_peer_id: PeerId,
	(threshold, players): (PeerIndex, PeerIndex),
	keystore: KeyStorePtr,
	client: Arc<Client<B, E, Block, RA>>,
	network: N,
) -> ClientResult<impl Future<Output = Result<(), ()>> + Send + 'static>
where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + 'static + Send + Sync,
	Block: BlockT<Hash = H256>,
	Block::Hash: Ord,
	N: Network<Block> + Send + Sync + Unpin + 'static,
	N::In: Send + 'static,
	RA: Send + Sync + 'static,
{
	let config = NodeConfig {
		threshold,
		players,
		keystore: Some(keystore),
	};

	// let keystore = keystore.read();

	// let persistent_data: SharedState = load_persistent(&**client.backend()).unwrap();
	// println!("{:?}", persistent_data);
	// println!("Local peer ID {:?}", current_id.as_bytes());

	// let mut signers = persistent_data.signer_set;
	// let current_id = current_id.into_bytes();
	// if !signers.contains(&current_id) {
	// 	// if our id is not in it, add our self
	// 	signers.push(current_id);
	// 	set_signers(&**client.backend(), signers);
	// }
	let bridge = NetworkBridge::new(network, config.clone(), local_peer_id);

	let key_gen_work = KeyGenWork::new(client, config, bridge).map_err(|e| error!("Error {:?}", e));
	Ok(key_gen_work)
}

#[cfg(test)]
mod tests;
