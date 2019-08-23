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
	message::{KeyGenMessage, SignMessage},
	peer::{PeerInfo, PeerState, Peers},
};
use hbbft_primitives::PublicKey;

#[derive(Debug, Encode, Decode)]
pub enum GossipMessage {
	KeyGen(KeyGenMessage),
	Sign(SignMessage),
}

#[derive(Debug)]
struct Inner {
	local_peer_info: PeerInfo,
	peers: Peers,
	config: crate::NodeConfig,
}

impl Inner {
	fn new(config: crate::NodeConfig) -> Self {
		Self {
			config,
			local_peer_info: PeerInfo::default(),
			peers: Peers::default(),
		}
	}

	fn set_local_state(&mut self, state: PeerState) {
		self.local_peer_info.state = state;
	}

	fn set_peer_state(&mut self, who: &PeerId, state: PeerState) {
		self.peers.set_state(who, state);
	}
}

pub struct GossipValidator<Block: BlockT> {
	inner: parking_lot::RwLock<Inner>,
	_phantom: PhantomData<Block>,
}

impl<Block: BlockT> GossipValidator<Block> {
	pub fn new(config: crate::NodeConfig) -> Self {
		Self {
			inner: parking_lot::RwLock::new(Inner::new(config)),
			_phantom: PhantomData,
		}
	}
}

impl<Block: BlockT> network_gossip::Validator<Block> for GossipValidator<Block> {
	fn new_peer(&self, context: &mut dyn ValidatorContext<Block>, who: &PeerId, _roles: Roles) {
		println!("in new peer");
		{
			let mut inner = self.inner.write();
			inner.peers.add(who.clone());
			println!("{:?}", inner.peers);
		}

		let inner = self.inner.read();
		if inner.config.players as usize - 1 == inner.peers.len(){
			println!("SHOULD START KEY GEN");
		}
		// if inner.config

		// 2. key gen?
		// awaiting peers -> send all my peer public keys to peer
		// generating ->
		// finished ->
	}

	fn peer_disconnected(&self, _context: &mut dyn ValidatorContext<Block>, who: &PeerId) {
		println!("in peer disconnected");
	}

	fn validate(
		&self,
		context: &mut dyn ValidatorContext<Block>,
		who: &PeerId,
		data: &[u8],
	) -> network_gossip::ValidationResult<Block::Hash> {
		println!("in validate {:?}", data);
		let topic = super::global_topic::<Block>(1);
		network_gossip::ValidationResult::ProcessAndKeep(topic)
	}

	fn message_allowed<'a>(
		&'a self,
	) -> Box<dyn FnMut(&PeerId, MessageIntent, &Block::Hash, &[u8]) -> bool + 'a> {
		let (inner, do_rebroadcast) = {
			use parking_lot::RwLockWriteGuard;

			let mut inner = self.inner.write();

			let now = Instant::now();
			let do_rebroadcast = false;
			// downgrade to read-lock.
			(RwLockWriteGuard::downgrade(inner), do_rebroadcast)
		};

		Box::new(move |who, intent, topic, mut data| {
			println!("message_allowed  inner: {:?}, data: {:?}", inner, data);
			false
		})
	}

	fn message_expired<'a>(&'a self) -> Box<dyn FnMut(Block::Hash, &[u8]) -> bool + 'a> {
		let inner = self.inner.read();
		Box::new(move |topic, mut data| {
			println!("message_expired {:?}", data);
			// match *inner {
			// 	1 => false,
			// 	_ => true,
			// }
			true
		})
	}
}
