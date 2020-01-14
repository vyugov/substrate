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

use crate::Network;
use crate::state_machine::{ConsensusGossip, Validator, RawMessage,ValidatorContext};
use std::collections::{ HashSet, }; //HashMap, hash_map::Entry

use sc_network::Context;
use sc_network::message::generic::ConsensusMessage;
use sc_network::{Event, ReputationChange};

use futures::{prelude::*, channel::mpsc, compat::Compat01As03, task::SpawnExt as _};
use libp2p::PeerId;
use parking_lot::Mutex;
use sp_runtime::{traits::Block as BlockT, ConsensusEngineId};
use std::{sync::Arc, time::Duration};

/// Wraps around an implementation of the `Network` crate and provides flooding capabilities on
/// top of it.
pub struct RantingEngine<B: BlockT> {
	inner: Arc<Mutex<RantingEngineInner<B>>>,
	engine_id: ConsensusEngineId,
}

pub struct RantingEngineInner<B: BlockT> {
	state_machine: ConsensusGossip<B>,
	network: Box<dyn Network<B> + Send>,
}

pub struct NetworkOvertext<'g,  B: BlockT>
{
  gossipa: &'g mut RantingEngineInner<B>,
  engine_id: ConsensusEngineId,
}


impl<'g,  B: BlockT> ValidatorContext<B> for NetworkOvertext<'g,  B> {

    /// Broadcast to all except
	fn broadcast_except(&mut self, except: HashSet<PeerId>, message: Vec<u8>)
	{
		self.gossipa.state_machine.broadcast_except(&mut *self.gossipa.network, self.engine_id,except , message, false,false)
	}
	fn send_to_set(&mut self,set:HashSet<PeerId>, message: Vec<u8>)
	{
		self.gossipa.state_machine.send_to_set(&mut *self.gossipa.network,self.engine_id, set, message, true,false,);
	}
	fn send_single(&mut self,who:&PeerId, message: Vec<u8>)
	{
		self.gossipa.state_machine.send_single(&mut *self.gossipa.network, self.engine_id,who, message)
	}
	fn keep(&mut self,cell:B::Hash, message: Vec<u8>)
	{
		self.gossipa.state_machine.register_message(cell, self.engine_id.clone(),message);
	}
	fn get_kept(&self,cell:&B::Hash )->Option<Vec<u8>>
	{
		self.gossipa.state_machine.get_kept(&cell)
	
	}
  
}

impl<B: BlockT> RantingEngine<B> {

	pub fn with_lock<Fnt,C>(&self,f_call:Fnt) ->C
	where Fnt: FnOnce(&mut NetworkOvertext<B>) ->C
	{
		let mut lock=&mut self.inner.lock();
		let mut context = NetworkOvertext { gossipa: &mut lock, engine_id: self.engine_id.clone() };
        f_call(&mut context)
	}

	/// Create a new instance.
	pub fn new<N: Network<B> + Send + Clone + 'static>(
		mut network: N,
		executor: &impl futures::task::Spawn,
		engine_id: ConsensusEngineId,
		validator: Arc<dyn Validator<B>>,
	) -> Self where B: 'static {
		let mut state_machine = ConsensusGossip::new(network.local_id());
	/*	let mut context = Box::new(ContextOverService {
			network: network.clone(),
		});
		let context_ext = Box::new(ContextOverService {
			network: network.clone(),
		});*/

		// We grab the event stream before registering the notifications protocol, otherwise we
		// might miss events.
		let event_stream = network.event_stream();

		network.register_notifications_protocol(engine_id);
		state_machine.register_validator(&mut network, engine_id, validator);

		let inner = Arc::new(Mutex::new(RantingEngineInner {
			state_machine,
			network: Box::new(network)
		}));

		let gossip_engine = RantingEngine {
			inner: inner.clone(),
			engine_id,
		};

		let res = executor.spawn({
			let inner = Arc::downgrade(&inner);
			async move {
				loop {
					let _ = futures_timer::Delay::new(Duration::from_millis(1100)).await;
					if let Some(inner) = inner.upgrade() {
						let mut inner = inner.lock();
						let inner = &mut *inner;
						inner.state_machine.tick(&mut *inner.network);
					} else {
						// We reach this branch if the `Arc<RantingEngineInner>` has no reference
						// left. We can now let the task end.
						break;
					}
				}
			}
		});

		// Note: we consider the chances of an error to spawn a background task almost null.
		if res.is_err() {
			log::error!(target: "gossip", "Failed to spawn background task");
		}

		let res = executor.spawn(async move {
			let mut stream = Compat01As03::new(event_stream);
			while let Some(Ok(event)) = stream.next().await {
				match event {
					Event::NotificationStreamOpened { remote, engine_id: msg_engine_id, roles } => {
						if msg_engine_id != engine_id {
							continue;
						}
						let mut inner = inner.lock();
						let inner = &mut *inner;
						inner.state_machine.new_peer(&mut *inner.network, remote, roles);
					}
					Event::NotificationsStreamClosed { remote, engine_id: msg_engine_id } => {
						if msg_engine_id != engine_id {
							continue;
						}
						let mut inner = inner.lock();
						let inner = &mut *inner;
						inner.state_machine.peer_disconnected(&mut *inner.network, remote);
					},
					Event::NotificationsReceived { remote, messages } => {
						let mut inner = inner.lock();
						let inner = &mut *inner;
						inner.state_machine.on_incoming(
							&mut *inner.network,
							remote,
							messages.into_iter()
								.filter_map(|(engine, data)| if engine == engine_id {
									Some(ConsensusMessage { engine_id: engine, data: data.to_vec() })
								} else { None })
								.collect()
						);
					},
					Event::Dht(_) => {}
				}
			}
		});

		// Note: we consider the chances of an error to spawn a background task almost null.
		if res.is_err() {
			log::error!(target: "gossip", "Failed to spawn background task");
		}

		gossip_engine
	}

	/// Closes all notification streams.
	pub fn abort(&self) {
		self.inner.lock().state_machine.abort();
	}

	pub fn report(&self, who: PeerId, reputation: ReputationChange) {
		self.inner.lock().network.report_peer(who, reputation);
	}

	/// Registers a message without propagating it to any peers. The message
	/// becomes available to new peers or when the service is asked to gossip
	/// the message's topic. No validation is performed on the message, if the
	/// message is already expired it should be dropped on the next garbage
	/// collection.
	pub fn register_gossip_message(
		&self,
		topic: B::Hash,
		message: Vec<u8>,
	) {

		self.inner.lock().state_machine.register_message(topic, self.engine_id.clone(),message);
	}

	/// Get data of valid, incoming messages for a topic (but might have expired meanwhile).
	pub fn messages_for(&self, )
		-> mpsc::UnboundedReceiver<RawMessage>
	{
		self.inner.lock().state_machine.messages_for(self.engine_id,)
	}



	pub fn broadcast_except(&self, except: HashSet<PeerId>, message: Vec<u8>)
	{
		let mut inner = self.inner.lock();
		let inner = &mut *inner;
		inner.state_machine.broadcast_except(&mut *inner.network, self.engine_id,except , message, false,false)
	}
	pub fn send_to_set(&self,set:HashSet<PeerId>, message: Vec<u8>)
	{
		let mut inner = self.inner.lock();
		let inner = &mut *inner;
		inner.state_machine.send_to_set(&mut *inner.network,self.engine_id, set, message, true,false,);
	}
	pub fn send_single(&self,who:PeerId, message: Vec<u8>)
	{
		let mut inner = self.inner.lock();
		let inner = &mut *inner;
	   inner.state_machine.send_single(&mut *inner.network, self.engine_id,&who, message)
	}
	pub fn keep(&self,cell:B::Hash, message: Vec<u8>)
	{
		self.inner.lock().state_machine.register_message(cell, self.engine_id.clone(),message);
   }

	/// Send addressed message to the given peers. The message is not kept or multicast
	/// later on.
	pub fn send_message(&self, who: Vec<sc_network::PeerId>, data: Vec<u8>) {
		let mut inner = self.inner.lock();
		let inner = &mut *inner;

		for who in &who {
			inner.state_machine.send_message(&mut *inner.network, who, self.engine_id.clone(), data.clone(),);
		}
	}

	/// Notify everyone we're connected to that we have the given block.
	///
	/// Note: this method isn't strictly related to gossiping and should eventually be moved
	/// somewhere else.
	pub fn announce(&self, block: B::Hash, associated_data: Vec<u8>) {
		self.inner.lock().network.announce(block, associated_data);
	}
}

impl<B: BlockT> Clone for RantingEngine<B> {
	fn clone(&self) -> Self {
		RantingEngine {
			inner: self.inner.clone(),
			engine_id: self.engine_id.clone(),
		}
	}
}

/*
struct ContextOverService<N> {
	network: N,
}

/// Validation context. Allows reacting to incoming messages by sending out further messages.



impl<B: BlockT, N: Network<B>> Context<B> for ContextOverService<N> {
	fn report_peer(&mut self, who: PeerId, reputation: ReputationChange) {
		self.network.report_peer(who, reputation);
	}

	fn disconnect_peer(&mut self, who: PeerId) {
		self.network.disconnect_peer(who)
	}

	fn send_consensus(&mut self, who: PeerId, messages: Vec<ConsensusMessage>) {
		for message in messages {
			self.network.write_notification(who.clone(), message.engine_id, message.data);
		}
	}

	fn send_chain_specific(&mut self, _: PeerId, _: Vec<u8>) {
		log::error!(
			target: "sub-libp2p",
			"send_chain_specific has been called in a context where it shouldn't"
		);
	}
}

trait ContextExt<B: BlockT> {
	fn announce(&self, block: B::Hash, associated_data: Vec<u8>);
}

impl<B: BlockT, N: Network<B>> ContextExt<B> for ContextOverService<N> {
	fn announce(&self, block: B::Hash, associated_data: Vec<u8>) {
		Network::announce(&self.network, block, associated_data)
	}
}*/
