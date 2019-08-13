// Copyright 2017-2019 Parity Technologies (UK) Ltd.
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

//! Communication streams for the polite-grandpa networking protocol.
//!
//! GRANDPA nodes communicate over a gossip network, where messages are not sent to
//! peers until they have reached a given round.
//!
//! Rather than expressing protocol rules,
//! polite-grandpa just carries a notion of impoliteness. Nodes which pass some arbitrary
//! threshold of impoliteness are removed. Messages are either costly, or beneficial.
//!
//! For instance, it is _impolite_ to send the same message more than once.
//! In the future, there will be a fallback for allowing sending the same message
//! under certain conditions that are used to un-stick the protocol.
use network::consensus_gossip::ValidatorContext;
use std::sync::Arc;
use std::iter;
use std::collections::{VecDeque};
use futures03::core_reexport::marker::PhantomData;
use std::time::{Duration, Instant};
use network::consensus_gossip:: MessageIntent;
use libp2p::swarm::{ PollParameters};
//use runtime_primitives::traits::NumberFor;
use badger::dynamic_honey_badger::DynamicHoneyBadger;
use badger::queueing_honey_badger::{ QueueingHoneyBadger};
use badger::sender_queue::{Message as BMessage, SenderQueue};
use badger::{ConsensusProtocol, CpStep, NetworkInfo,  Target,Contribution};
use rand::{rngs::OsRng, Rng};
use network::PeerId;
use ::unsigned_varint::{decode, encode};
use fg_primitives::{SignatureWrap,PublicKeyWrap};
//use grandpa::{voter, voter_set::VoterSet};
//use grandpa::Message::{Prevote, Precommit, PrimaryPropose};
use futures03::prelude::*;
use futures03::channel::{oneshot, mpsc};
use log::{debug, };// trace};
use parity_codec::{Encode, Decode};
use substrate_primitives::{ Pair};
use substrate_telemetry::{telemetry, CONSENSUS_DEBUG,};
use runtime_primitives::traits::{Block as BlockT, Hash as HashT, Header as HeaderT};
use network::{consensus_gossip as network_gossip, NetworkService};
use network_gossip::ConsensusMessage;
use crate::communication::gossip::GreetingMessage;

use libp2p::multihash;
use libp2p::multihash::Multihash;
#[macro_use]
use ::serde::{Serialize, Deserialize};
use ::serde::de::DeserializeOwned;
use gossip::GossipMessage;
use gossip::Peers;
use gossip::Action;
use fg_primitives::{AuthorityId, };
//use substrate_primitives::ed25519::{Public as AuthorityId, Signature as AuthoritySignature};
use network::config::Roles;

pub mod gossip;

use crate::Error;

const REBROADCAST_AFTER: Duration = Duration::from_secs(60 * 5);


#[cfg(test)]
mod tests;
pub use fg_primitives::HBBFT_ENGINE_ID;
//use badger::{SourcedMessage as BSM,  TargetedMessage};
// cost scalars for reporting peers.

/// A handle to the network. This is generally implemented by providing some
/// handle to a gossip service or similar.
///
/// Intended to be a lightweight handle such as an `Arc`.
pub trait Network<Block: BlockT>: Clone + Send + 'static {
	/// A stream of input messages for a topic.
	type In: Stream<Item=network_gossip::TopicNotification>;

	/// Get a stream of messages for a specific gossip topic.
	fn messages_for(&self, topic: Block::Hash) -> Self::In;

	/// Register a gossip validator.
	fn register_validator(&self, validator: Arc<dyn network_gossip::Validator<Block>>);

	/// Gossip a message out to all connected peers.
	///
	/// Force causes it to be sent to all peers, even if they've seen it already.
	/// Only should be used in case of consensus stall.
	fn gossip_message(&self, topic: Block::Hash, data: Vec<u8>, force: bool);

	/// Register a message with the gossip service, it isn't broadcast right
	/// away to any peers, but may be sent to new peers joining or when asked to
	/// broadcast the topic. Useful to register previous messages on node
	/// startup.
	fn register_gossip_message(&self, topic: Block::Hash, data: Vec<u8>);

	/// Send a message to a bunch of specific peers, even if they've seen it already.
	fn send_message(&self, who: Vec<network::PeerId>, data: Vec<u8>);

	/// Report a peer's cost or benefit after some action.
	fn report(&self, who: network::PeerId, cost_benefit: i32);

	/// Inform peers that a block with given hash should be downloaded.
	fn announce(&self, block: Block::Hash);
}

/// Create a unique topic for a round and set-id combo.
pub(crate) fn round_topic<B: BlockT>(round: u64, set_id: u64) -> B::Hash {
	<<B::Header as HeaderT>::Hashing as HashT>::hash(format!("{}-{}", set_id, round).as_bytes())
}

pub fn badger_topic<B: BlockT>() -> B::Hash {
	<<B::Header as HeaderT>::Hashing as HashT>::hash(format!("badger-mushroom").as_bytes())
}


/// Create a unique topic for global messages on a set ID.
pub(crate) fn global_topic<B: BlockT>(set_id: u64) -> B::Hash {
	<<B::Header as HeaderT>::Hashing as HashT>::hash(format!("{}-GLOBAL", set_id).as_bytes())
}
use parity_codec::alloc::collections::BTreeSet;


#[derive(Clone, Debug, PartialEq, Eq,Encode,Decode)]
pub enum LocalTarget 
{
    /// The message must be sent to all remote nodes.
    All,
    /// The message must be sent to the node with the given ID.
    Node(PeerIdW),
    /// The message must be sent to all remote nodes except the passed nodes.
    /// Useful for sending messages to observer nodes that aren't
    /// present in a node's `all_ids()` list.
    AllExcept(BTreeSet<PeerIdW>),
}
/*impl parity_codec::Decode for LocalTarget 
{
  fn decode<I: Input>(value: &mut I) -> Result<Self,parity_codec::Error>
  {
	 Ok(LocalTarget::All)
  }
}*/
/* impl<N> parity_codec::Encode for LocalTarget<N> 
 where N: Encode+Decode
{
	 	fn encode_to<T: parity_codec::Output>(&self, dest: &mut T) 
		 {
		   	
	     }
}

 */

impl From<Target<PeerIdW>> for LocalTarget
{
  fn from(t: Target<PeerIdW>) -> Self {
       match t
	   {
        Target::All => LocalTarget::All,
		Target::Node(n)=> LocalTarget::Node(n),
		Target::AllExcept(set)=>LocalTarget::AllExcept(set)
		//Target::AllExcept(set)=>LocalTarget::AllExcept(set.iter().map(|n| *n).collect())
	   }
    }
}

impl Into<Target<PeerIdW>> for LocalTarget
{
    fn into(self) -> Target<PeerIdW> {
         match self
	   {
        LocalTarget::All=> Target::All,
		LocalTarget::Node(n)=> Target::Node(n),
		LocalTarget::AllExcept(set)=>Target::AllExcept(set)
	   }
    }
}



#[derive(Eq, PartialEq, Debug, Encode, Decode)]
struct SourcedMessage<D: ConsensusProtocol> 
where D::NodeId: Encode+Decode+Clone,
{
    sender_id: D::NodeId,
    target: LocalTarget,
    message: Vec<u8>,
}


#[derive(Clone,Debug,Hash,)]
pub struct PeerIdW( pub PeerId);
impl parity_codec::Encode for PeerIdW {
    fn size_hint(&self) ->usize
     {
		 self.0.as_bytes().len()+8
	 }
	fn encode_to<T: parity_codec::Output>(&self, dest: &mut T) {
		//let len = <C::Affine as CurveAffine>::Compressed::size();
		let buf=self.0.as_bytes();
		let len:u64=buf.len() as u64;
		dest.push(&len);
		 for bt in buf
		 {
			 dest.push_byte(*bt);
		 }
	
	}
}
use parity_codec::Input;
impl parity_codec::Decode for PeerIdW {
  fn decode<I: Input>(value: &mut I) -> Result<Self,parity_codec::Error>
  {
	  let blen:[u8;8];
	   match value.read(&mut blen)
	  {
		  Ok(_) => {},
		  Err(_) => return Err( "Error decoding field PeerIdW".into())
		};
	  let len:u64=Decode::decode(&mut blen.to_vec().as_slice()).unwrap();
	  let mut mt:Vec<u8>= Vec::with_capacity(len as usize);

	  match value.read(&mut mt)
	  {
		  Ok(_) => {},
		  Err(_) => return Err( "Error decoding field PeerIdW".into())
		}
     let pw= match PeerId::from_bytes(mt)
	 {
		 Ok(res) => res,
		 Err(_) => return Err( "Error decoding field PeerIdW".into())
	 };
	 Ok(PeerIdW{0:pw})
  }
}



use crate::serde::ser::SerializeSeq;
use serde::Serializer;
impl Serialize for PeerIdW
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
		let e=self.0.as_bytes();

        let mut seq = serializer.serialize_seq(Some(e.len()))?;
        for s in e {
            seq.serialize_element(s)?;
        }
        seq.end()
    }
}

struct PeerIdVisitor;
use std::fmt;
use serde::de::Visitor;
use serde::de::SeqAccess;
use serde::de::Deserializer;
use crate::serde::de::Error as SerdeError;

impl<'de> Visitor<'de> for PeerIdVisitor
{
    // The type that our Visitor is going to produce.
    type Value = PeerIdW;

    // Format a message stating what data this Visitor expects to receive.
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("sequence of bytes in PeerId shape")
    }

    // Deserialize MyMap from an abstract "map" provided by the
    // Deserializer. The MapAccess input is a callback provided by
    // the Deserializer to let us see each entry in the map.
    fn visit_seq<M>(self, mut access: M) -> Result<Self::Value, M::Error>
    where
        M: SeqAccess<'de>,
    {
        let mut buf = Vec::<u8>::with_capacity(access.size_hint().unwrap_or(0));

        // While there are entries remaining in the input, add them
        // into our map.
        while let Some( value) = access.next_element()? {
           buf.push(value)
        }
        match PeerId::from_bytes(buf)
		{
			Ok(res) =>Ok(PeerIdW{0:res}),
			Err(_) => Err(M::Error::custom("Peerid frombytes failed"))
		}     
    }
}
impl<'de> Deserialize<'de> for PeerIdW {
    fn deserialize<D>(deserializer: D) -> Result<PeerIdW, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(PeerIdVisitor)
    }
}


impl rand::distributions::Distribution<PeerIdW> for rand::distributions::Standard
{
fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> PeerIdW
	{
		let hash=multihash::Hash::SHA2256;
	let mut buf = encode::u16_buffer();
        let code = encode::u16(hash.code(), &mut buf);

        let header_len = code.len() + 1;
        let size = hash.size();

        let mut output = Vec::new();
        output.resize(header_len + size as usize, 0);
        output[..code.len()].copy_from_slice(code);
        output[code.len()] = size;

        for b in output[header_len..].iter_mut() {
            *b = rng.gen();
        }

        let mhash= Multihash {
            bytes:  Bytes::from(output),}	;
		PeerIdW {0:PeerId {multihash:mhash}}	
	}
}
use std::convert::TryInto;
use bytes::Bytes;
impl rand::distributions::Distribution<PeerIdW> for PeerIdW
{
	fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> PeerIdW
	{
		let hash=multihash::Hash::SHA2256;
	let mut buf = encode::u16_buffer();
        let code = encode::u16(hash.code(), &mut buf);

        let header_len = code.len() + 1;
        let size = hash.size();

        let mut output = Vec::new();
        output.resize(header_len + size as usize, 0);
        output[..code.len()].copy_from_slice(code);
        output[code.len()] = size;

        for b in output[header_len..].iter_mut() {
            *b = rng.gen();
        }

        let mhash= Multihash {
            bytes:  Bytes::from(output),}	;
		PeerIdW {0:PeerId {multihash:mhash}}	
	}
}

impl From<PeerId> for PeerIdW {
    fn from(id: PeerId) -> Self {
       PeerIdW(id)
    }
}

impl Into<PeerId> for PeerIdW {
    fn into(self) -> PeerId {
        self.0
    }
}
impl std::cmp::PartialOrd for PeerIdW
{
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::cmp::PartialEq for PeerIdW {
    fn eq(&self, other: &Self) -> bool {
        self.0==other.0
    }
}

impl std::cmp::Eq for PeerIdW {}

impl std::cmp::Ord for PeerIdW
{
	fn  cmp(&self, other: &Self) -> std::cmp::Ordering
	{
     self.0.to_base58().cmp(&other.0.to_base58())
	}
}

pub type BadgerTransaction = Vec<u8>;
pub type QHB = SenderQueue<QueueingHoneyBadger<BadgerTransaction, PeerIdW, Vec<BadgerTransaction>>>;



pub struct BadgerNode<B: BlockT,D> 
where 
D: ConsensusProtocol<NodeId=PeerIdW>, //specialize to avoid some of the confusion
D::Message: Serialize + DeserializeOwned,
{
    /// This node's own ID.
    id: PeerId,
    /// The instance of the broadcast algorithm.
    algo: D,

	main_rng:OsRng,

	peers: Peers,
	authorities: Vec<AuthorityId>,
	config: crate::Config,
	//next_rebroadcast: Instant,
	/// Incoming messages from other nodes that this node has not yet handled.
    in_queue: VecDeque<SourcedMessage<D>>,
    /// Outgoing messages to other nodes.
    out_queue: VecDeque<SourcedMessage<D>>,
    /// The values this node has output so far, with timestamps.
    outputs: Vec<D::Output>,
	phantom: PhantomData<B>

}

impl<B: BlockT,D: ConsensusProtocol<NodeId=PeerIdW> > BadgerNode<B,D>
where D::Message: serde::Serialize + DeserializeOwned
{
fn register_peer_public_key(&mut self,who :&PeerId, auth:AuthorityId)
 {
  self.peers.update_id(who,auth)
 }
fn push_transaction(&mut self,tx: Vec<u8>) -> Result<CpStep<D>>
{
algo.push_transaction(tx,&self.rng)
}
 fn is_authority(&self, who :&PeerId) -> bool
  {
	  let auth=self.peers.peer(who);
	  match auth
	  {
		  Some(info) =>
		  {
			  if let Some(iid) =info.id
			  {
			   self.authorities.contains(&iid)
			  }
			  else
			  {
                false
			  }

		  },
           None => false
	  }  
  }
}



pub type BadgerNodeStepResult<D> = CpStep<D>;
pub type TransactionSet = Vec<Vec<u8>>; //agnostic?
impl<B: BlockT,D: ConsensusProtocol<NodeId=PeerIdW> > BadgerNode<B,D>
where D::Message: serde::Serialize + DeserializeOwned
{
	pub fn new(config: crate::Config, self_id:PeerId) -> BadgerNode<B,QHB>
	{
	let mut rng=OsRng::new().unwrap();
	let secr=match config.secret_key_share
	{
		Some(wrap) => Some(wrap.0.clone()),
		None => None
	};
	let ni=NetworkInfo::<D::NodeId>::new(PeerIdW{ 0: self_id.clone() },secr,config.public_key_set.0.clone(),config.node_id.1.clone(),config.initial_validators.clone());
	let peer_ids: Vec<_> = ni
            .all_ids()
            .filter(|&&them| them != PeerIdW{ 0: self_id} )
            .cloned()
            .collect();
    let dhb = DynamicHoneyBadger::builder().build(ni);
	let (qhb, qhb_step) = QueueingHoneyBadger::builder(dhb)
            .batch_size(config.batch_size.try_into().unwrap())
            .build(&mut rng).expect("instantiate QueueingHoneyBadger");

	 let (sq, mut step) = SenderQueue::builder(qhb, peer_ids.into_iter()).build(PeerIdW{0:self_id.clone()});
     let output = step.extend_with(qhb_step, |fault| fault, BMessage::from);
	 assert!(output.is_empty());
     let out_queue = step
            .messages
            .into_iter()
            .map(|msg| {
                let ser_msg = bincode::serialize(&msg.message).expect("serialize");
                SourcedMessage {
                    sender_id: sq.our_id().clone(),
                    target: msg.target.into(),
                    message: ser_msg,
                }
            }).collect();
	 let outputs = step
            .output
            .into_iter()
            .collect();
     let mut node=BadgerNode {
		 id: self_id,
		 algo: sq,
         main_rng: rng,
         peers:  Peers::new(),
		 authorities : config.initial_validators.clone().iter().map(|(_,val)| PublicKeyWrap{0:*val} ).collect(),
		 config: config.clone(),
         in_queue:  VecDeque::new(),
		    out_queue: out_queue,
			outputs:outputs,
			phantom: PhantomData
	 };
	 	for (k,v) in config.initial_validators.clone()
		 {
			 node.register_peer_public_key(&k.0,PublicKeyWrap{0:v})
		 }
		 node 
	}

	pub fn  handle_message(&mut self, who: &PeerIdW, msg:  D::Message) -> Result<(),&'static str>
	{
		match   self.algo.handle_message(who, msg, &mut self.main_rng) 
		{
			Ok(step) => 
			 {
              let out_msgs: Vec<_> = step
            .messages
            .into_iter()
            .map(|mmsg| {
                let ser_msg = bincode::serialize(&mmsg.message).expect("serialize");
                (mmsg.target, ser_msg)
                }).collect();
		    self.outputs.extend(step.output.into_iter());	
			    for (target, message) in out_msgs 
				{
                 self.out_queue.push_back(SourcedMessage {
                   sender_id: PeerIdW{0: self.id.clone()},
                   target: target.into(),
                   message,});
				}
				
				Ok(())
			 }
			Err(_) => return Err("Cannot handle message")
		}
	}
}
use parking_lot::RwLock;
pub(super) struct BadgerGossipValidator<Block: BlockT>
 {
	inner: parking_lot::RwLock<BadgerNode<Block,QHB>>,
}
impl<Block: BlockT> BadgerGossipValidator<Block> 
{
	   fn flush_messages(&self,context: &mut dyn ValidatorContext<Block>)
   {
    let mut locked= self.inner.write();
	let topic = badger_topic::<Block>();    
		 for msg in locked.out_queue.drain(..)
		 {
			 let vdata=GossipMessage::BadgerData(msg.message).encode();
			 match &msg.target
			 {
				 LocalTarget::All  => context.broadcast_message(topic,vdata,true), 
				LocalTarget::Node(to_id) => 
				  {
				  context.send_message(&to_id.0, vdata);

				  },
              LocalTarget::AllExcept(exclude) => {
				    let mut inner = self.inner.write();
                    for pid in inner.peers.peer_list().iter().filter(|n| !exclude.contains(&PeerIdW{0:*(*n) })) {
						let tmp=PeerIdW {0:pid.clone() } ;
                        if tmp != msg.sender_id {
                            context.send_message(pid, vdata.clone());
                        }
                    }
                   }

			 }
			
		 }
   }
fn flush_message_net<N:Network<Block>>(&self,context: &N)
   {
    let locked= self.inner.write();
	let topic = badger_topic::<Block>();    
		 for msg in locked.out_queue.drain(..)
		 {
			 let vdata=GossipMessage::BadgerData(msg.message).encode();
			 match &msg.target
			 {
				 LocalTarget::All  => context.gossip_message(topic,vdata,true), 
				LocalTarget::Node(to_id) => 
				  {
				  context.send_message(vec![to_id.0.clone()], vdata);

				  },
              LocalTarget::AllExcept(exclude) => {
				    let mut inner = self.inner.write();
                    for pid in inner.peers.peer_list().iter().filter(|n| !exclude.contains(&PeerIdW{0:(*n).clone() })) {
						let tmp=PeerIdW {0:*pid } ;
                        if tmp != msg.sender_id {
                            context.send_message(vec![pid.clone()], vdata.clone());
                        }
                    }
                   }

			 }
			
		 }
   }
	/// Create a new gossip-validator. 
	pub(super) fn new(config: crate::Config, self_id:PeerId)
		-> BadgerGossipValidator<Block>
	{
		let val = BadgerGossipValidator {
			inner: parking_lot::RwLock::new(BadgerNode::<Block,QHB>::new(config,self_id)),
		};

		val
	}
    /// collect outputs from 
    pub fn pop_output(&self) -> Option<<QHB as ConsensusProtocol>::Output>
	{
      let mut locked = self.inner.write();
	  locked.outputs.pop()
	}

    
	pub  fn is_validator(&self) ->bool
	{
		let rd = self.inner.read();
		rd.is_authority(&rd.id)
	}
    pub fn push_transaction<N:Network<Block>>(&self,tx: Vec<u8>,net: &N) ->Result<(),Error>
	{
       let mut locked=self.inner.write();
	   match locked.push_transaction(tx)
	   {
		   Ok(step) => {
			let out_msgs: Vec<_> = step
             .messages
             .into_iter()
             .map(|mmsg| {
                let ser_msg = bincode::serialize(&mmsg.message).expect("serialize");
                (mmsg.target, ser_msg)
                }).collect();
		    locked.outputs.extend(step.output.into_iter());	
			    for (target, message) in out_msgs 
				{
                 locked.out_queue.push_back(SourcedMessage {
                   sender_id: PeerIdW{0:self.inner.read().id.clone()},
                   target: target.into(),
                   message,});
				}
				//send messages out
				self.flush_message_net(net);
				Ok(())
		   },
		   Err(e) => return Err(Error::Badger(e.to_string()))
	   }

	}

	pub(super) fn do_validate(&self, who: &PeerId, mut data: &[u8])
		-> (Action<Block::Hash>,  Option<GossipMessage>)
	{

		let mut peer_reply = None;


		let action = {
			match GossipMessage::decode(&mut data) {
					Ok(GossipMessage::Greeting(msg))  =>
					{
						use badger::crypto::Signature;
						
						if  msg.myId.0.verify(&msg.mySig.0,msg.myId.0.to_bytes().to_vec())
						 {
							self.inner.write().register_peer_public_key(who,msg.myId);
							Action::Keep()
						 } 
						 else
						 {
						 Action::Discard(-1)
						 }
					},
					Ok(GossipMessage::RequestGreeting) =>
					{
						let rd=self.inner.read();

						let msrep=GreetingMessage { myId: PublicKeyWrap{0:rd.config.node_id.0.clone()},mySig: SignatureWrap { 0: rd.config.node_id.1.sign(rd.config.node_id.0.clone().to_bytes().to_vec())} } ;
                    peer_reply = Some(GossipMessage::Greeting(msrep));
					Action::ProcessAndDiscard()
					},
					Ok(GossipMessage::BadgerData(badger_msg)) => 
					{
						
					let mut locked=self.inner.write();
					if locked.is_authority(who) 
					 {
						if let Ok(msg) = bincode::deserialize::<<QHB as ConsensusProtocol>::Message>(&badger_msg)
						{
						match locked.handle_message(&PeerIdW{0:who.clone()} ,msg)
						{
							Ok(_) => 
							{
                            //send is handled separately. trigger propose? or leave it for stream
                            Action::ProcessAndDiscard() 
							}
							Err(e) =>
							{
							telemetry!(CONSENSUS_DEBUG; "afg.err_handling_msg"; "err" => ?format!("{}", e));
							Action::Discard(-1)
							}
						}
						}
						else
						{
							Action::Discard(-1)
						}
					 } 
					else
					 { 
						Action::Discard(-1) 
					 }
					},

				Err(_) => {
					debug!(target: "afg", "Error decoding message");
					telemetry!(CONSENSUS_DEBUG; "afg.err_decoding_msg"; "" => "");

					let len = std::cmp::min(i32::max_value() as usize, data.len()) as i32;
					Action::Discard(-len)
				}
			}
		};

		(action,  peer_reply)
	}
}


impl<Block: BlockT > network_gossip::Validator<Block> for BadgerGossipValidator<Block> 
{
	fn new_peer(&self, context: &mut dyn ValidatorContext<Block>, who: &PeerId, _roles: Roles) 
	{
		{
			let mut inner = self.inner.write();
			inner.peers.new_peer(who.clone());
		};
		let packet = {
			let mut inner = self.inner.write();
			inner.peers.new_peer(who.clone());
            GreetingMessage
			{
				myId: PublicKeyWrap{ 0 : inner.config.node_id.0},
				mySig: SignatureWrap {0: inner.config.node_id.1.sign(inner.config.node_id.0.clone().to_bytes().to_vec())}
			}
		};

		//if let Some(packet) = packet {
			let packet_data = GossipMessage::from(packet).encode();
			context.send_message(who, packet_data);
		//}
	}

	fn peer_disconnected(&self, _context: &mut dyn ValidatorContext<Block>, who: &PeerId) 
	{
		self.inner.write().peers.peer_disconnected(who);
	}

	fn validate(&self, context: &mut dyn ValidatorContext<Block>, who: &PeerId, data: &[u8])
		-> network_gossip::ValidationResult<Block::Hash>
	{
		let (action,  peer_reply) = self.do_validate(who, data);
        let topic = badger_topic::<Block>();
		// not with lock held!
		if let Some(msg) = peer_reply {
			context.send_message(who, msg.encode());
		}
		context.send_topic(who, topic, false);

		{
        self.flush_messages(context);
		}
		
		match action {
			Action::Keep() => {
				context.broadcast_message(topic, data.to_vec(), false);
				network_gossip::ValidationResult::ProcessAndKeep(topic)
			}
			Action::ProcessAndDiscard() => {
				
				network_gossip::ValidationResult::ProcessAndDiscard(topic)
			}
			Action::Discard(cb) => {
				//self.report(who.clone(), cb);
				network_gossip::ValidationResult::Discard
			}
			Action::Useless(_) => { 	network_gossip::ValidationResult::Discard}
		}
	}

	fn message_allowed<'b>(&'b self) //todo: not sure what this is for
		-> Box<dyn FnMut(&PeerId, MessageIntent, &Block::Hash, &[u8]) -> bool + 'b>
	{
		let (inner, do_rebroadcast) = {
			use parking_lot::RwLockWriteGuard;

			let mut inner = self.inner.write();
			let now = Instant::now();
			let do_rebroadcast = false;
			
			/*if now >= inner.next_rebroadcast {
				inner.next_rebroadcast = now + REBROADCAST_AFTER;
				true
			} else {
				false
			};*/

			// downgrade to read-lock.
			(RwLockWriteGuard::downgrade(inner), do_rebroadcast)
		};

		Box::new(move |who, intent, topic, mut data| {
			if let MessageIntent::PeriodicRebroadcast = intent {
				return false; //rebroadcast not needed, I hope?
			}

			let peer = match inner.peers.peer(who) {
				None => return false,
				Some(x) => x,
			};

            if *topic!= badger_topic::<Block>() //only one topic, i guess we may add epochs eventually
			{
				return false;
			}
			// if the topic is not something we're keeping at the moment,
			// do not send.
	
			match GossipMessage::decode(&mut data) {
				Err(_) => false,
				Ok(GossipMessage::BadgerData(_)) => {
					return  false
				}
				Ok(GossipMessage::Greeting(_)) => true,
				Ok(GossipMessage::RequestGreeting) => false,
			}
		})
	}

	fn message_expired<'b>(&'b self) -> Box<dyn FnMut(Block::Hash, &[u8]) -> bool + 'b> {
		let inner = self.inner.read();
		Box::new(move |topic, mut data| {
			// if the topic is not one of the ones that we are keeping at the moment,
			// it is expired.
			if topic!= badger_topic::<Block>() //only one topic, i guess we may add epochs eventually
			{
				return true;
			}

			match GossipMessage ::decode(&mut data) {
				Err(_) => true,
				Ok(GossipMessage::Greeting(_))
					=> false,
				Ok(_) => true,
			}
		})
	}
}




impl<B, S, H> Network<B> for Arc<NetworkService<B, S, H>> where
	B: BlockT,
	S: network::specialization::NetworkSpecialization<B>,
	H: network::ExHashT,
{
	type In = NetworkStream;

	fn messages_for(&self, topic: B::Hash) -> Self::In {
		let (tx, rx) = futures03::channel::oneshot::channel::<futures03::channel::mpsc::UnboundedReceiver<_>>();
		self.with_gossip(move |gossip, _| {
			let inner_rx = gossip.messages_for(HBBFT_ENGINE_ID, topic);
			let _ = tx.send(inner_rx);
		});
		NetworkStream { outer: rx, inner: None }
	}

	fn register_validator(&self, validator: Arc<dyn network_gossip::Validator<B>>) {
		self.with_gossip(
			move |gossip, context| gossip.register_validator(context, HBBFT_ENGINE_ID, validator)
		)
	}

	fn gossip_message(&self, topic: B::Hash, data: Vec<u8>, force: bool) {
		let msg = ConsensusMessage {
			engine_id: HBBFT_ENGINE_ID,
			data,
		};

		self.with_gossip(
			move |gossip, ctx| gossip.multicast(ctx, topic, msg, force)
		)
	}

	fn register_gossip_message(&self, topic: B::Hash, data: Vec<u8>) {
		let msg = ConsensusMessage {
			engine_id: HBBFT_ENGINE_ID,
			data,
		};

		self.with_gossip(move |gossip, _| gossip.register_message(topic, msg))
	}

	fn send_message(&self, who: Vec<network::PeerId>, data: Vec<u8>) {
		let msg = ConsensusMessage {
			engine_id: HBBFT_ENGINE_ID,
			data,
		};

		self.with_gossip(move |gossip, ctx| for who in &who {
			gossip.send_message(ctx, who, msg.clone())
		})
	}

	fn report(&self, who: network::PeerId, cost_benefit: i32) {
		self.report_peer(who, cost_benefit)
	}

	fn announce(&self, block: B::Hash) {
		self.announce_block(block)
	}
}

/// A stream used by NetworkBridge in its implementation of Network.
pub struct NetworkStream {
	inner: Option<mpsc::UnboundedReceiver<network_gossip::TopicNotification>>,
	outer: oneshot::Receiver<futures03::channel::mpsc::UnboundedReceiver<network_gossip::TopicNotification>>
}
use std::{pin::Pin, task::Context, task::Poll};

impl Stream for NetworkStream {
	type Item = network_gossip::TopicNotification;

	fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> 
	{
		if let Some(ref mut inner) = self.inner {
			match  inner.try_next()
			{
				Ok(Some(opt)) => return futures03::Poll::Ready(Some(opt)),
				Ok(None) => return futures03::Poll::Pending,
				Err(_) => return futures03::Poll::Pending,
			};
		}
		match self.outer.try_recv() {
			
		    Ok(Some(mut inner)) => {
				let poll_result = match  inner.try_next()
			{
				Ok(Some(opt)) =>  futures03::Poll::Ready(Some(opt)),
				Ok(None) =>  futures03::Poll::Pending,
				Err(_) =>  futures03::Poll::Pending,
			};
				self.inner = Some(inner);
				poll_result
			},
			Ok(None) => futures03::Poll::Pending,
			Err(_) => futures03::Poll::Pending,
		}
	}
}

/// Bridge between the underlying network service, gossiping consensus messages and Grandpa
pub(crate) struct NetworkBridge<B: BlockT, N: Network<B>> 
where 
{
	service: N,
	node: Arc<BadgerGossipValidator<B>>,
}

impl<B: BlockT, N: PollParameters + Network<B>> NetworkBridge<B, N> 
{
	/// Create a new NetworkBridge to the given NetworkService. Returns the service
	/// handle and a future that must be polled to completion to finish startup.
	/// If a voter set state is given it registers previous round votes with the
	/// gossip service.
	pub(crate) fn new(
		service: N,
		config: crate::Config,
		on_exit:  impl Future<Output = ()> + Clone + Send +Unpin,
	) -> (
		Self,
		impl futures03::future::Future<Output = ()> + Send + Unpin,
	) {

		let validator= BadgerGossipValidator::new(config, service.local_peer_id().clone());
		let validator_arc = Arc::new(validator);
		service.register_validator(validator_arc.clone());



	
		let bridge = NetworkBridge { service, node:validator_arc };

		let startup_work = futures03::future::lazy(move |_| {
			// lazily spawn these jobs onto their own tasks. the lazy future has access
			// to tokio globals, which aren't available outside.
		//	let mut executor = tokio_executor::DefaultExecutor::current();
		//	executor.spawn(Box::new(reporting_job.select(on_exit.clone()).then(|_| Ok(()))))
		//		.expect("failed to spawn grandpa reporting job task");
			()
		});

		(bridge, startup_work)
	}
pub  fn is_validator(&self) ->bool
{
	self.node.is_validator()
}


	/// Set up the global communication streams. blocks out transaction in. Maybe reverse of grandpa... 
	pub(crate) fn global_communication(
		&self,
		is_voter: bool,
	) -> (
		impl Stream<Item = <QHB as ConsensusProtocol>::Output>,
		impl Sink<TransactionSet>,
	)
	 {


		let service = self.service.clone();
		let topic = badger_topic::<B>();
		let incoming = incoming_global::<B,N>( self.node.clone());

		let outgoing = TransactionFeed::<B, N>::new(
			self.service.clone(),
			is_voter,
			self.node.clone(),
		);

	
		(incoming, outgoing)
	}
}

fn incoming_global<B: BlockT, N: Network<B>>(
	gossip_validator: Arc<BadgerGossipValidator<B>>,
) -> impl Stream<Item = <QHB as ConsensusProtocol>::Output> 
{
	BadgerStream::new(gossip_validator.clone())
}

impl<B: BlockT, N: Network<B>> Clone for NetworkBridge<B, N> 
{
	fn clone(&self) -> Self {
		NetworkBridge {
			service: self.service.clone(),
			node: Arc::clone(&self.node),
		}
	}
}



pub struct BadgerStream<Block: BlockT> 
 {
	validator:Arc<BadgerGossipValidator<Block>>,
}


impl<Block:BlockT> BadgerStream<Block>
{
  fn new( gossip_validator: Arc<BadgerGossipValidator<Block>>) ->Self
  {
	  BadgerStream{
		validator:gossip_validator,
	  }

  }
}

impl<Block:BlockT> Stream for BadgerStream <Block>
{
	type Item = <QHB as ConsensusProtocol>::Output;
	
	fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>>
	{
		match self.validator.pop_output()
		{
			Some(data) => Poll::Ready(Some(data)),
			None => Poll::Pending
		}
		
	}
}


/// An output sink for commit messages.
struct TransactionFeed<Block: BlockT, N: Network<Block>> 
{
	network: N,
	is_voter: bool,
	gossip_validator: Arc<BadgerGossipValidator<Block>>,
}

impl<Block: BlockT, N: Network<Block>> TransactionFeed<Block, N> 
{
	/// Create a new commit output stream.
	pub(crate) fn new(
		network: N,
		is_voter: bool,
		gossip_validator: Arc<BadgerGossipValidator<Block>>,
	) -> Self {
		TransactionFeed {
			network,
			is_voter,
			gossip_validator,
		}
	}
}
use network::consensus_gossip::ConsensusGossip;
use runtime_primitives::ConsensusEngineId;

struct NetworkSubtext<'g, 'p, B: BlockT> {
	gossip: &'g mut ConsensusGossip<B>,
	protocol: &'p mut dyn network::Context<B>,
	engine_id: ConsensusEngineId,
}

impl<'g, 'p, B: BlockT> ValidatorContext<B> for NetworkSubtext<'g, 'p, B> {
	/// Broadcast all messages with given topic to peers that do not have it yet.
	fn broadcast_topic(&mut self, topic: B::Hash, force: bool) {
		self.gossip.broadcast_topic(self.protocol, topic, force);
	}

	/// Broadcast a message to all peers that have not received it previously.
	fn broadcast_message(&mut self, topic: B::Hash, message: Vec<u8>, force: bool) {
		self.gossip.multicast(
			self.protocol,
			topic,
			ConsensusMessage{ data: message, engine_id: self.engine_id.clone() },
			force,
		);
	}

	/// Send addressed message to a peer.
	fn send_message(&mut self, who: &PeerId, message: Vec<u8>) {
		self.protocol.send_consensus(who.clone(), ConsensusMessage {
			engine_id: self.engine_id,
			data: message,
		});
	}

	/// Send all messages with given topic to a peer.
	fn send_topic(&mut self, who: &PeerId, topic: B::Hash, force: bool) {
		self.gossip.send_topic(self.protocol, who, topic, self.engine_id, force);
	}
}


impl<Block: BlockT, N: Network<Block>> Sink<Vec<Vec<u8>>> for TransactionFeed<Block, N> 
where 
 {
	type Error = Error;
     
    fn poll_ready( self: Pin<&mut Self>,  cx: &mut Context) -> Poll<Result<(), Self::Error>>
	{
		 Poll::Ready(Ok(()))
	}

	fn start_send(self: Pin<&mut Self>, input:Vec<Vec<u8>>) ->  Result<(), Self::Error>
	{
		
       for tx in input.into_iter().enumerate()
	   {
        let locked=&self.gossip_validator;
		match locked.push_transaction(tx.1,&self.network)
		{
        Ok(_) =>{},
		Err(e) => return Err(e)
		}
   
	   }
	   Ok(())
	}
 fn poll_flush(
        self: Pin<&mut Self>, 
        cx: &mut Context
    ) -> Poll<Result<(), Self::Error>>
	{
		  Poll::Ready(Ok(()))
	}
    fn poll_close(
        self: Pin<&mut Self>, 
        cx: &mut Context
    ) -> Poll<Result<(), Self::Error>>
	{
    Poll::Ready(Ok(()))
	}

}
