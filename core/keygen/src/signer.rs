use std::{
	collections::VecDeque,
	fmt::Debug,
	marker::PhantomData,
	sync::Arc,
	time::{Duration, Instant},
};

use client::blockchain::HeaderBackend;
use client::{
	backend::Backend, error::Error as ClientError, error::Result as ClientResult, BlockchainEvents,
	CallExecutor, Client,
};
use codec::{Decode, Encode};
use consensus_common::SelectChain;
use futures::{future::Loop as FutureLoop, prelude::*, stream::Fuse, sync::mpsc};
use hbbft::crypto::{PublicKey, SecretKey, SignatureShare};
use hbbft_primitives::HbbftApi;
use inherents::InherentDataProviders;
use log::{debug, error, info, warn};
use network;
use primitives::{Blake2Hasher, H256};
use sr_primitives::generic::BlockId;
use sr_primitives::traits::{Block as BlockT, DigestFor, NumberFor, ProvideRuntimeApi};
use substrate_telemetry::{telemetry, CONSENSUS_DEBUG, CONSENSUS_INFO, CONSENSUS_WARN};
use tokio_executor::DefaultExecutor;
use tokio_timer::Interval;

use super::{Environment, Message, Network, SignedMessage};

pub(crate) struct Signer<B, E, Block: BlockT, N: Network<Block>, RA, In, Out> {
	env: Arc<Environment<B, E, Block, N, RA>>,
	global_in: In,
	global_out: Out,
}

impl<B, E, Block, N, RA, In, Out> Signer<B, E, Block, N, RA, In, Out>
where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + Send + Sync + 'static,
	Block: BlockT<Hash = H256>,
	Block::Hash: Ord,
	N: Network<Block> + Sync,
	N::In: Send + 'static,
	RA: Send + Sync + 'static,
	In: Stream<Item = SignedMessage<Block>, Error = ClientError>,
	Out: Sink<SinkItem = Message<Block>, SinkError = ClientError>,
{
	pub fn new(env: Arc<Environment<B, E, Block, N, RA>>, global_in: In, global_out: Out) -> Self {
		Signer {
			env,
			global_in,
			global_out,
		}
	}
	// fn handle_incoming(&self) ->
}

impl<B, E, Block, N, RA, In, Out> Future for Signer<B, E, Block, N, RA, In, Out>
where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + Send + Sync + 'static,
	Block: BlockT<Hash = H256>,
	Block::Hash: Ord,
	N: Network<Block> + Sync,
	N::In: Send + 'static,
	RA: Send + Sync + 'static,
	In: Stream<Item = SignedMessage<Block>, Error = ClientError>,
	Out: Sink<SinkItem = Message<Block>, SinkError = ClientError>,
{
	type Item = ();
	type Error = ClientError;
	fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
		while let Async::Ready(Some(item)) = self.global_in.poll()? {
			println!("Item: {:?}", item);
		}
		Ok(Async::NotReady)
	}
}
