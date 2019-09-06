use codec::Encode;
use futures::prelude::*;
use futures::sync::mpsc;
use keyring::Ed25519Keyring;
use network::consensus_gossip as network_gossip;
use network::test::{Block, Hash};
use network_gossip::Validator;
use std::sync::Arc;
use tokio::runtime::current_thread;

use super::gossip::{self, GossipValidator};
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

fn make_test_network() -> (impl Future<Item = Tester, Error = ()>, TestNetwork) {
	let (tx, rx) = mpsc::unbounded();
	let net = TestNetwork { sender: tx };

	#[derive(Clone)]
	struct Exit;

	impl Future for Exit {
		type Item = ();
		type Error = ();

		fn poll(&mut self) -> Poll<(), ()> {
			Ok(Async::NotReady)
		}
	}

	let config = NodeConfig {
		threshold: 1,
		players: 3,
		name: None,
	};

	let id = network::PeerId::random();
	let (bridge, startup_work) = super::NetworkBridge::new(net.clone(), config, id);

	(
		startup_work.map(move |()| Tester {
			gossip_validator: bridge.validator.clone(),
			net_handle: bridge,
			events: rx,
		}),
		net,
	)
}
struct NoopContext;

impl network_gossip::ValidatorContext<Block> for NoopContext {
	fn broadcast_topic(&mut self, _: Hash, _: bool) {}
	fn broadcast_message(&mut self, _: Hash, _: Vec<u8>, _: bool) {}
	fn send_message(&mut self, _: &network::PeerId, _: Vec<u8>) {}
	fn send_topic(&mut self, _: &network::PeerId, _: Hash, _: bool) {}
}

#[test]
fn test_message() {}
