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
use hbbft_primitives::HbbftApi;
use inherents::InherentDataProviders;
use log::{debug, error, info, warn};
use network::PeerId;
use primitives::{Blake2Hasher, H256};
use sr_primitives::generic::BlockId;
use sr_primitives::traits::{Block as BlockT, DigestFor, NumberFor, ProvideRuntimeApi};
use substrate_telemetry::{telemetry, CONSENSUS_DEBUG, CONSENSUS_INFO, CONSENSUS_WARN};
use tokio_executor::DefaultExecutor;
use tokio_timer::Interval;

use super::{
	ConfirmPeersMessage, Environment, GossipMessage, KeyGenMessage, Message, MessageWithSender,
	Network, SignMessage,
};

struct Buffered<S: Sink> {
	inner: S,
	buffer: VecDeque<S::SinkItem>,
}

impl<S: Sink> Buffered<S> {
	fn new(inner: S) -> Buffered<S> {
		Buffered {
			buffer: VecDeque::new(),
			inner,
		}
	}

	// push an item into the buffered sink.
	// the sink _must_ be driven to completion with `poll` afterwards.
	fn push(&mut self, item: S::SinkItem) {
		self.buffer.push_back(item);
	}

	// returns ready when the sink and the buffer are completely flushed.
	fn poll(&mut self) -> Poll<(), S::SinkError> {
		let polled = self.schedule_all()?;

		match polled {
			Async::Ready(()) => self.inner.poll_complete(),
			Async::NotReady => {
				self.inner.poll_complete()?;
				Ok(Async::NotReady)
			}
		}
	}

	fn schedule_all(&mut self) -> Poll<(), S::SinkError> {
		while let Some(front) = self.buffer.pop_front() {
			match self.inner.start_send(front) {
				Ok(AsyncSink::Ready) => continue,
				Ok(AsyncSink::NotReady(front)) => {
					self.buffer.push_front(front);
					break;
				}
				Err(e) => return Err(e),
			}
		}

		if self.buffer.is_empty() {
			Ok(Async::Ready(()))
		} else {
			Ok(Async::NotReady)
		}
	}
}

pub(crate) struct Signer<B, E, Block: BlockT, N: Network<Block>, RA, In, Out>
where
	In: Stream<Item = MessageWithSender, Error = ClientError>,
	Out: Sink<SinkItem = MessageWithSender, SinkError = ClientError>,
{
	env: Arc<Environment<B, E, Block, N, RA>>,
	global_in: In,
	global_out: Buffered<Out>,
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
	In: Stream<Item = MessageWithSender, Error = ClientError>,
	Out: Sink<SinkItem = MessageWithSender, SinkError = ClientError>,
{
	pub fn new(env: Arc<Environment<B, E, Block, N, RA>>, global_in: In, global_out: Out) -> Self {
		Signer {
			env,
			global_in,
			global_out: Buffered::new(global_out),
		}
	}

	fn handle_incoming(&mut self, msg: &Message, sender: &Option<PeerId>) {
		match msg {
			Message::ConfirmPeers(ConfirmPeersMessage::Confirming(index, hash)) => {
				println!("Receiving hash {:?} from {:?}", hash, index);
				let inner = self.env.bridge.validator.inner.read();
				self.global_out.push((
					Message::ConfirmPeers(ConfirmPeersMessage::Confirmed(inner.local_id())),
					sender.clone(),
				));
			}
			Message::ConfirmPeers(ConfirmPeersMessage::Confirmed(_)) => {
				// change state
				println!("Received confirmed hash");
			}
			Message::KeyGen(_) => {}
			Message::Sign(_) => {}
		}
	}
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
	In: Stream<Item = MessageWithSender, Error = ClientError>,
	Out: Sink<SinkItem = MessageWithSender, SinkError = ClientError>,
{
	type Item = ();
	type Error = ClientError;
	fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
		while let Async::Ready(Some(item)) = self.global_in.poll()? {
			let (msg, sender) = item;
			println!("global_in msg: {:?} from: {:?}", msg, sender);
			self.handle_incoming(&msg, &sender);
		}
		// send messages
		self.global_out.poll()?;
		Ok(Async::NotReady)
	}
}
