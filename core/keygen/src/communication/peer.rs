use codec::{Decode, Encode, Error as CodecError, Input};
use hbbft_primitives::PublicKey;
use log::{debug, error, trace, warn};
use multihash::Multihash as PkHash;
use network::{config::Roles, PeerId};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cmp::{min, Ordering};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::convert::From;

pub type Index = usize;

#[derive(Debug)]
pub enum PeerState {
	AwaitingPeers,
	Generating,
	Complete,
}

impl Default for PeerState {
	fn default() -> Self {
		PeerState::AwaitingPeers
	}
}

#[derive(Debug)]
pub(crate) struct PeerInfo {
	pub state: PeerState,
}

impl Default for PeerInfo {
	fn default() -> Self {
		Self {
			state: PeerState::default(),
		}
	}
}

#[derive(Debug)]
pub(crate) struct Peers {
	map: HashMap<PeerId, PeerInfo>,
	set: BTreeSet<String>,
}

impl Default for Peers {
	fn default() -> Self {
		Self {
			map: HashMap::default(),
			set: BTreeSet::default(),
		}
	}
}

impl Peers {
	pub fn add(&mut self, who: PeerId) {
		let base58_id = who.to_base58();
		self.map.insert(who, PeerInfo::default());
		self.set.insert(base58_id);
	}

	pub fn del(&mut self, who: &PeerId) {
		self.map.remove(who);
		self.set.remove(&who.to_base58());
	}

	pub fn len(&self) -> usize {
		self.map.len()
	}

	pub fn set_state(&mut self, who: &PeerId, state: PeerState) {
		let peer = self.map.get_mut(who).expect("Peer not found!");
		peer.state = state;
	}

	pub fn set_joining(&mut self, who: &PeerId) {
		self.set_state(who, PeerState::Joining);
	}

	pub fn set_finished(&mut self, who: &PeerId) {
		self.set_state(who, PeerState::Finished);
	}

	pub fn get_position(&self, who: &PeerId) -> Option<usize> {
		self.set.iter().position(|x| *x == who.to_base58())
	}
}
