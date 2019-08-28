use codec::{Decode, Encode};
use futures::prelude::*;
use futures::sync::mpsc;
use hbbft_primitives::AuthorityId;
use log::{debug, error, trace, warn};
use network::consensus_gossip::{self as network_gossip, MessageIntent, ValidatorContext};
use network::{config::Roles, PeerId};
use serde::{Deserialize, Serialize};
use sr_primitives::traits::{Block as BlockT, NumberFor, Zero};
use std::{
	collections::{HashMap, VecDeque},
	marker::PhantomData,
	time::{Duration, Instant},
};
use substrate_telemetry::{telemetry, CONSENSUS_DEBUG};

use super::{
	message::{ConfirmPeersMessage, KeyGenMessage, Message, SignMessage},
	peer::{PeerInfo, PeerState, Peers},
	string_topic,
};
use hbbft_primitives::PublicKey;

#[derive(Debug, Encode, Decode)]
pub enum GossipMessage {
	Message(Message),
}

#[derive(Debug)]
pub struct Inner {
	local_peer_id: PeerId,
	local_peer_info: PeerInfo,
	peers: Peers,
	config: crate::NodeConfig,
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
		}
	}

	fn get_peer_index(&self, who: &PeerId) -> usize {
		self.peers.get_position(who).unwrap()
	}

	fn add_peer(&mut self, who: PeerId) {
		self.peers.add(who);
	}

	fn del_peer(&mut self, who: &PeerId) {
		self.peers.del(who);
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
	fn new_peer(&self, context: &mut dyn ValidatorContext<Block>, who: &PeerId, _roles: Roles) {
		{
			let mut inner = self.inner.write();
			inner.add_peer(who.clone());
		}

		let inner = self.inner.read();
		if inner.config.players as usize == inner.peers.len() {
			// broadcast message to check all peers are the same
			// may need to handle ">" case
			let peers_hash = inner.peers.get_hash();
			let from_index = inner.peers.get_position(&inner.local_peer_id).unwrap() as u16;
			let confirm_peers_msg = ConfirmPeersMessage::Confirming(from_index, peers_hash);
			let msg = Message::ConfirmPeers(confirm_peers_msg);
			self.broadcast(context, GossipMessage::Message(msg).encode());
			// let topic = string_topic::<Block>("hash");
			// context.broadcast_message(topic, GossipMessage::Message(msg).encode(), false);
			println!("SHOULD START KEY GEN");
		}
	}

	fn peer_disconnected(&self, _context: &mut dyn ValidatorContext<Block>, who: &PeerId) {
		println!("in peer disconnected");
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
		println!("in validate");
		let gossip_msg = GossipMessage::decode(&mut data);
		if let Ok(gossip_msg) = gossip_msg {
			let topic = super::string_topic::<Block>("hash");
			return network_gossip::ValidationResult::ProcessAndKeep(topic);
		}
		network_gossip::ValidationResult::Discard
	}

	fn message_allowed<'a>(
		&'a self,
	) -> Box<dyn FnMut(&PeerId, MessageIntent, &Block::Hash, &[u8]) -> bool + 'a> {
		// rebroadcasted message
		let inner = self.inner.read();

		Box::new(move |who, intent, topic, mut data| {
			let gossip_msg = GossipMessage::decode(&mut data);
			if let Ok(gossip_msg) = gossip_msg {
				println!(
					"In `message_allowed` inner: {:?}, msg: {:?}",
					inner, gossip_msg
				);
				match gossip_msg {
					GossipMessage::Message(Message::ConfirmPeers(_)) => return true,
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
				println!("In `message_expired` msg: {:?}", gossip_msg);

				match gossip_msg {
					GossipMessage::Message(Message::ConfirmPeers(_)) => {
						if inner.config.players as usize == inner.peers.len()
							&& inner.local_peer_info.state != PeerState::AwaitingPeers
						{
							return true;
						} else {
							return false;
						}
					}
					_ => return false,
				}
			}
			true
		})
	}
}
