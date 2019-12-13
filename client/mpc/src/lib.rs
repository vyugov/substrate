use std::{
	fmt::Debug, hash::Hash, marker::PhantomData, pin::Pin, sync::Arc, thread, time::Duration,
};

use futures::{
	future::{select, FutureExt, TryFutureExt},
	prelude::{Future, Sink, Stream},
	stream::StreamExt,
};
use log::{debug, error, info};

use client::Client;
use client_api::{backend::Backend, BlockchainEvents, CallExecutor, ExecutionStrategy};
// use runtime_io::offchain::local_storage_set;

use primitives::offchain::{OffchainStorage, StorageKind};
use primitives::{Blake2Hasher, H256};
use sp_blockchain::{HeaderBackend, Result as ClientResult};
use sp_offchain::STORAGE_PREFIX;
use sp_runtime::generic::OpaqueDigestItemId;
use sp_runtime::traits::{Block as BlockT, Header};

use sp_mpc::{ConsensusLog, MPC_ENGINE_ID};

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
			let args = n
				.header
				.digest()
				.logs()
				.iter()
				.filter_map(|l| {
					l.try_to::<ConsensusLog>(OpaqueDigestItemId::Consensus(&MPC_ENGINE_ID))
				})
				.find_map(|l| match l {
					ConsensusLog::RequestForSig(id, data) => {
						info!("consensus log ok");
						Some((id, data))
					}
					_ => None,
				});

			if let Some((id, data)) = args {
				if let Some(mut offchain_storage) = backend.offchain_storage() {
					let key = id.to_le_bytes();
					info!("key {:?} data {:?}", key, data);
					offchain_storage.set(STORAGE_PREFIX, &key, &data);
					info!("set storage ok");
				}
			}
			futures::future::ready(())
		});

	Ok(streamer.map(|_| Ok::<(), ()>(())).compat())
}
