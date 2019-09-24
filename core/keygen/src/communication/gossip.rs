use codec::{Decode, Encode};
use log::{error, info, trace, warn};
use serde::{Deserialize, Serialize};
use std::{
	collections::VecDeque,
	marker::PhantomData,
	time::{Duration, Instant},
};

use network::consensus_gossip::{self as network_gossip, MessageIntent, ValidatorContext};
use network::{config::Roles, PeerId};
use sr_primitives::traits::{Block as BlockT, Zero};

use super::{
	message::{ConfirmPeersMessage, KeyGenMessage, Message, SignMessage},
	peer::{PeerInfo, PeerState, Peers},
	string_topic,
};

const REBROADCAST_AFTER: Duration = Duration::from_secs(60 * 5);

#[derive(Debug, Encode, Decode)]
pub enum GossipMessage {
	Message(Message),
}

pub struct Inner {
	local_peer_id: PeerId,
	local_peer_info: PeerInfo,
	peers: Peers,
	config: crate::NodeConfig,
	next_rebroadcast: Instant,
}

impl Inner {
	fn new(config: crate::NodeConfig, local_peer_id: PeerId) -> Self {
		let mut peers = Peers::default();
		peers.add(local_peer_id.clone());

		Self {
			config,
			local_peer_id,
			local_peer_info: PeerInfo::default(),
			peers,
			next_rebroadcast: Instant::now() + REBROADCAST_AFTER,
		}
	}
	fn add_peer(&mut self, who: PeerId) {
		self.peers.add(who);
	}

	fn del_peer(&mut self, who: &PeerId) {
		self.peers.del(who);
	}

	pub fn get_peers(&self) -> Vec<PeerId> {
		let local_id = &self.local_peer_id;
		self.peers
			.keys()
			.filter(|&pid| pid != local_id)
			.map(|x| x.clone())
			.collect()
	}

	pub fn get_peers_hash(&self) -> u64 {
		self.peers.get_hash()
	}

	pub fn get_local_index(&self) -> usize {
		self.get_peer_index(&self.local_id())
	}

	pub fn get_peer_index(&self, who: &PeerId) -> usize {
		self.peers.get_position(who).unwrap()
	}

	pub fn get_peer_id_by_index(&self, index: usize) -> Option<PeerId> {
		self.peers.get_peer_id_by_index(index)
	}

	pub fn local_id(&self) -> PeerId {
		self.local_peer_id.clone()
	}

	pub fn local_string_id(&self) -> String {
		self.local_peer_id.to_base58()
	}

	pub fn local_info(&self) -> PeerInfo {
		self.local_peer_info.clone()
	}

	pub fn set_local_generating(&mut self) {
		self.set_peer_generating(&self.local_id());
		self.local_peer_info.state = PeerState::Generating;
	}

	pub fn set_local_complete(&mut self) {
		self.set_peer_complete(&self.local_id());
		self.local_peer_info.state = PeerState::Complete;
	}

	pub fn set_peer_generating(&mut self, who: &PeerId) {
		self.peers.set_generating(who);
	}

	pub fn set_peer_complete(&mut self, who: &PeerId) {
		self.peers.set_complete(who);
	}
}

pub struct GossipValidator<Block: BlockT> {
	pub inner: parking_lot::RwLock<Inner>,
	_phantom: PhantomData<Block>,
}

impl<Block: BlockT> GossipValidator<Block> {
	pub fn new(config: crate::NodeConfig, local_peer_id: PeerId) -> Self {
		Self {
			inner: parking_lot::RwLock::new(Inner::new(config, local_peer_id)),
			_phantom: PhantomData,
		}
	}

	pub fn broadcast(&self, context: &mut dyn ValidatorContext<Block>, msg: Vec<u8>) {
		let inner = self.inner.read();
		let local_peer_id = &inner.local_peer_id;
		for (peer_id, _) in inner.peers.iter() {
			if peer_id != local_peer_id {
				context.send_message(peer_id, msg.clone())
			}
		}
	}
}

impl<Block: BlockT> network_gossip::Validator<Block> for GossipValidator<Block> {
	fn new_peer(&self, context: &mut dyn ValidatorContext<Block>, who: &PeerId, roles: Roles) {
		if roles != Roles::AUTHORITY {
			return;
		}

		let (players, all_peers) = {
			let mut inner = self.inner.write();
			inner.add_peer(who.clone());
			(inner.config.players as usize, inner.peers.len())
		};

		if players == all_peers {
			// broadcast message to check all peers are the same
			// may need to handle ">" case
			let inner = self.inner.read();

			let all_peers_hash = inner.peers.get_hash();
			let from_index = inner.peers.get_position(&inner.local_peer_id).unwrap() as u16;
			let msg =
				Message::ConfirmPeers(ConfirmPeersMessage::Confirming(from_index, all_peers_hash));
			self.broadcast(context, GossipMessage::Message(msg).encode());
		}
	}

	fn peer_disconnected(&self, _context: &mut dyn ValidatorContext<Block>, who: &PeerId) {
		{
			let mut inner = self.inner.write();
			inner.del_peer(who);
		}
	}

	fn validate(
		&self,
		context: &mut dyn ValidatorContext<Block>,
		who: &PeerId,
		mut data: &[u8],
	) -> network_gossip::ValidationResult<Block::Hash> {
		let gossip_msg = GossipMessage::decode(&mut data);
		if let Ok(_) = gossip_msg {
			let topic = super::string_topic::<Block>("hash");
			return network_gossip::ValidationResult::ProcessAndKeep(topic);
		}
		network_gossip::ValidationResult::Discard
	}

	fn message_allowed<'a>(
		&'a self,
	) -> Box<dyn FnMut(&PeerId, MessageIntent, &Block::Hash, &[u8]) -> bool + 'a> {
		// rebroadcasted message
		let (inner, do_rebroadcast) = {
			use parking_lot::RwLockWriteGuard;

			let mut inner = self.inner.write();
			let now = Instant::now();
			let do_rebroadcast = if now >= inner.next_rebroadcast {
				inner.next_rebroadcast = now + REBROADCAST_AFTER;
				true
			} else {
				false
			};

			(RwLockWriteGuard::downgrade(inner), do_rebroadcast)
		};

		Box::new(move |who, intent, topic, mut data| {
			println!("In `message_allowed` rebroadcast: {:?}", do_rebroadcast);

			if let MessageIntent::PeriodicRebroadcast = intent {
				return do_rebroadcast;
			}

			let gossip_msg = GossipMessage::decode(&mut data);
			if let Ok(gossip_msg) = gossip_msg {
				match gossip_msg {
					GossipMessage::Message(Message::ConfirmPeers(_)) => return true,
					GossipMessage::Message(Message::KeyGen(_)) => return true,
					_ => return false,
				}
			}
			false
		})
	}

	fn message_expired<'a>(&'a self) -> Box<dyn FnMut(Block::Hash, &[u8]) -> bool + 'a> {
		let inner = self.inner.read();

		Box::new(move |topic, mut data| {
			let gossip_msg = GossipMessage::decode(&mut data);
			if let Ok(gossip_msg) = gossip_msg {
				println!("In `message_expired`");
				// println!("msg: {:?}", gossip_msg);
				match gossip_msg {
					GossipMessage::Message(msg) => match msg {
						Message::ConfirmPeers(cpm) => {
							let invalid_state =
								inner.local_peer_info.state != PeerState::AwaitingPeers;
							match cpm {
								ConfirmPeersMessage::Confirming(from, hash) => {
									let our_hash = inner.get_peers_hash();
									return our_hash != hash || invalid_state;
								}
								_ => {}
							}
							return invalid_state;
						}
						Message::KeyGen(_) => {
							let complet_state = inner.local_peer_info.state == PeerState::Complete;
							return complet_state;
						}
						_ => return false,
					},
				}
			}
			true
		})
	}
}
