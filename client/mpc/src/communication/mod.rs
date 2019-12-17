use std::{future, marker::Unpin, pin::Pin, sync::Arc};

use codec::{Decode, Encode};
use futures::channel::oneshot::{self, Canceled};
use futures::compat::{Compat, Compat01As03};
use futures::future::FutureExt;
use futures::prelude::{Future, Sink, Stream, TryStream};
use futures::stream::{FilterMap, StreamExt, TryStreamExt};
use futures::task::{Context, Poll};
use log::{error, info, trace};

use sc_network::message::generic::{ConsensusMessage, Message};
use sc_network::{NetworkService, PeerId};
use sc_network_gossip::TopicNotification;
use sp_runtime::traits::{
	Block as BlockT, DigestFor, Hash as HashT, Header as HeaderT, NumberFor, ProvideRuntimeApi,
};

use sp_mpc::MPC_ENGINE_ID;

pub mod gossip;
pub mod message;
mod peer;

use crate::Error;

use gossip::{GossipMessage, GossipValidator, MessageWithReceiver, MessageWithSender};
use message::{ConfirmPeersMessage, KeyGenMessage, SignMessage};

pub struct NetworkStream<R> {
	inner: Option<R>,
	outer: oneshot::Receiver<R>,
}

impl<R> Stream for NetworkStream<R>
where
	R: Stream<Item = TopicNotification> + Unpin,
{
	type Item = R::Item;

	fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
		if let Some(ref mut inner) = self.as_mut().inner {
			return inner.poll_next_unpin(cx);
		}

		match self.outer.poll_unpin(cx) {
			Poll::Ready(r) => match r {
				Ok(mut inner) => {
					let poll_result = inner.poll_next_unpin(cx);
					self.inner = Some(inner);
					poll_result
				}
				Err(Canceled) => panic!("Oneshot cancelled"),
			},
			Poll::Pending => Poll::Pending,
		}
	}
}

pub(crate) fn hash_topic<B: BlockT>(hash: u64) -> B::Hash {
	<<B::Header as HeaderT>::Hashing as HashT>::hash(&hash.to_be_bytes())
}

pub(crate) fn string_topic<B: BlockT>(input: &str) -> B::Hash {
	<<B::Header as HeaderT>::Hashing as HashT>::hash(input.as_bytes())
}

use futures::channel::mpsc::UnboundedReceiver;
impl<B, S, H> Network<B> for Arc<NetworkService<B, S, H>>
where
	B: BlockT,
	S: sc_network::specialization::NetworkSpecialization<B>,
	H: sc_network::ExHashT,
{
	type In = NetworkStream<UnboundedReceiver<TopicNotification>>; //Pin<Box<dyn Stream<Item = TopicNotification> + Send>>>;

	fn messages_for(&self, topic: B::Hash) -> Self::In {
		let (tx, rx) = oneshot::channel();

		self.with_gossip(move |gossip, _| {
			let inner_rx = gossip.messages_for(MPC_ENGINE_ID, topic);
			//	let inner_rx = Compat01As03::new(inner_rx).map(|x| x.unwrap()).boxed();
			let _ = tx.send(inner_rx);
		});

		Self::In {
			outer: rx,
			inner: None,
		}
	}

	fn register_validator(&self, validator: Arc<dyn sc_network_gossip::Validator<B>>) {
		self.with_gossip(move |gossip, context| {
			gossip.register_validator(context, MPC_ENGINE_ID, validator)
		})
	}

	fn gossip_message(&self, topic: B::Hash, data: Vec<u8>, force: bool) {
		let msg = ConsensusMessage {
			engine_id: MPC_ENGINE_ID,
			data,
		};

		self.with_gossip(move |gossip, ctx| gossip.multicast(ctx, topic, msg, force))
	}

	fn register_gossip_message(&self, topic: B::Hash, data: Vec<u8>) {
		let msg = ConsensusMessage {
			engine_id: MPC_ENGINE_ID,
			data,
		};

		self.with_gossip(move |gossip, _| gossip.register_message(topic, msg))
	}

	fn send_message(&self, who: Vec<sc_network::PeerId>, data: Vec<u8>) {
		let msg = ConsensusMessage {
			engine_id: MPC_ENGINE_ID,
			data,
		};

		self.with_gossip(move |gossip, ctx| {
			for who in &who {
				gossip.send_message(ctx, who, msg.clone())
			}
		})
	}

	fn report(&self, who: sc_network::PeerId, cost_benefit: i32) {
		self.report_peer(who, cost_benefit)
	}

	fn announce(&self, block: B::Hash, associated_data: Vec<u8>) {
		self.announce_block(block, associated_data)
	}
}

struct MessageSender<Block: BlockT, N: Network<Block> + Unpin> {
	network: N,
	validator: Arc<GossipValidator<Block>>,
}

impl<Block, N> MessageSender<Block, N>
where
	Block: BlockT,
	N: Network<Block> + Unpin,
{
	fn broadcast(&self, msg: GossipMessage) {
		let raw_msg = msg.encode();
		let inner = self.validator.inner.read();
		let peers = inner.get_other_peers();
		self.network.send_message(peers, raw_msg);
	}

	fn send_message(&self, target: PeerId, msg: GossipMessage) {
		let raw_msg = msg.encode();
		self.network.send_message(vec![target], raw_msg);
	}
}

impl<Block, N> Sink<MessageWithReceiver> for MessageSender<Block, N>
where
	Block: BlockT,
	N: Network<Block> + Unpin,
{
	type Error = Error;

	fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}

	fn start_send(self: Pin<&mut Self>, item: MessageWithReceiver) -> Result<(), Self::Error> {
		let (msg, receiver_opt) = item;

		if let Some(receiver) = receiver_opt {
			self.send_message(receiver, msg);
		} else {
			self.broadcast(msg);
		}

		Ok(())
	}

	fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}

	fn poll_close(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}
}

pub(crate) struct NetworkBridge<B: BlockT, N: Network<B>> {
	pub service: N,
	pub validator: Arc<GossipValidator<B>>,
}

impl<B, N> NetworkBridge<B, N>
where
	B: BlockT,
	N: Network<B> + Unpin,
	N::In: Stream<Item = TopicNotification> + Unpin + Send,
{
	pub fn new(service: N, config: crate::NodeConfig, local_peer_id: PeerId) -> Self {
		let validator = Arc::new(GossipValidator::new(config, local_peer_id));
		service.register_validator(validator.clone());

		Self { service, validator }
	}

	pub fn global(
		&self,
	) -> (
		impl Stream<Item = MessageWithSender>,
		impl Sink<MessageWithReceiver, Error = Error>,
	) {
		let topic = string_topic::<B>("hash"); // related with `fn validate` in gossip.rs

		let incoming = self.service.messages_for(topic).filter_map(|notification| {
			async {
				let decoded = GossipMessage::decode(&mut &notification.message[..]);
				if let Err(e) = decoded {
					trace!("notification error {:?}", e);
					println!("NOTIFICATION ERROR");
					return None;
				}
				println!("sender in global {:?}", notification.sender);
				Some((decoded.unwrap(), notification.sender))
			}
		});

		let outgoing = MessageSender::<B, N> {
			network: self.service.clone(),
			validator: self.validator.clone(),
		};

		(incoming.boxed(), outgoing)
	}
}

impl<B: BlockT, N: Network<B>> Clone for NetworkBridge<B, N> {
	fn clone(&self) -> Self {
		Self {
			service: self.service.clone(),
			validator: Arc::clone(&self.validator),
		}
	}
}

// #[cfg(test)]
// mod tests;
