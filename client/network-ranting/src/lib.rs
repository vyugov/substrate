// Copyright 2019 Parity Technologies (UK) Ltd.
// This file is part of Substrate.

// Substrate is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Substrate is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Substrate.  If not, see <http://www.gnu.org/licenses/>.

//! Impolite ranting.
//!
//! This crate provides multicast  flood capabilities on top of a network.
//!
//! Gossip messages are separated by two categories: "topics" and consensus engine ID.
//! The consensus engine ID is sent over the wire with the message, while the topic is not,
//! with the expectation that the topic can be derived implicitly from the content of the
//! message, assuming it is valid.
//!
//! Topics are a single 32-byte tag associated with a message, used to group those messages
//! in an opaque way. Consensus code can invoke `broadcast_topic` to attempt to send all messages
//! under a single topic to all peers who don't have them yet, and `send_topic` to
//! send all messages under a single topic to a specific peer.
//!
//! # Usage
//!
//! - Implement the `Network` trait, representing the low-level networking primitives. It is
//!   already implemented on `sc_network::NetworkService`.
//! - Implement the `Validator` trait. See the section below.
//! - Decide on a `ConsensusEngineId`. Each gossiping protocol should have a different one.
//! - Build a `RantingEngine` using these three elements.
//! - Use the methods of the `RantingEngine` in order to send out messages and receive incoming
//!   messages.
//!
//! # What is a validator?
//!
//! The primary role of a `Validator` is to process incoming messages from peers, and decide
//! whether to discard them or process them. It also decides whether to re-broadcast the message.
//!
//! The secondary role of the `Validator` is to check if a message is allowed to be sent to a given
//! peer. All messages, before being sent, will be checked against this filter.
//! This enables the validator to use information it's aware of about connected peers to decide
//! whether to send messages to them at any given moment in time - In particular, to wait until
//! peers can accept and process the message before sending it.
//!
//! Lastly, the fact that gossip validators can decide not to rebroadcast messages
//! opens the door for neighbor status packets to be baked into the gossip protocol.
//! These status packets will typically contain light pieces of information
//! used to inform peers of a current view of protocol state.

pub use self::bridge::RantingEngine;
pub use self::state_machine::{Validator, ValidatorContext, ValidationResult};
pub use self::state_machine::DiscardAll;

use futures::prelude::*;
use sc_network::{specialization::NetworkSpecialization, Event, ExHashT, NetworkService, PeerId, ReputationChange};
use sp_runtime::{traits::Block as BlockT, ConsensusEngineId};
use std::sync::Arc;

mod bridge;
mod state_machine;
pub use state_machine::RawMessage;
/// Abstraction over a network.
pub trait Network<B: BlockT> {
	/// Returns a stream of events representing what happens on the network.
	fn event_stream(&self) -> Box<dyn futures01::Stream<Item = Event, Error = ()> + Send>;

	/// Adjust the reputation of a node.
	fn report_peer(&self, peer_id: PeerId, reputation: ReputationChange);

	/// Force-disconnect a peer.
	fn disconnect_peer(&self, who: PeerId);

	/// Send a notification to a peer.
	fn write_notification(&self, who: PeerId, engine_id: ConsensusEngineId, message: Vec<u8>);

	/// Registers a notifications protocol.
	///
	/// See the documentation of [`NetworkService:register_notifications_protocol`] for more information.
	fn register_notifications_protocol(
		&self,
		engine_id: ConsensusEngineId
	);
	
	fn local_id(&self)->PeerId;
	/// Notify everyone we're connected to that we have the given block.
	///
	/// Note: this method isn't strictly related to gossiping and should eventually be moved
	/// somewhere else.
	fn announce(&self, block: B::Hash, associated_data: Vec<u8>);
}
use sc_network::NetworkStateInfo;
impl<B: BlockT, S: NetworkSpecialization<B>, H: ExHashT> Network<B> for Arc<NetworkService<B, S, H>> {
	fn event_stream(&self) -> Box<dyn futures01::Stream<Item = Event, Error = ()> + Send> {
		Box::new(NetworkService::event_stream(self).map(|v| Ok::<_, ()>(v)).compat())
	}
	fn local_id(&self) ->PeerId
	{
		self.local_peer_id().clone()
	} 

	fn report_peer(&self, peer_id: PeerId, reputation: ReputationChange) {
		NetworkService::report_peer(self, peer_id, reputation);
	}

	fn disconnect_peer(&self, who: PeerId) {
		NetworkService::disconnect_peer(self, who)
	}

	fn write_notification(&self, who: PeerId, engine_id: ConsensusEngineId, message: Vec<u8>) {
		NetworkService::write_notification(self, who, engine_id, message)
	}

	fn register_notifications_protocol(
		&self,
		engine_id: ConsensusEngineId,
	) {
		NetworkService::register_notifications_protocol(self, engine_id)
	}

	fn announce(&self, block: B::Hash, associated_data: Vec<u8>) {
		NetworkService::announce_block(self, block, associated_data)
	}
}
