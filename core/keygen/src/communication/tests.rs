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
fn test_message() {
	let id = network::PeerId::random();
	let global_topic = super::string_topic::<Block>("hash");

	let test = make_test_network()
		.0
		.and_then(move |tester| {
			// register a peer.
			tester
				.gossip_validator
				.new_peer(&mut NoopContext, &id, network::config::Roles::FULL);
			Ok((tester, id))
		})
		.and_then(move |(tester, id)| {
			// start round, dispatch commit, and wait for broadcast.
			let (commits_in, _) =
				tester
					.net_handle
					.global_communication(SetId(1), voter_set, false);

			{
				let (action, ..) = tester
					.gossip_validator
					.do_validate(&id, &encoded_commit[..]);
				match action {
					gossip::Action::ProcessAndDiscard(t, _) => assert_eq!(t, global_topic),
					_ => panic!("wrong expected outcome from initial commit validation"),
				}
			}

			let commit_to_send = encoded_commit.clone();

			// asking for global communication will cause the test network
			// to send us an event asking us for a stream. use it to
			// send a message.
			let sender_id = id.clone();
			let send_message = tester.filter_network_events(move |event| match event {
				Event::MessagesFor(topic, sender) => {
					if topic != global_topic {
						return false;
					}
					let _ = sender.unbounded_send(network_gossip::TopicNotification {
						message: commit_to_send.clone(),
						sender: Some(sender_id.clone()),
					});

					true
				}
				_ => false,
			});

			// when the commit comes in, we'll tell the callback it was good.
			let handle_commit = commits_in
				.into_future()
				.map(|(item, _)| match item.unwrap() {
					grandpa::voter::CommunicationIn::Commit(_, _, mut callback) => {
						callback.run(grandpa::voter::CommitProcessingOutcome::good());
					}
					_ => panic!("commit expected"),
				})
				.map_err(|_| panic!("could not process commit"));

			// once the message is sent and commit is "handled" we should have
			// a repropagation event coming from the network.
			send_message
				.join(handle_commit)
				.and_then(move |(tester, ())| {
					tester.filter_network_events(move |event| match event {
						Event::GossipMessage(topic, data, false) => {
							if topic == global_topic && data == encoded_commit {
								true
							} else {
								panic!("Trying to gossip something strange")
							}
						}
						_ => false,
					})
				})
				.map_err(|_| panic!("could not watch for gossip message"))
				.map(|_| ())
		});

	current_thread::block_on_all(test).unwrap();
}
