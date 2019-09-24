use codec::Encode;
use futures::prelude::*;
use futures::sync::mpsc;
use keyring::Ed25519Keyring;
use network::consensus_gossip as network_gossip;
use network::test::{Block, Hash};
use network_gossip::Validator;
use std::sync::Arc;
use tokio::runtime::current_thread;
use tokio_executor::Executor;

use super::{
	gossip::{self, GossipValidator},
	message::{ConfirmPeersMessage, KeyGenMessage, Message, SignMessage},
};

use crate::NodeConfig;

enum Event {
	MessagesFor(
		Hash,
		mpsc::UnboundedSender<network_gossip::TopicNotification>,
	),
	RegisterValidator(Arc<dyn network_gossip::Validator<Block>>),
	GossipMessage(Hash, Vec<u8>, bool),
	SendMessage(Vec<network::PeerId>, Vec<u8>),
	Report(network::PeerId, i32),
	Announce(Hash),
}

#[derive(Clone)]
struct TestNetwork {
	sender: mpsc::UnboundedSender<Event>,
}

impl super::Network<Block> for TestNetwork {
	type In = mpsc::UnboundedReceiver<network_gossip::TopicNotification>;

	fn messages_for(&self, topic: Hash) -> Self::In {
		let (tx, rx) = mpsc::unbounded();
		let _ = self.sender.unbounded_send(Event::MessagesFor(topic, tx));

		rx
	}

	fn register_validator(&self, validator: Arc<dyn network_gossip::Validator<Block>>) {
		let _ = self
			.sender
			.unbounded_send(Event::RegisterValidator(validator));
	}

	fn gossip_message(&self, topic: Hash, data: Vec<u8>, force: bool) {
		let _ = self
			.sender
			.unbounded_send(Event::GossipMessage(topic, data, force));
	}

	fn send_message(&self, who: Vec<network::PeerId>, data: Vec<u8>) {
		let _ = self.sender.unbounded_send(Event::SendMessage(who, data));
	}

	fn register_gossip_message(&self, _topic: Hash, _data: Vec<u8>) {
		// NOTE: only required to restore previous state on startup
		//       not required for tests currently
	}

	fn report(&self, who: network::PeerId, cost_benefit: i32) {
		let _ = self.sender.unbounded_send(Event::Report(who, cost_benefit));
	}

	fn announce(&self, block: Hash) {
		let _ = self.sender.unbounded_send(Event::Announce(block));
	}
}
impl network_gossip::ValidatorContext<Block> for TestNetwork {
	fn broadcast_topic(&mut self, _: Hash, _: bool) {}

	fn broadcast_message(&mut self, _: Hash, _: Vec<u8>, _: bool) {}

	fn send_message(&mut self, who: &network::PeerId, data: Vec<u8>) {
		<Self as super::Network<Block>>::send_message(self, vec![who.clone()], data);
	}

	fn send_topic(&mut self, _: &network::PeerId, _: Hash, _: bool) {}
}

struct Tester {
	net_handle: super::NetworkBridge<Block, TestNetwork>,
	gossip_validator: Arc<GossipValidator<Block>>,
	events: mpsc::UnboundedReceiver<Event>,
}

impl Tester {
	fn filter_network_events<F>(self, mut pred: F) -> impl Future<Item = Self, Error = ()>
	where
		F: FnMut(Event) -> bool,
	{
		let mut s = Some(self);
		futures::future::poll_fn(move || loop {
			match s.as_mut().unwrap().events.poll().expect("concluded early") {
				Async::Ready(None) => panic!("concluded early"),
				Async::Ready(Some(item)) => {
					if pred(item) {
						return Ok(Async::Ready(s.take().unwrap()));
					}
				}
				Async::NotReady => return Ok(Async::NotReady),
			}
		})
	}
}

fn make_test_network() -> impl Future<Item = Tester, Error = ()> {
	let (tx, rx) = mpsc::unbounded();
	let net = TestNetwork { sender: tx };

	let config = NodeConfig {
		threshold: 1,
		players: 3,
		name: None,
		keystore: None,
	};

	let id = network::PeerId::random();
	let bridge = super::NetworkBridge::new(net.clone(), config, id);

	futures::future::lazy(move || {
		Ok(Tester {
			gossip_validator: bridge.validator.clone(),
			net_handle: bridge,
			events: rx,
		})
	})
}
struct NoopContext;

impl network_gossip::ValidatorContext<Block> for NoopContext {
	fn broadcast_topic(&mut self, _: Hash, _: bool) {}
	fn broadcast_message(&mut self, _: Hash, _: Vec<u8>, _: bool) {}
	fn send_message(&mut self, _: &network::PeerId, _: Vec<u8>) {}
	fn send_topic(&mut self, _: &network::PeerId, _: Hash, _: bool) {}
}

#[test]
fn test_confirm_peer_message() {
	let id = network::PeerId::random();

	let global_topic = super::string_topic::<Block>("hash");

	let encoded_msg = gossip::GossipMessage::Message(Message::ConfirmPeers(
		ConfirmPeersMessage::Confirming(0, 1),
	))
	.encode();

	let test = make_test_network()
		.and_then(move |tester| {
			tester.gossip_validator.new_peer(
				&mut NoopContext,
				&id,
				network::config::Roles::AUTHORITY,
			);

			Ok((tester, id))
		})
		.and_then(move |(tester, id)| {
			let (global_in, _) = tester.net_handle.global();

			let sender_id = id.clone();
			let msg_to_send = encoded_msg.clone();

			let send_message = tester.filter_network_events(move |event| match event {
				Event::MessagesFor(topic, sender) => {
					if topic != global_topic {
						return false;
					}
					let _ = sender.unbounded_send(network_gossip::TopicNotification {
						message: msg_to_send.clone(),
						sender: Some(sender_id.clone()),
					});

					true
				}
				_ => false,
			});

			let sender_id = id.clone();

			let handle_in = global_in
				.into_future()
				.map(move |(item, _)| {
					let (msg, sender_opt) = item.unwrap();
					match msg {
						Message::ConfirmPeers(ConfirmPeersMessage::Confirming(from, hash)) => {
							assert_eq!(from, 0);
							assert_eq!(hash, 1);
						}
						_ => panic!("invalid msg"),
					}

					assert_eq!(sender_opt.unwrap(), sender_id.clone());
				})
				.map_err(|_| panic!("could not process"));

			send_message
				.join(handle_in)
				.map_err(|_| panic!("could not watch for gossip message"))
				.map(|_| ())
		});

	current_thread::block_on_all(test).unwrap();
}
