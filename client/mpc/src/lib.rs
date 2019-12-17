use std::{
	fmt::Debug, hash::Hash, marker::PhantomData, pin::Pin, sync::Arc, thread, time::Duration,
};

use futures::{
	future::{select, FutureExt, TryFutureExt},
	prelude::{Future, Sink, Stream},
	stream::StreamExt,
};
use log::{debug, error, info};
use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2018::party_i::{
	KeyGenBroadcastMessage1 as KeyGenCommit, KeyGenDecommitMessage1 as KeyGenDecommit, Keys,
	Parameters, PartyPrivate, SharedKeys, SignKeys,
};

use sc_client::Client;
use sc_client_api::{backend::Backend, BlockchainEvents, CallExecutor, ExecutionStrategy};
use sc_keystore::KeyStorePtr;
use sp_blockchain::{Error as ClientError, HeaderBackend, Result as ClientResult};
use sp_core::offchain::{OffchainStorage, StorageKind};
use sp_core::{Blake2Hasher, H256};
use sp_mpc::{ConsensusLog, RequestId, MPC_ENGINE_ID};
use sp_offchain::STORAGE_PREFIX;
use sp_runtime::generic::OpaqueDigestItemId;
use sp_runtime::traits::{Block as BlockT, Header};

mod communication;
mod periodic_stream;

type Count = u16;

#[derive(Debug)]
pub enum Error {
	Network(String),
	Periodic,
	Client(ClientError),
	Rebuild,
}

#[derive(Clone)]
pub struct NodeConfig {
	pub duration: u64,
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
pub enum MpcArgument {
	KeyGen(RequestId),
	SigGen(RequestId, Vec<u8>),
}

pub fn run_task<B, E, Block, RA>(
	client: Arc<Client<B, E, Block, RA>>,
	backend: Arc<B>,
) -> ClientResult<impl futures01::Future<Item = (), Error = ()>>
//ClientResult<impl Future<Output = ()> + Send + 'static>
where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + 'static + Send + Sync,
	Block: BlockT<Hash = H256> + Unpin,
	Block::Hash: Ord,
	// N: Network<Block> + Send + Sync + Unpin + 'static,
	// N::In: Send + 'static,
	RA: Send + Sync + 'static,
{
	let streamer = client
		.clone()
		.import_notification_stream()
		.for_each(move |n| {
			let logs = n.header.digest().logs().iter();

			let arg = logs
				.filter_map(|l| {
					l.try_to::<ConsensusLog>(OpaqueDigestItemId::Consensus(&MPC_ENGINE_ID))
				})
				.find_map(|l| match l {
					ConsensusLog::RequestForSig(id, data) => Some(MpcArgument::SigGen(id, data)),
					ConsensusLog::RequestForKey(id) => Some(MpcArgument::KeyGen(id)),
				});

			if let Some(arg) = arg {
				match arg {
					MpcArgument::SigGen(id, mut data) => {
						if let Some(mut offchain_storage) = backend.offchain_storage() {
							let key = id.to_le_bytes();
							info!("key {:?} data {:?}", key, data);
							let mut t = vec![1u8];
							t.append(&mut data);
							offchain_storage.set(STORAGE_PREFIX, &key, &t);
						}
					}
					MpcArgument::KeyGen(id) => {}
				}
			}

			futures::future::ready(())
		});

	Ok(streamer.map(|_| -> Result<(), ()> { Ok(()) }).compat())
}
