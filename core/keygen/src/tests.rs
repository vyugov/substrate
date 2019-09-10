use client::{
	error::Result,
	runtime_api::{ApiExt, Core, RuntimeVersion},
	LongestChain,
};
use codec::Decode;
use consensus_common::import_queue::{
	BoxBlockImport, BoxFinalityProofImport, BoxJustificationImport,
};
use consensus_common::{
	BlockImportParams, BlockOrigin, ForkChoiceStrategy, ImportResult, ImportedAux,
};
use futures03::{StreamExt as _, TryStreamExt as _};
use keyring::Ed25519Keyring;
use network::config::{BoxFinalityProofRequestBuilder, ProtocolConfig, Roles};
use network::test::PassThroughVerifier;
use network::test::{Block, DummySpecialization, Hash, Peer, PeersClient, TestNetFactory};
use parking_lot::Mutex;
use primitives::{crypto::Public, ExecutionContext, NativeOrEncoded};
use sr_primitives::generic::BlockId;
use sr_primitives::traits::{ApiRef, Header as HeaderT, ProvideRuntimeApi};
use std::collections::{HashMap, HashSet};
use std::result;
use test_client::{self, runtime::BlockNumber};
use tokio::runtime::current_thread;

use super::*;

type PeerData = Mutex<Option<(u8, u8)>>;
type MecPeer = Peer<PeerData, DummySpecialization>;

struct MecTestNet {
	peers: Vec<MecPeer>,
	test_config: TestApi,
}

impl MecTestNet {
	fn new(test_config: TestApi, n_peers: usize) -> Self {
		let mut net = Self {
			peers: Vec::with_capacity(n_peers),
			test_config,
		};
		let config = Self::default_config();
		for _ in 0..n_peers {
			net.add_full_peer(&config);
		}
		net
	}
}

impl TestNetFactory for MecTestNet {
	type Specialization = DummySpecialization;
	type Verifier = PassThroughVerifier;
	type PeerData = PeerData;

	/// Create new test network with peers and given config.
	fn from_config(_config: &ProtocolConfig) -> Self {
		GrandpaTestNet {
			peers: Vec::new(),
			test_config: Default::default(),
		}
	}

	fn default_config() -> ProtocolConfig {
		// the authority role ensures gossip hits all nodes here.
		ProtocolConfig {
			roles: Roles::AUTHORITY,
		}
	}

	fn make_verifier(&self, _client: PeersClient, _cfg: &ProtocolConfig) -> Self::Verifier {
		PassThroughVerifier(false) // use non-instant finality.
	}

	fn peer(&mut self, i: usize) -> &mut MecPeer {
		&mut self.peers[i]
	}

	fn peers(&self) -> &Vec<MecPeer> {
		&self.peers
	}

	fn mut_peers<F: FnOnce(&mut Vec<MecPeer>)>(&mut self, closure: F) {
		closure(&mut self.peers);
	}
}

#[derive(Clone)]
struct Exit;

impl Future for Exit {
	type Item = ();
	type Error = ();

	fn poll(&mut self) -> Poll<(), ()> {
		Ok(Async::NotReady)
	}
}

#[derive(Default, Clone)]
pub(crate) struct TestApi {
	genesis_authorities: Vec<(AuthorityId, u64)>,
	scheduled_changes: Arc<Mutex<HashMap<Hash, ScheduledChange<BlockNumber>>>>,
	forced_changes: Arc<Mutex<HashMap<Hash, (BlockNumber, ScheduledChange<BlockNumber>)>>>,
}

impl TestApi {
	pub fn new(genesis_authorities: Vec<(AuthorityId, u64)>) -> Self {
		TestApi {
			genesis_authorities,
			scheduled_changes: Arc::new(Mutex::new(HashMap::new())),
			forced_changes: Arc::new(Mutex::new(HashMap::new())),
		}
	}
}

pub(crate) struct RuntimeApi {
	inner: TestApi,
}

impl ProvideRuntimeApi for TestApi {
	type Api = RuntimeApi;

	fn runtime_api<'a>(&'a self) -> ApiRef<'a, Self::Api> {
		RuntimeApi {
			inner: self.clone(),
		}
		.into()
	}
}

impl Core<Block> for RuntimeApi {
	fn Core_version_runtime_api_impl(
		&self,
		_: &BlockId<Block>,
		_: ExecutionContext,
		_: Option<()>,
		_: Vec<u8>,
	) -> Result<NativeOrEncoded<RuntimeVersion>> {
		unimplemented!("Not required for testing!")
	}

	fn Core_execute_block_runtime_api_impl(
		&self,
		_: &BlockId<Block>,
		_: ExecutionContext,
		_: Option<(Block)>,
		_: Vec<u8>,
	) -> Result<NativeOrEncoded<()>> {
		unimplemented!("Not required for testing!")
	}

	fn Core_initialize_block_runtime_api_impl(
		&self,
		_: &BlockId<Block>,
		_: ExecutionContext,
		_: Option<&<Block as BlockT>::Header>,
		_: Vec<u8>,
	) -> Result<NativeOrEncoded<()>> {
		unimplemented!("Not required for testing!")
	}
}

impl ApiExt<Block> for RuntimeApi {
	fn map_api_result<F: FnOnce(&Self) -> result::Result<R, E>, R, E>(
		&self,
		_: F,
	) -> result::Result<R, E> {
		unimplemented!("Not required for testing!")
	}

	fn runtime_version_at(&self, _: &BlockId<Block>) -> Result<RuntimeVersion> {
		unimplemented!("Not required for testing!")
	}

	fn record_proof(&mut self) {
		unimplemented!("Not required for testing!")
	}

	fn extract_proof(&mut self) -> Option<Vec<Vec<u8>>> {
		unimplemented!("Not required for testing!")
	}
}
