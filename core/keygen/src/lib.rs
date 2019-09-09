use std::{
	collections::{BTreeMap, VecDeque},
	fmt::Debug,
	marker::PhantomData,
	sync::Arc,
	time::{Duration, Instant},
};

use codec::{Decode, Encode};
use curv::cryptographic_primitives::proofs::sigma_dlog::DLogProof;
use curv::cryptographic_primitives::secret_sharing::feldman_vss::VerifiableSS;
use curv::FE;
use futures::{prelude::*, stream::Fuse, sync::mpsc};
use log::{debug, error, info};
use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2018::party_i::{
	KeyGenBroadcastMessage1 as KeyGenCommit, KeyGenDecommitMessage1 as KeyGenDecommit, Keys,
	Parameters,
};
use parking_lot::RwLock;
use tokio_executor::DefaultExecutor;
use tokio_timer::Interval;

use client::blockchain::HeaderBackend;
use client::{
	backend::Backend, error::Error as ClientError, error::Result as ClientResult, BlockchainEvents,
	CallExecutor, Client,
};
use consensus_common::SelectChain;
use inherents::InherentDataProviders;
use mpe_primitives::HbbftApi;
use network::{self, PeerId};
use primitives::{Blake2Hasher, H256};
use sr_primitives::generic::BlockId;
use sr_primitives::traits::{Block as BlockT, DigestFor, NumberFor, ProvideRuntimeApi};
use substrate_telemetry::{telemetry, CONSENSUS_DEBUG, CONSENSUS_INFO, CONSENSUS_WARN};

mod communication;
mod periodic_stream;
mod shared_state;
mod signer;

use communication::{
	gossip::GossipMessage,
	message::{
		ConfirmPeersMessage, KeyGenMessage, Message, MessageWithSender, PeerIndex, SignMessage,
	},
	Network, NetworkBridge,
};
use periodic_stream::PeriodicStream;
use shared_state::{load_persistent, set_signers, SharedState};
use signer::Signer;

type Count = u16;

#[derive(Debug)]
pub enum Error {
	Network(String),
	Client(ClientError),
}

#[derive(Clone, Debug)]
pub struct NodeConfig {
	pub threshold: Count,
	pub players: Count,
	pub name: Option<String>,
}

impl NodeConfig {
	pub fn name(&self) -> &str {
		self.name
			.as_ref()
			.map(|s| s.as_str())
			.unwrap_or("<unknown>")
	}
}

#[derive(Debug, Default)]
pub struct Confirmations {
	pub peer: Count,
	pub com_decom: Count,
	pub secret_share: Count,
	pub vss: Count,
	pub proof: Count,
}

#[derive(Debug)]
pub struct KeyGenState {
	pub complete: bool,
	pub confirmations: Confirmations,
	pub local_key: Option<Keys>,
	pub commits: BTreeMap<PeerIndex, KeyGenCommit>,
	pub decommits: BTreeMap<PeerIndex, KeyGenDecommit>,
	pub vsss: BTreeMap<PeerIndex, VerifiableSS>,
	pub secret_shares: BTreeMap<PeerIndex, FE>,
	pub proofs: BTreeMap<PeerIndex, DLogProof>,
}

impl Default for KeyGenState {
	fn default() -> Self {
		Self {
			complete: false,
			confirmations: Confirmations::default(),
			local_key: None,
			commits: BTreeMap::new(),
			decommits: BTreeMap::new(),
			vsss: BTreeMap::new(),
			secret_shares: BTreeMap::new(),
			proofs: BTreeMap::new(),
		}
	}
}

fn global_comm<Block, N>(
	bridge: &NetworkBridge<Block, N>,
) -> (
	impl Stream<Item = MessageWithSender, Error = Error>,
	impl Sink<SinkItem = MessageWithSender, SinkError = Error>,
)
where
	Block: BlockT<Hash = H256>,
	N: Network<Block>,
{
	let (global_in, global_out) = bridge.global();
	let global_in = PeriodicStream::<Block, _, MessageWithSender>::new(global_in);
	(global_in, global_out)
}

pub(crate) struct Environment<B, E, Block: BlockT, N: Network<Block>, RA> {
	pub client: Arc<Client<B, E, Block, RA>>,
	pub config: NodeConfig,
	pub bridge: NetworkBridge<Block, N>,
	pub state: Arc<RwLock<KeyGenState>>,
}

struct KeyGenWork<B, E, Block: BlockT, N: Network<Block>, RA> {
	key_gen: Box<dyn Future<Item = (), Error = Error> + Send + 'static>,
	env: Arc<Environment<B, E, Block, N, RA>>,
}

impl<B, E, Block, N, RA> KeyGenWork<B, E, Block, N, RA>
where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + Send + Sync + 'static,
	Block: BlockT<Hash = H256>,
	Block::Hash: Ord,
	N: Network<Block> + Sync,
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
			// `voter` is set to a temporary value and replaced below when
			// calling `rebuild_voter`.
			key_gen: Box::new(futures::empty()) as Box<_>,
			env,
		};
		work.rebuild();
		work
	}

	fn rebuild(&mut self) {
		let should_rebuild = true;
		if should_rebuild {
			let (incoming, outgoing) = global_comm(&self.env.bridge);
			let signer = Signer::new(self.env.clone(), incoming, outgoing);
			self.key_gen = Box::new(signer);
		} else {
			self.key_gen = Box::new(futures::empty());
		}
	}
}

impl<B, E, Block, N, RA> Future for KeyGenWork<B, E, Block, N, RA>
where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + 'static + Send + Sync,
	Block: BlockT<Hash = H256>,
	Block::Hash: Ord,
	N: Network<Block> + Send + Sync + 'static,
	N::In: Send + 'static,
	RA: Send + Sync + 'static,
{
	type Item = ();
	type Error = Error;

	fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
		match self.key_gen.poll() {
			Ok(Async::NotReady) => {
				return Ok(Async::NotReady);
			}
			Ok(Async::Ready(())) => {
				return Ok(Async::Ready(()));
			}
			Err(e) => {
				// return inner observer error
				println!("error from poll {:?}", e);
				return Err(e);
			}
		}
	}
}

pub fn init_shared_state<B, E, Block, RA>(client: Arc<Client<B, E, Block, RA>>) -> SharedState
where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + 'static + Clone + Send + Sync,
	Block: BlockT<Hash = H256>,
	Block::Hash: Ord,
	RA: Send + Sync + 'static,
{
	let persistent_data: SharedState = load_persistent(&**client.backend()).unwrap();
	persistent_data
}

pub fn run_key_gen<B, E, Block, N, RA>(
	local_peer_id: PeerId,
	client: Arc<Client<B, E, Block, RA>>,
	network: N,
) -> ClientResult<impl Future<Item = (), Error = ()> + Send + 'static>
where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + 'static + Send + Sync,
	Block: BlockT<Hash = H256>,
	Block::Hash: Ord,
	N: Network<Block> + Send + Sync + 'static,
	N::In: Send + 'static,
	RA: Send + Sync + 'static,
{
	let config = NodeConfig {
		name: None,
		threshold: 1,
		players: 3,
	};

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
