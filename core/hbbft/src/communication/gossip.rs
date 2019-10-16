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

//! Gossip and politeness for polite-grandpa.
//!
//! This module implements the following message types:
//! #### Neighbor Packet
//!
//! The neighbor packet is sent to only our neighbors. It contains this information
//!
//!   - Current Round
//!   - Current voter set ID
//!   - Last finalized hash from commit messages.
//!
//! If a peer is at a given voter set, it is impolite to send messages from
//! an earlier voter set. It is extremely impolite to send messages
//! from a future voter set. "future-set" messages can be dropped and ignored.
//!
//! If a peer is at round r, is impolite to send messages about r-2 or earlier and extremely
//! impolite to send messages about r+1 or later. "future-round" messages can
//!  be dropped and ignored.
//!
//! It is impolite to send a neighbor packet which moves backwards in protocol state.
//!
//! This is beneficial if it conveys some progress in the protocol state of the peer.
//!
//! #### Prevote / Precommit
//!
//! These are votes within a round. Noting that we receive these messages
//! from our peers who are not necessarily voters, we have to account the benefit
//! based on what they might have seen.
//!
//! #### Propose
//!
//! This is a broadcast by a known voter of the last-round estimate.
//!
//! #### Commit
//!
//! These are used to announce past agreement of finality.
//!
//! It is impolite to send commits which are earlier than the last commit
//! sent. It is especially impolite to send commits which are invalid, or from
//! a different Set ID than the receiving peer has indicated.
//!
//! Sending a commit is polite when it may finalize something that the receiving peer
//! was not aware of.
//!
//! #### Catch Up
//!
//! These allow a peer to request another peer, which they perceive to be in a
//! later round, to provide all the votes necessary to complete a given round
//! `R`.
//!
//! It is impolite to send a catch up request for a round `R` to a peer whose
//! announced view is behind `R`. It is also impolite to send a catch up request
//! to a peer in a new different Set ID.
//!
//! The logic for issuing and tracking pending catch up requests is implemented
//! in the `GossipValidator`. A catch up request is issued anytime we see a
//! neighbor packet from a peer at a round `CATCH_UP_THRESHOLD` higher than at
//! we are.
//!
//! ## Expiration
//!
//! We keep some amount of recent rounds' messages, but do not accept new ones from rounds
//! older than our current_round - 1.
//!
//! ## Message Validation
//!
//! We only send polite messages to peers,

//use runtime_primitives::traits::Block as BlockT;
//use network::consensus_gossip::{self as network_gossip, MessageIntent, ValidatorContext};
use fg_primitives::{AuthorityId, AuthoritySignature};
use network::PeerId; //config::Roles,
use parity_codec::{Decode, Encode};

//use substrate_telemetry::{telemetry, CONSENSUS_DEBUG};
//use log::{trace, debug, warn};
//use futures03::prelude::*;
//use futures03::channel::mpsc;

use log::info;
use std::collections::HashMap;
//use std::time::{ Instant};//Duration

//const REBROADCAST_AFTER: Duration = Duration::from_secs(60 * 5);
//const CATCH_UP_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
//const CATCH_UP_PROCESS_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum number of rounds we are behind a peer before issuing a
/// catch up request.
//const CATCH_UP_THRESHOLD: u64 = 2;

//const KEEP_RECENT_ROUNDS: usize = 3;

//const BADGER_TOPIC: &str = "itsasnake";

use crate::communication::PeerIdW;
/// HB gossip message type.
/// This is the root type that gets encoded and sent on the network.
#[derive(Debug, Encode, Decode)]
pub enum GossipMessage
{
  /// Grandpa message with round and set info.
  Greeting(GreetingMessage),
  /// Raw Badger data
  BadgerData(BadgeredMessage),

  RequestGreeting,
}

#[derive(Debug, Encode, Decode)]
pub struct BadgeredMessage
{
  pub uid: u64,
  pub data: Vec<u8>,
  pub originator: PeerIdW,
}

#[derive(Debug, Encode, Decode)]
pub struct GreetingMessage
{
  pub my_pubshare: Option<AuthorityId>,
  /// the badger ID of the peer
  pub my_id: AuthorityId,
  /// Signature to verify id
  pub my_sig: AuthoritySignature,
}

impl From<GreetingMessage> for GossipMessage
{
  fn from(greet: GreetingMessage) -> Self
  {
    GossipMessage::Greeting(greet)
  }
}

#[derive(Debug, Encode, Decode, PartialEq, Eq)]
pub enum PeerConsensusState
{
  Unknown,
  Connected,
  GreetingReceived,
  Disconnected,
}
pub struct PeerInfo
{
  //view: View<N>,
  pub id: Option<AuthorityId>, //public key
  pub state: PeerConsensusState,
}

impl PeerInfo
{
  fn new() -> Self
  {
    PeerInfo {
      id: None,
      state: PeerConsensusState::Unknown,
    }
  }
  fn new_id(id: AuthorityId) -> Self
  {
    PeerInfo {
      id: Some(id),
      state: PeerConsensusState::Unknown,
    }
  }
}

/// The peers we're connected to in gossip.
pub struct Peers
{
  inner: HashMap<PeerId, PeerInfo>,
}

impl Default for Peers
{
  fn default() -> Self
  {
    Peers {
      inner: HashMap::new(),
    }
  }
}

impl Peers
{
  pub fn new() -> Self
  {
    Peers {
      inner: HashMap::new(),
    }
  }
  pub fn new_peer(&mut self, who: PeerId)
  {
    match self.inner.get(&who)
    {
      Some(_) =>
      {}
      None =>
      {
        let ti = PeerInfo::new();
        self.inner.insert(who, ti);
      }
    }
  }
  pub fn update_peer_state(&mut self, who: &PeerId, state: PeerConsensusState)
  {
    match self.inner.get_mut(&who)
    {
      Some(dat) =>
      {
        dat.state = state;
      }
      None =>
      {
        let mut ti = PeerInfo::new();
        ti.state = state;
        self.inner.insert(who.clone(), ti);
      }
    };
  }
  pub fn peer_list(&self) -> Vec<PeerId>
  {
    self.inner.iter().map(|(k, _)| k.clone()).collect()
  }
  pub fn connected_peer_list(&self) -> Vec<PeerId>
  {
    self
      .inner
      .iter()
      .filter(|v| {
        v.1.state == PeerConsensusState::Connected ||
          v.1.state == PeerConsensusState::GreetingReceived
      })
      .map(|(k, _)| k.clone())
      .collect()
  }

  pub fn peer_disconnected(&mut self, who: &PeerId)
  {
    match self.inner.get_mut(&who)
    {
      Some(dat) =>
      {
        dat.state = PeerConsensusState::Disconnected;
      }
      None =>
      {}
    };
    //	self.inner.remove(who);
    info!(
      "Peer disconnected {:?} ! now {:?} peers known about",
      who,
      self.inner.len()
    );
  }

  pub fn update_id(&mut self, who: &PeerId, auth_id: AuthorityId)
  {
    let peer = match self.inner.get_mut(who)
    {
      None =>
      {
        self.inner.insert(who.clone(), PeerInfo::new_id(auth_id));
        return;
      }
      Some(p) => p,
    };
    peer.id = Some(auth_id);
  }

  pub fn peer<'a>(&'a self, who: &PeerId) -> Option<&'a PeerInfo>
  {
    self.inner.get(who)
  }
}

#[derive(Debug, PartialEq)]
pub enum Action<H>
{
  // repropagate under given topic, to the given peers, applying cost/benefit to originator.
  Keep(),
  // discard and process.
  ProcessAndDiscard(),
  // discard
  Discard(i32),
  Useless(H),
}

/*struct Inner
{
  //local_view: Option<View<NumberFor<Block>>>,
  peers: Peers,
  authorities: Vec<AuthorityId>,
  config: crate::Config,
  next_rebroadcast: Instant,
}*/
