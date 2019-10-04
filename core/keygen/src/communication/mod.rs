#![feature(trait_alias)]


use std::{marker::PhantomData, sync::Arc};

use codec::{Decode, Encode};
#[cfg(not(feature = "upgraded"))]
use futures::prelude::*;

#[cfg(not(feature = "upgraded"))]
use futures::sync::{mpsc, oneshot};


#[cfg(feature="upgraded")]
use futures03::channel::{mpsc, oneshot};
#[cfg(feature="upgraded")]
use futures03::prelude::*;

use log::{error, info, trace};

use gossip::{GossipMessage, GossipValidator};
use network::{consensus_gossip as network_gossip, NetworkService, PeerId};
use network_gossip::ConsensusMessage;
use sr_primitives::traits::{
	Block as BlockT, DigestFor, Hash as HashT, Header as HeaderT, NumberFor, ProvideRuntimeApi,
};

use mpe_primitives::MP_ECDSA_ENGINE_ID;

pub mod gossip;
pub mod message;
mod peer;

use crate::Error;

use message::{
	ConfirmPeersMessage, KeyGenMessage, Message, MessageWithReceiver, MessageWithSender,
	SignMessage,
};

	type Item = network_gossip::TopicNotification;

/// A stream used by NetworkBridge in its implementation of Network.
pub struct NetworkStream
{
  inner: Option<mpsc::UnboundedReceiver<network_gossip::TopicNotification>>,
  outer: oneshot::Receiver<
    mpsc::UnboundedReceiver<network_gossip::TopicNotification>,
  >,
}

#[cfg(not(feature = "upgraded"))]
impl Stream for NetworkStream {
	type Item = network_gossip::TopicNotification;
	type Error = ();

	fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
		println!("POLL ");
		if let Some(ref mut inner) = self.inner {
			return inner.poll();
		}
		match self.outer.poll() {
			Ok(futures::Async::Ready(mut inner)) => {
				let poll_result = inner.poll();
				self.inner = Some(inner);
				poll_result
			}
			Ok(futures::Async::NotReady) => Ok(futures::Async::NotReady),
			Err(_) => Err(()),
		}
	}
}



#[cfg(feature = "upgraded")]
use std::{pin::Pin, task::Context, task::Poll};

#[cfg(feature = "upgraded")]
impl Stream for NetworkStream
{
  type Item = network_gossip::TopicNotification;

  fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context) -> Poll<Option<Self::Item>>
  {
    if let Some(ref mut inner) = self.inner
    {
      match inner.try_next()
      {
        Ok(Some(opt)) => return futures03::Poll::Ready(Some(opt)),
        Ok(None) => return futures03::Poll::Pending,
        Err(_) => return futures03::Poll::Pending,
      };
    }
    match self.outer.try_recv()
    {
      Ok(Some(mut inner)) =>
      {
        let poll_result = match inner.try_next()
        {
          Ok(Some(opt)) => futures03::Poll::Ready(Some(opt)),
          Ok(None) => futures03::Poll::Pending,
          Err(_) => futures03::Poll::Pending,
        };
        self.inner = Some(inner);
        poll_result
      }
      Ok(None) => futures03::Poll::Pending,
      Err(_) => futures03::Poll::Pending,
    }
  }
}



pub(crate) fn hash_topic<B: BlockT>(hash: u64) -> B::Hash {
	<<B::Header as HeaderT>::Hashing as HashT>::hash(&hash.to_be_bytes())
}

pub(crate) fn string_topic<B: BlockT>(input: &str) -> B::Hash {
	<<B::Header as HeaderT>::Hashing as HashT>::hash(input.as_bytes())
}

pub trait Network<Block: BlockT>: Clone + Send + 'static {
	/// A stream of input messages for a topic.
	#[cfg(not(feature = "upgraded"))]
	type In: Stream<Item = network_gossip::TopicNotification, Error = ()>;

	#[cfg(feature = "upgraded")]
	type In: Stream<Item = network_gossip::TopicNotification>;

	/// Get a stream of messages for a specific gossip topic.
	fn messages_for(&self, topic: Block::Hash) -> Self::In;

	/// Register a gossip validator.
	fn register_validator(&self, validator: Arc<dyn network_gossip::Validator<Block>>);

	/// Gossip a message out to all connected peers.
	///
	/// Force causes it to be sent to all peers, even if they've seen it already.
	/// Only should be used in case of consensus stall.
	fn gossip_message(&self, topic: Block::Hash, data: Vec<u8>, force: bool);

	/// Register a message with the gossip service, it isn't broadcast right
	/// away to any peers, but may be sent to new peers joining or when asked to
	/// broadcast the topic. Useful to register previous messages on node
	/// startup.
	fn register_gossip_message(&self, topic: Block::Hash, data: Vec<u8>);

	/// Send a message to a bunch of specific peers, even if they've seen it already.
	fn send_message(&self, who: Vec<network::PeerId>, data: Vec<u8>);

	/// Report a peer's cost or benefit after some action.
	fn report(&self, who: network::PeerId, cost_benefit: i32);

	/// Inform peers that a block with given hash should be downloaded.
	fn announce(&self, block: Block::Hash, associated_data: Vec<u8>);
}

impl<B, S, H> Network<B> for Arc<NetworkService<B, S, H>>
where
	B: BlockT,
	S: network::specialization::NetworkSpecialization<B>,
	H: network::ExHashT,
{
	type In = NetworkStream;

	fn messages_for(&self, topic: B::Hash) -> Self::In {
		let (tx, rx) = oneshot::channel();
		self.with_gossip(move |gossip, _| {
			let inner_rx = gossip.messages_for(MP_ECDSA_ENGINE_ID, topic);
			let _ = tx.send(inner_rx);
		});
		Self::In {
			outer: rx,
			inner: None,
		}
	}

	fn register_validator(&self, validator: Arc<dyn network_gossip::Validator<B>>) {
		self.with_gossip(move |gossip, context| {
			gossip.register_validator(context, MP_ECDSA_ENGINE_ID, validator)
		})
	}

	fn gossip_message(&self, topic: B::Hash, data: Vec<u8>, force: bool) {
		let msg = ConsensusMessage {
			engine_id: MP_ECDSA_ENGINE_ID,
			data,
		};

		self.with_gossip(move |gossip, ctx| gossip.multicast(ctx, topic, msg, force))
	}

	fn register_gossip_message(&self, topic: B::Hash, data: Vec<u8>) {
		let msg = ConsensusMessage {
			engine_id: MP_ECDSA_ENGINE_ID,
			data,
		};

		self.with_gossip(move |gossip, _| gossip.register_message(topic, msg))
	}

	fn send_message(&self, who: Vec<network::PeerId>, data: Vec<u8>) {
		let msg = ConsensusMessage {
			engine_id: MP_ECDSA_ENGINE_ID,
			data,
		};

		self.with_gossip(move |gossip, ctx| {
			for who in &who {
				gossip.send_message(ctx, who, msg.clone())
			}
		})
	}

	fn report(&self, who: network::PeerId, cost_benefit: i32) {
		self.report_peer(who, cost_benefit)
	}

	fn announce(&self, block: B::Hash, associated_data: Vec<u8>) {
		self.announce_block(block,associated_data)
	}
}

struct MessageSender<Block: BlockT, N: Network<Block>> {
	network: N,
	validator: Arc<GossipValidator<Block>>,
}

impl<Block, N> MessageSender<Block, N>
where
	Block: BlockT,
	N: Network<Block>,
{
	fn broadcast(&self, msg: Message) {
		let raw_msg = GossipMessage::Message(msg).encode();
		let inner = self.validator.inner.read();
		let peers = inner.get_peers();
		self.network.send_message(peers, raw_msg.clone());
	}

	fn send_message(&self, target: PeerId, msg: Message) {
		let raw_msg = GossipMessage::Message(msg).encode();
		self.network.send_message(vec![target], raw_msg);
	}
}

#[cfg(feature="upgraded")]
impl<Block: BlockT, N: Network<Block>> Sink<MessageWithReceiver> for MessageSender<Block, N>
{
  type Error = Error;

  fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<Result<(), Self::Error>>
  {
    Poll::Ready(Ok(()))
  }

 	fn start_send(self:Pin<&mut Self> , item: MessageWithReceiver) ->  Result<(), Self::Error>
	  {
		let (msg, receiver_opt) = item;

		if let Some(receiver) = receiver_opt {
			self.send_message(receiver, msg);
		} else {
			self.broadcast(msg);
		}

		Ok(())
	}

	 fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>>
  {
    Poll::Ready(Ok(()))
  }

	fn poll_close(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>>
  {
    Poll::Ready(Ok(()))
  }

}



#[cfg(not(feature = "upgraded"))]
impl<Block, N> Sink for MessageSender<Block, N>
where
	Block: BlockT,
	N: Network<Block>,
{
	type SinkItem = MessageWithReceiver;
	type SinkError = Error;

	fn start_send(&mut self, item: Self::SinkItem) -> StartSend<Self::SinkItem, Self::SinkError> {
		let (msg, receiver_opt) = item;

		if let Some(receiver) = receiver_opt {
			self.send_message(receiver, msg);
		} else {
			self.broadcast(msg);
		}

		Ok(AsyncSink::Ready)
	}

	fn poll_complete(&mut self) -> Poll<(), Error> {
		Ok(Async::Ready(()))
	}

	fn close(&mut self) -> Poll<(), Error> {
		Ok(Async::Ready(()))
	}
}

pub(crate) struct NetworkBridge<B: BlockT, N: Network<B>> {
	pub service: N,
	pub validator: Arc<GossipValidator<B>>,
}

#[cfg(not(feature = "upgraded"))]
pub trait MWSStream= Stream<Item = MessageWithSender, Error = Error>;


#[cfg(not(feature = "upgraded"))]
pub trait MWRSink= Sink<SinkItem = MessageWithReceiver, SinkError = Error>;

#[cfg(not(feature = "upgraded"))]
pub trait MWSSink= Sink<SinkItem = MessageWithSender, SinkError = Error>;


#[cfg(feature = "upgraded")]
pub trait MWSStream= Stream<Item = MessageWithSender>+Unpin;

#[cfg(feature = "upgraded")]
pub trait MWRSink= Sink<MessageWithReceiver,Error = Error>;

#[cfg(feature = "upgraded")]
pub trait MWSSink= Sink<MessageWithSender, Error = Error>+Unpin;



impl<B: BlockT, N: Network<B>> NetworkBridge<B, N> 
where <N as Network<B>>::In: Unpin
{
	pub fn new(service: N, config: crate::NodeConfig, local_peer_id: PeerId) -> Self {
		let validator = Arc::new(GossipValidator::new(config, local_peer_id));
		service.register_validator(validator.clone());

		Self { service, validator }
	}

	pub fn global(
		&self,
	) -> (
		impl MWSStream,
		impl MWRSink,
	) {
		let topic = string_topic::<B>("hash"); // related with fn validate in gossip.rs

		let incoming = self
			.service
			.messages_for(topic).filter_map(|notification| {
				let decoded = GossipMessage::decode(&mut &notification.message[..]);
				if let Err(e) = decoded {
					trace!("notification error {:?}", e);
					#[cfg(not(feature = "upgraded"))]
					return None;
					#[cfg(feature = "upgraded")]
					return future::ready(None);
				}
				#[cfg(not(feature = "upgraded"))]
				 return Some((decoded.unwrap(), notification.sender));
				#[cfg(feature = "upgraded")]
				 return future::ready(Some((decoded.unwrap(), notification.sender)));
				 
			})
			.filter_map(move |val|
			 {
				 #[cfg(not(feature = "upgraded"))]
				 {
				 let (msg, sender)=val;
			     match msg 
			     {
				 GossipMessage::Message(message) => return Some((message, sender)),
			     }
				}
                 #[cfg(feature = "upgraded")]
                 {
				  let (msg, sender)=val ;
				  
					match msg 
			       {
				   GossipMessage::Message(message) => return future::ready(Some((message, sender))),
			        }
				  
				  return future::ready(None)
				 }
		
			 }
			 );


		 #[cfg(not(feature = "upgraded"))]
		let incoming=incoming.map_err(|()| {
				Error::Network("Failed to receive message on unbounded stream".to_string())
			});
			

		let outgoing = MessageSender::<B, N> {
			network: self.service.clone(),
			validator: self.validator.clone(),
		};

		(incoming, outgoing)
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

#[cfg(test)]
mod tests;
