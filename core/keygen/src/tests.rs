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

pub struct TestNet {
	peers: Vec<Peer<(), DummySpecialization>>,
}

impl TestNet {
	fn new(n_peers: usize) -> Self {
		let mut net = Self {
			peers: Vec::with_capacity(n_peers),
		};
		let config = Self::default_config();
		for _ in 0..n_peers {
			net.add_full_peer(&config);
		}
		net
	}
}

impl TestNetFactory for TestNet {
	type Specialization = DummySpecialization;
	type Verifier = PassThroughVerifier;
	type PeerData = ();

	/// Create new test network with peers and given config.
	fn from_config(_config: &ProtocolConfig) -> Self {
		TestNet { peers: Vec::new() }
	}

	fn make_verifier(&self, _client: PeersClient, _config: &ProtocolConfig) -> Self::Verifier {
		PassThroughVerifier(false)
	}

	fn peer(&mut self, i: usize) -> &mut Peer<(), Self::Specialization> {
		&mut self.peers[i]
	}

	fn peers(&self) -> &Vec<Peer<(), Self::Specialization>> {
		&self.peers
	}

	fn mut_peers<F: FnOnce(&mut Vec<Peer<(), Self::Specialization>>)>(&mut self, closure: F) {
		closure(&mut self.peers);
	}
}

fn create_keystore(authority: Ed25519Keyring) -> (KeyStorePtr, tempfile::TempDir) {
	let keystore_path = tempfile::tempdir().expect("Creates keystore path");
	let keystore = keystore::Store::open(keystore_path.path(), None).expect("Creates keystore");
	(keystore, keystore_path)
}

#[test]
fn test_1_of_3_key_gen() {
	use super::*;
	let peers = &[
		Ed25519Keyring::Alice,
		Ed25519Keyring::Bob,
		Ed25519Keyring::Charlie,
	];

	let mut runtime = current_thread::Runtime::new().unwrap();
	let mut net = TestNet::new(peers.len());
	net.peer(0).push_blocks(10, false);
	net.block_until_sync(&mut runtime);

	let net = Arc::new(Mutex::new(net));

	let all_peers = peers.iter().cloned();
	let mut finality_notifications = Vec::new();

	for (peer_id, local_key) in all_peers.enumerate() {
		let net = net.lock();
		let client = net.peers[peer_id].client().clone();
		let network = net.peers[peer_id].network_service().clone();
		let local_peer_id = network.local_peer_id();
		let (keystore, keystore_path) = create_keystore(local_key);

		finality_notifications.push(
			client
				.finality_notification_stream()
				.map(|v| Ok::<_, ()>(v))
				.compat()
				.take_while(|n| Ok(n.header.number() < &10))
				.for_each(move |_| Ok(())),
		);
		let node =
			run_key_gen(local_peer_id, keystore, client.as_full().unwrap(), network).unwrap();
		runtime.spawn(node);
	}

	let wait_for = futures::future::join_all(finality_notifications)
		.map(|_| ())
		.map_err(|_| ());

	let drive_to_completion = futures::future::poll_fn(|| {
		net.lock().poll();
		Ok(Async::NotReady)
	});
	let _ = runtime
		.block_on(wait_for.select(drive_to_completion).map_err(|_| ()))
		.unwrap();
}
