
// Copyright 2018-2019 Parity Technologies (UK) Ltd.
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

//! Integration of the GRANDPA finality gadget into substrate.
//!
//! This crate is unstable and the API and usage may change.
//!
//! This crate provides a long-running future that produces finality notifications.
//!
//! # Usage
//!
//! First, create a block-import wrapper with the `block_import` function.
//! The GRANDPA worker needs to be linked together with this block import object,
//! so a `LinkHalf` is returned as well. All blocks imported (from network or consensus or otherwise)
//! must pass through this wrapper, otherwise consensus is likely to break in
//! unexpected ways.
//!
//! # Changing authority sets
//!
//! The rough idea behind changing authority sets in GRANDPA is that at some point,
//! we obtain agreement for some maximum block height that the current set can
//! finalize, and once a block with that height is finalized the next set will
//! pick up finalization from there.
//!
//! Technically speaking, this would be implemented as a voting rule which says,
//! "if there is a signal for a change in N blocks in block B, only vote on
//! chains with length NUM(B) + N if they contain B". This conditional-inclusion
//! logic is complex to compute because it requires looking arbitrarily far
//! back in the chain.
//!
//! Instead, we keep track of a list of all signals we've seen so far (across
//! all forks), sorted ascending by the block number they would be applied at.
//! We never vote on chains with number higher than the earliest handoff block
//! number (this is num(signal) + N). When finalizing a block, we either apply
//! or prune any signaled changes based on whether the signaling block is
//! included in the newly-finalized chain.

extern crate serde;
use serde::Serialize;
use serde::de::DeserializeOwned;  
extern crate unsigned_varint;
use runtime_primitives::traits::Hash as THash;
use runtime_primitives::traits::{NumberFor, Block as BlockT, Header,  ProvideRuntimeApi,BlakeTwo256,};
use runtime_primitives::{generic::{self, BlockId}, Justification,ApplyError};
//use runtime_primitives::{
	//traits::{Block as BlockT, Hash as HashT, Header as HeaderT, ProvideRuntimeApi, DigestFor, },
	//generic::BlockId,
	//ApplyError,
//};
use consensus_common::{evaluation};
//mod aux_schema;
use inherents::{ InherentIdentifier,  };
mod communication;
use communication::NetworkBridge;

use communication::TransactionSet;
use communication::PeerIdW;
//use network::consensus_gossip::{self as network_gossip, MessageIntent, ValidatorContext};
use network::{PeerId};//config::Roles, 
use substrate_telemetry::{telemetry, CONSENSUS_WARN, CONSENSUS_INFO};//CONSENSUS_TRACE, CONSENSUS_DEBUG, 
use futures03::prelude::*;
//use futures03::channel::mpsc;
use futures03::task::Poll;
use futures03::sink::SendAll;
use serde_json as json;
use hex;
use transaction_pool::txpool::{self, Pool as TransactionPool};
use std::{sync::Arc, time::Duration,  marker::PhantomData, hash::Hash, fmt::Debug};//time::Instant, thread,
use client::error::{Error as ClientError, };
use runtime_primitives::traits::DigestFor;
use client::runtime_api::ConstructRuntimeApi;


use service::TelemetryOnConnect;
use client::blockchain::HeaderBackend;
use badger::crypto::{ PublicKey,   SecretKey, };//PublicKeySet,PublicKeyShare,SecretKeyShare,Signature,
use client::Client;
use client::CallExecutor;
use client::backend::Backend;
use std::str::FromStr;
//use client::blockchain::Backend;


use parity_codec::{Encode, Decode, };
use consensus_common::block_import::BlockImport;

use consensus_common::{self,  
	ForkChoiceStrategy, BlockImportParams, BlockOrigin, 
	SelectChain, well_known_cache_keys::{ Id as CacheKeyId}
};//Environment, Proposer,Error as ConsensusError,self,
use consensus_common::import_queue::{
	Verifier, BasicQueue, BoxBlockImport, BoxJustificationImport, BoxFinalityProofImport,
};

use futures03::core_reexport::pin::Pin;
use client::{
	block_builder::api::BlockBuilder as BlockBuilderApi,
	blockchain::ProvideCache,
	//runtime_api::ApiExt,
	error,
	//error::Result as CResult,
	backend::AuxStore,
};

use rand::Rng;
use crate::communication::Network;
use badger::ConsensusProtocol;
use fg_primitives::SecretKeyShareWrap;
use fg_primitives::SecretKeyWrap;
use fg_primitives::PublicKeySetWrap;
use fg_primitives::PublicKeyWrap;
use std::path::PathBuf;
//use fg_primitives::AuthorityId;
use fg_primitives::HbbftApi;
use fg_primitives::AuthorityPair;
//use fg_primitives::AuthoritySignature;
use serde_json::Value::Object;
use serde_json::Value::Number;
use serde_json::Value;
use libp2p::swarm::{ PollParameters};

use std::fs::File;
use inherents::{InherentDataProviders, InherentData};
use bincode;
//use std::fmt; 
use futures03::{future::Future};
use parking_lot::Mutex;
//use tokio_timer::Timeout;
use log::{error, warn, debug, info, trace};

pub use consensus_common::SyncOracle;

use substrate_primitives::{
	Blake2Hasher, H256, ExecutionContext
};



pub const INHERENT_IDENTIFIER: InherentIdentifier = *b"snakeshr";



pub type BadgerImportQueue<B> = BasicQueue<B>;



pub  struct BadgerWorker<C,  I, SO,Inbound,R:Rng,B:BlockT,N: Network<B>,D: ConsensusProtocol<NodeId=PeerIdW>,A> 
where A: txpool::ChainApi,
D::Message: Serialize + DeserializeOwned,
{
	pub client: Arc<C>,
	pub block_import: Arc<Mutex<I>>,
	pub network: NetworkBridge<B,N,D,R>,

	pub transaction_pool: Arc<TransactionPool<A>>,
	pub sync_oracle: SO,
	pub blocks_in:Inbound 
}

pub struct BadgerVerifier<C, Pub,Sig>
where 
Sig: std::marker::Send+std::marker::Sync,
Pub: std::marker::Send+std::marker::Sync
 {
	client: Arc<C>,
	phantom: PhantomData<Pub>,
	phantom2: PhantomData<Sig>,
	inherent_data_providers: inherents::InherentDataProviders,
}

impl<C, Pub,Sig> BadgerVerifier<C, Pub,Sig>
where 
Sig: std::marker::Send+std::marker::Sync,
Pub: std::marker::Send+std::marker::Sync
{
	fn check_inherents<B: BlockT>(
		&self,
		block: B,
		block_id: BlockId<B>,
		inherent_data: InherentData,
		timestamp_now: u64,
	) -> Result<(), String>
		where C: ProvideRuntimeApi, C::Api: BlockBuilderApi<B>
	{
		//empty for now
			Ok(())
		
	}
}

impl<B: BlockT, C, Pub,Sig> Verifier<B> for BadgerVerifier<C, Pub,Sig> where
	C: ProvideRuntimeApi + Send + Sync + client::backend::AuxStore + ProvideCache<B>,
	C::Api: BlockBuilderApi<B> + HbbftApi<B>,
	//DigestItemFor<B>: CompatibleDigestItem<P>,
	Pub: Send + Sync + Hash + Eq + Clone + Decode + Encode + Debug,
	Sig: Encode + Decode+Send+Sync,
{
	fn verify(
		&self,
		origin: BlockOrigin,
		header: B::Header,
		justification: Option<Justification>,
		mut body: Option<Vec<B::Extrinsic>>,
	) -> Result<(BlockImportParams<B>, Option<Vec<(CacheKeyId, Vec<u8>)>>), String> {
        // dummy for the moment
     	let hash = header.hash();
		let parent_hash = *header.parent_hash();
		let import_block = BlockImportParams {
					origin,
					header: header,
					post_digests: vec![],
					body,
					finalized: true,
					justification,
					auxiliary: Vec::new(),
					fork_choice: ForkChoiceStrategy::LongestChain,
				};

				Ok((import_block, None))
		
	}
}




/// Register the aura inherent data provider, if not registered already.
fn register_badger_inherent_data_provider(
	inherent_data_providers: &InherentDataProviders,
	slot_duration: u64,
) -> Result<(), consensus_common::Error> {
	Ok(())
/*	if !inherent_data_providers.has_provider(&hbbft::INHERENT_IDENTIFIER) {
		inherent_data_providers
			.register_provider(srml_aura::InherentDataProvider::new(slot_duration))
			.map_err(Into::into)
			.map_err(consensus_common::Error::InherentData)
	} else {
		Ok(())
	}*/
}

/// Start an import queue for the Badger consensus algorithm.
pub fn badger_import_queue<B, C, Pub,Sig>(
	block_import: BoxBlockImport<B>,
	justification_import: Option<BoxJustificationImport<B>>,
	finality_proof_import: Option<BoxFinalityProofImport<B>>,
	client: Arc<C>,
	inherent_data_providers: InherentDataProviders,
) -> Result<BadgerImportQueue<B>, consensus_common::Error> where
	B: BlockT,
	C: 'static + ProvideRuntimeApi + ProvideCache<B> + Send + Sync + AuxStore,
	C::Api: BlockBuilderApi<B> + HbbftApi<B>,
	//DigestItemFor<B>: CompatibleDigestItem<P>,
	Pub: Clone + Eq + Send + Sync + Hash + Debug + Encode + Decode + AsRef<Pub>,
	Sig: Encode + Decode,
{
	register_badger_inherent_data_provider(&inherent_data_providers, 1)?;
	//initialize_authorities_cache(&*client)?;

	let verifier = Arc::new(
		BadgerVerifier {
			client: client.clone(),
			inherent_data_providers,
			phantom: PhantomData,
			phantom2: PhantomData,
		}
	);
	Ok(BasicQueue::new(
		verifier,
		block_import,
		justification_import,
		finality_proof_import,
	))
}

use std::collections::{BTreeMap, };


const REBROADCAST_AFTER: Duration = Duration::from_secs(60 * 5);
const CATCH_UP_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const CATCH_UP_PROCESS_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum number of rounds we are behind a peer before issuing a
/// catch up request.
const CATCH_UP_THRESHOLD: u64 = 2;

const KEEP_RECENT_ROUNDS: usize = 3;

const BADGER_TOPIC: &str = "itsasnake";



/// Configuration for the Badger service.
#[derive(Clone)]
pub struct Config {
	/// Some local identifier of the node.
	pub name: Option<String>,
	pub num_validators: usize,
	pub secret_key_share: Option<Arc<SecretKeyShareWrap>>,
	pub node_id: Arc<AuthorityPair>,
	pub public_key_set: Arc<PublicKeySetWrap>,
	pub batch_size:u32,
    pub initial_validators: BTreeMap<PeerIdW, PublicKey>,
}
fn secret_share_from_string(st:&str) ->Result<SecretKeyShareWrap,Error>
{
let data=hex::decode(st)?;
match bincode::deserialize(&data)
 {
  Ok(val) => Ok(SecretKeyShareWrap { 0: val}),
  Err(_)  => return Err(Error::Badger("secret key share binary invalid".to_string()))
 }
}
fn secret_from_string(st:&str) ->Result<SecretKeyWrap,Error>
{
let data=hex::decode(st)?;
match bincode::deserialize(&data)
 {
  Ok(val) => Ok(SecretKeyWrap { 0: val}),
  Err(_)  => return Err(Error::Badger("secret key binary invalid".to_string()))
 }
}
impl Config {
	fn name(&self) -> &str {
		self.name.as_ref().map(|s| s.as_str()).unwrap_or("<unknown>")
	}
	pub fn from_json_file_with_name(path: PathBuf, name: &str) -> Result<Self, String> {
		let file = File::open(&path).map_err(|e| format!("Error opening config file: {}", e))?;
		let spec :serde_json::Value = json::from_reader(file).map_err(|e| format!("Error parsing spec file: {}", e))?;
		let nodedata= match spec["nodes"]
		   {
           Object(map) =>
		    {
		      match map.get(name)
			  {
				  Some(dat) =>dat,
				  None => return Err("Could not find node name".to_string()),
			  }
      
		    }
		    _ =>  return Err("Nodes object should be present".to_string()),
		   };  

        let ret = Config
		{
         name: Some(name.to_string()),
         num_validators: match spec["num_validators"]
		   {
			   Number(x) => match x.as_u64() {Some(y)=> y as usize, None => return Err("Invalid num_validators".to_string())},
               Value::String(st) => match st.parse::<usize>()
			     {
					 Ok(v) =>v,
					 Err(_) => return Err("Invalid num_validators".to_string())
				 },
			   _ => return Err("Invalid num_validators".to_string())
		   },
		 secret_key_share: match nodedata["secret_key_share"]
		            {
                      Value::String(st) => {
						    Some(Arc::new( match secret_share_from_string(&st) {
							                 Ok(val) =>val, Err(_) => return Err("secret_key_share".to_string()) } ))
						   },
					  _ =>  None,
					}, 
		 node_id: match nodedata["node_id"]
                  {
					  Object(nid) => {
						  let pub_key:PublicKey=match nid["pub"]
						           {
                                    Value::String(st) => 
									{
									let data= match hex::decode(st)
									 {
										 Ok(val) =>val,
										 Err(_) =>return Err("Hex error in pub".to_string())
									 };
									match bincode::deserialize(&data)
									{
									Ok(val) => val,
									Err(_)  => return Err("public key binary invalid".to_string())
									}
									}
									_ => return Err("pub key not string".to_string()),
								   };
						 let sec_key:SecretKey=match nid["priv"]
						           {
                                    Value::String(st) => 
									{
									 let data= match hex::decode(st)
									 {
										 Ok(val) =>val,
										 Err(_) =>return Err("Hex error in priv".to_string())
									 };
									 match bincode::deserialize(&data)
									 {
									 Ok(val) => val,
									 Err(_)  => return Err("secret key binary invalid".to_string())
									 }
									}
									_ => return Err("priv key not string".to_string()),
								   };
								   Arc::new((pub_key,sec_key))
					  },
					  _ => return Err(  "node id not pub/priv object".to_string())

				  },
		public_key_set: match spec["public_key_set"]
		                {
                           Value::String(st) => 
									{
									let data=match hex::decode(st)
									 {
										 Ok(val) =>val,
										 Err(_) =>return Err("Hex error in public_key_set".to_string())
									 };
									match bincode::deserialize(&data)
									{
									Ok(val) => Arc::new(PublicKeySetWrap{0: val}),
									Err(_)  => return Err( "public key set binary invalid".to_string())
									}
									}
									_ => return Err("pub key set not string".to_string()),
						},
		batch_size : 	match spec["batch_size"]
		   {
			   Number(x) => match x.as_u64() {Some(y)=> y as u32, None => return Err("Invalid batch_size".to_string())},
               Value::String(st) => match st.parse::<u32>(){Ok(val)=>val,Err(_)=>return Err("batch_size parsing error".to_string())},
			   _ => return Err(  "Invalid batch_size".to_string()) ,
		   },
		  
		  initial_validators : match spec["initial_validators"]
		   {
            Object(dict) => {
				let mut ret=BTreeMap::<PeerIdW, PublicKey>::new();
						for (k,v) in dict.iter()
					{
						let peer=match PeerId::from_str(k)
						   {
							   Ok(val) => val,
							   Err(_) => return Err(  "Invalid PeerId".to_string()) 
						   };
						let data= match hex::decode(v)
						{
							Ok(val) =>val,
							Err(_) =>return Err("Hex error in pubkey".to_string())
						};
						let pubkey :PublicKeyWrap =  match bincode::deserialize(&data)
							{
							Ok(val) => val,
							Err(_)  => return Err("public key binary invalid".to_string())
							};
                      ret.insert(PeerIdW{0:peer},pubkey.0);
					}

				ret
			},
			_ =>return Err(  "Invalid initial_validators, should be object".to_string())

		   }				  
		};
		Ok(ret)
	}
}

/// Errors that can occur while voting in BADGER.
#[derive(Debug)]
pub enum Error {
	/// An error within badger.
	Badger(String),
	/// A network error.
	Network(String),
	/// A blockchain error.
	Blockchain(String),
	/// Could not complete a round on disk.
	Client(ClientError),
	/// An invariant has been violated (e.g. not finalizing pending change blocks in-order)
	Safety(String),
	/// A timer failed to fire.
	Timer(tokio_timer::Error),
}

impl From<hex::FromHexError> for Error
{
  fn from(e: hex::FromHexError) -> Self {
		Error::Badger(e.to_string())
	}
}
impl From<ClientError> for Error {
	fn from(e: ClientError) -> Self {
		Error::Client(e)
	}
}

/// Something which can determine if a block is known.
pub trait BlockStatus<Block: BlockT> {
	/// Return `Ok(Some(number))` or `Ok(None)` depending on whether the block
	/// is definitely known and has been imported.
	/// If an unexpected error occurs, return that.
	fn block_number(&self, hash: Block::Hash) -> Result<Option<NumberFor<Block>>, Error>;
}
pub trait BlockNumberOps:
	std::fmt::Debug +
	std::cmp::Ord +
	std::ops::Add<Output=Self> +
	std::ops::Sub<Output=Self> +
	num::One +
	num::Zero +
	num::AsPrimitive<usize>
{}

impl<T> BlockNumberOps for T where
	T: std::fmt::Debug,
	T: std::cmp::Ord,
	T: std::ops::Add<Output=Self>,
	T: std::ops::Sub<Output=Self>,
	T: num::One,
	T: num::Zero,
	T: num::AsPrimitive<usize>,
{}

impl<B, E, Block: BlockT<Hash=H256>, RA> BlockStatus<Block> for Arc<Client<B, E, Block, RA>> where
	B: Backend<Block, Blake2Hasher>,
	E: CallExecutor<Block, Blake2Hasher> + Send + Sync,
	RA: Send + Sync,
	NumberFor<Block>: BlockNumberOps,
{
	fn block_number(&self, hash: Block::Hash) -> Result<Option<NumberFor<Block>>, Error> {
		self.block_number_from_id(&BlockId::Hash(hash))
			.map_err(|e| Error::Blockchain(format!("{:?}", e)))
	}
}




pub struct LinkHalf<B, E, Block: BlockT<Hash=H256>, RA, SC> {
	client: Arc<Client<B, E, Block, RA>>,
	select_chain: SC,
	//persistent_data: PersistentData<Block>,
	//voter_commands_rx: mpsc::UnboundedReceiver<VoterCommand<Block::Hash, NumberFor<Block>>>,
}

/// Make block importer and link half necessary to tie the background voter
/// to it.
/* pub fn block_import<B, E, Block: BlockT<Hash=H256>, RA, PRA, SC>(
	client: Arc<Client<B, E, Block, RA>>,
	api: Arc<PRA>,
	select_chain: SC,
) -> Result<(
		GrandpaBlockImport<B, E, Block, RA, PRA, SC>,
		LinkHalf<B, E, Block, RA, SC>
	), ClientError>
where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + 'static + Clone + Send + Sync,
	RA: Send + Sync,
	PRA: ProvideRuntimeApi,
	PRA::Api: HbbftApi<Block>,
	SC: SelectChain<Block>,
{


	let chain_info = client.info();
	let genesis_hash = chain_info.chain.genesis_hash;

	/* let persistent_data = aux_schema::load_persistent(
		#[allow(deprecated)]
		&**client.backend(),
		genesis_hash,
		<NumberFor<Block>>::zero(),
		|| {
			let genesis_authorities = api.runtime_api()
				.badger_authorities(&BlockId::number(Zero::zero()))?;
			telemetry!(CONSENSUS_DEBUG; "afg.loading_authorities";
				"authorities_len" => ?genesis_authorities.len()
			);
			Ok(genesis_authorities)
		}
	)?; */

	let (voter_commands_tx, voter_commands_rx) = mpsc::unbounded();

	Ok((
		BadgerBlockImport::new(
			client.clone(),
			select_chain.clone(),
			persistent_data.authority_set.clone(),
			voter_commands_tx,
			persistent_data.consensus_changes.clone(),
			api,
		),
		LinkHalf {
			client,
			select_chain,
			persistent_data,
			voter_commands_rx,
		},
	))
} */

fn global_communication<Block: BlockT<Hash=H256>, B, E, N, RA,D, R>(
	client: &Arc<Client<B, E, Block, RA>>,
	network: &NetworkBridge<Block, N,D,R>,
) -> (
		impl Stream<Item = D::Output>,
		impl Sink<TransactionSet>,
) where
	B: Backend<Block, Blake2Hasher>,
	E: CallExecutor<Block, Blake2Hasher> + Send + Sync,
	N: Network<Block>+PollParameters,
	RA: Send + Sync,
	NumberFor<Block>: BlockNumberOps,
	D: ConsensusProtocol<NodeId=PeerIdW>,
	D::Output: Send+Sync,
	R: Rng +Send+Sync,
	D::Message: Serialize+DeserializeOwned,
{

	let is_voter = network.is_validator();

	// verification stream
	let (global_in, global_out) = network.global_communication(
	//	voters.clone(),
		is_voter,
	);

	// block commit and catch up messages until relevant blocks are imported.
/*let global_in = UntilGlobalMessageBlocksImported::new(
		client.import_notification_stream(),
		client.clone(),
		global_in,
	);*/ //later

	(global_in, global_out)
}



/// Parameters used to run Grandpa.
pub struct BadgerStartParams< Block: BlockT<Hash=H256>, N :Network<Block>,  X> 

{
	/// Configuration for the GRANDPA service.
	pub config: Config,
	/// A link to the block import worker.
	//pub link: LinkHalf<B, E, Block, RA, SC>,
	/// The Network instance.
	pub network: N,
	/// The inherent data providers.
	pub inherent_data_providers: InherentDataProviders,
	/// Handle to a future that will resolve on exit.
	pub on_exit: X,
	/// If supplied, can be used to hook on telemetry connection established events.
	pub telemetry_on_connect: Option<TelemetryOnConnect>,
	ph: PhantomData<Block>
}

pub struct TxStream<A>
where A: txpool::ChainApi
{
 pub transaction_pool: Arc<TransactionPool<A>>,
}

impl<A> Stream for TxStream<A>
where A: txpool::ChainApi
{
	type Item=TransactionSet;
	fn poll_next(
        self: Pin<&mut Self>, 
        cx: &mut futures03::task::Context
    ) -> Poll<Option<Self::Item>>
	{
     let pending_iterator = self.transaction_pool.ready();
	  let batch:Vec<_>=pending_iterator.into_iter().map(|a| a.encode()).collect();
	  if batch.len()==0
	  {
		  return Poll::Pending;
	  }
	  Poll::Ready(Some(batch))
	}
}

pub struct BadgerProposerWorker<D: ConsensusProtocol<NodeId=PeerIdW>, S, N, Block:BlockT<Hash=H256>, TF,A,I,B,E,RA>
where
S : Stream<Item = D::Output> ,
TF: Sink<TransactionSet>+Unpin,
A: txpool::ChainApi,
B: client::backend::Backend<Block, Blake2Hasher> + Send + Sync + 'static,
E: CallExecutor<Block, Blake2Hasher> + Send + Sync + Clone + 'static,
RA: ConstructRuntimeApi<Block, Client<B, E, Block, RA>>,
//N : Network<Block> + Send + Sync + 'static,
NumberFor<Block>: BlockNumberOps,
I:BlockImport<Block>
{
 pub block_out: S,
 pub transaction_in: TF,
 pub transaction_pool: Arc<TransactionPool<A>>,
 pub network: N,
 pub client: Arc<Client<B, E, Block, RA>>,
 pub block_import: Arc<Mutex<I>>,
 pub inherent_data_providers: InherentDataProviders,
 ph:PhantomData<D>,
 phb:PhantomData<Block>,

}
impl<'a,D:ConsensusProtocol<NodeId=PeerIdW>+'a,S: Stream<Item = D::Output> +'a ,N:'a,Block:BlockT<Hash=H256>,TF: Sink<TransactionSet>+Unpin,A: txpool::ChainApi,I:BlockImport<Block>+'a,B,E,RA:'a>   BadgerProposerWorker<D,S,N,Block,TF,A,I,B,E,RA>
where
NumberFor<Block>: BlockNumberOps,
Client<B, E, Block, RA>: ProvideRuntimeApi,
<Client<B, E, Block, RA> as ProvideRuntimeApi>::Api: BlockBuilderApi<Block>,
B: client::backend::Backend<Block, Blake2Hasher> + Send + Sync + 'static,
E: CallExecutor<Block, Blake2Hasher> + Send + Sync + Clone + 'static,
RA: ConstructRuntimeApi<Block, Client<B, E, Block, RA>>,
{
	pub fn make_sink(& mut self) -> SendAll<TF, TxStream<A>> 
	{
		self.transaction_in.send_all(TxStream{transaction_pool:self.transaction_pool.clone()})
	}

	pub fn make_block_spitter (&mut self) -> impl Future + '_
	{
		Box::pin( self.block_out.for_each(move |batch|
		  {
			let inherent_data = match self.inherent_data_providers.create_inherent_data() {
				Ok(id) => id,
				Err(err) => return future::ready(()),//future::err(err),
			};
			//empty for now?
			let inherent_digests= generic::Digest {
							logs: vec![],
						};
           	if let Ok(imp_blocks) =self.make_import_blocks(&batch, inherent_data,inherent_digests)
			   {
			  for import_block in imp_blocks.drain(..)
			  { 
			if let Err(e) = self.block_import.lock().import_block(import_block, Default::default()) {
				warn!(target: "badger", "Error with block built on {:?}: {:?}",
						import_block.header.parent_hash().clone(), e);
				telemetry!(CONSENSUS_WARN; "mushroom.err_with_block_built_on";
					"hash" => ?import_block.header.parent_hash().clone(), "err" => ?e
				);
			  }
			}

			}
		 future::ready(())
		  }
		)  ) //.map_err(|e| consensus_common::Error::ClientImport(format!("{:?}", e)).into())
	
	}


	pub fn make_import_blocks(
		&self,
		batch: &D::Output,
		inherent_data: InherentData,
		inherent_digests: DigestFor<Block>,
	) -> Result<Vec<BlockImportParams<Block>>, error::Error> {

		 info!("Processing batch with epoch {:?} of {:?} transactions into blocks",batch.epoch(),batch.len());
         let mut ret=Vec::new();
   		let mut chain_head = match self.client.best_chain() {
				Ok(x) => x,
				Err(e) => {
					warn!(target: "slots", "Unable to author block. \
					no best block header: {:?}", e);
					return Err(error::Error::Backend("No best block".to_string()))
				}
			};
        let mut parent_hash = chain_head.hash();
		let mut pnumber= chain_head.number();
		let mut parent_id=BlockId::hash(parent_hash);
		let mut block_builder = self.client.new_block_at(&parent_id, inherent_digests)?;

		// We don't check the API versions any further here since the dispatch compatibility
		// check should be enough. 
		// do this only once? 
		for extrinsic in self.client.runtime_api()
			.inherent_extrinsics_with_context(
				&parent_id,
				ExecutionContext::BlockConstruction,
				inherent_data
			)?
		{
			block_builder.push(extrinsic)?;
		}

		// proceed with transactions
		let mut is_first = true;
		let mut skipped = 0;
		let mut unqueue_invalid = Vec::new();
		let mut unqueue_valid = Vec::new();
		let pending_iterator = self.transaction_pool.ready();

		debug!("Attempting to push transactions from the batch.");
		for (nid , pending) in batch.contributions.into_iter() {

			trace!("[{:?}] Pushing to the block.", pending.hash);
			match client::block_builder::BlockBuilder::push(&mut block_builder, *pending.data.clone()) {
				Ok(()) => {
					debug!("[{:?}] Pushed to the block.", pending.hash);
				}
				Err(error::Error::ApplyExtrinsicFailed(ApplyError::FullBlock)) => {
					if is_first {
						debug!("[{:?}] Invalid transaction: FullBlock on empty block", pending.hash);
						unqueue_invalid.push(pending.hash.clone());
					  }  else 
					   {
						debug!("Block is full, proceed with proposing.");
                        let block = block_builder.bake()?;
						info!("Prepared block for proposing at {} [hash: {:?}; parent_hash: {}; extrinsics: [{}]]",
							block.header().number(),
							<Block as BlockT>::Hash::from(block.header().hash()),
							block.header().parent_hash(),
							block.extrinsics()
								.iter()
								.map(|xt| format!("{}", BlakeTwo256::hash_of(xt)))
								.collect::<Vec<_>>()
								.join(", ")
						);
						telemetry!(CONSENSUS_INFO; "prepared_block_for_proposing";
							"number" => ?block.header().number(),
							"hash" => ?<Block as BlockT>::Hash::from(block.header().hash()),
							);
						if Decode::decode(&mut block.encode().as_slice()).as_ref() != Some(&block) 
						 {
    	                	error!("Failed to verify block encoding/decoding");
		                    }

						if let Err(err) = evaluation::evaluate_initial(&block, &parent_hash, pnumber) {
							error!("Failed to evaluate authored block: {:?}", err);
						}
						let (header, body) = block.deconstruct();

						let header_num = header.number().clone();
						let parent_hash = header.parent_hash().clone();

						// sign the pre-sealed hash of the block and then
						// add it to a digest item.
						let header_hash = header.hash();

						let import_block: BlockImportParams<Block> = BlockImportParams {
							origin: BlockOrigin::Own,
							header,
							justification: None,
							post_digests: vec![],
							body: Some(body),
							finalized: true,
							auxiliary: Vec::new(),
							fork_choice: ForkChoiceStrategy::LongestChain,
						};

						info!("Pre-sealed block for proposal at {}. Hash now {:?}, previously {:?}.",
								header_num,
								import_block.post_header().hash(),
								header_hash
						);
						telemetry!(CONSENSUS_INFO; "badger.pre_sealed_block";
							"header_num" => ?header_num,
							"hash_now" => ?import_block.post_header().hash(),
							"hash_previously" => ?header_hash
						);
							parent_hash = import_block.post_header().hash();
							pnumber = import_block.post_header().number();
							parent_id = BlockId::hash(parent_hash);
							// go on to next block
							ret.push(import_block);
							block_builder = self.client.new_block_at(&parent_id, inherent_digests)?;
							is_first=true;
							continue;
					//	break;
					}
				}
				Err(e) => {
					debug!("[{:?}] Invalid transaction: {}", pending.hash, e);
					unqueue_invalid.push(pending.hash.clone());
				}
			}

			is_first = false;
		}

		self.transaction_pool.remove_invalid(&unqueue_invalid);

		Ok(ret)
	}

   pub fn  get_aggregate(mut self) -> futures03::future::Select< impl Future+ 'a, impl Future + 'a>
   {
	   futures03::future::select(self.make_sink(),self.make_block_spitter())
   }

}

/*impl<D:ConsensusProtocol<NodeId=PeerIdW>,S: Stream<Item = D::Output>  ,N,Block:BlockT,TF: Sink<TransactionSet>+Unpin,C,A: txpool::ChainApi,I:BlockImport<Block>>
  Future on BadgerProposerWorker<D,S,N,Block,TF,C,A,I>
{
	type Output=();
	fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output>
	{

	}
}*/
/// Run a GRANDPA voter as a task. Provide configuration and a link to a
/// block import worker that has already been instantiated with `block_import`.
pub fn run_honey_badger<B, E, Block: BlockT<Hash=H256>, N, RA, SC, X, I,  A>(
	client : Arc<Client<B, E, Block, RA>>,
	t_pool: Arc<TransactionPool<A>>,
	config: Config,
    network:N,
    on_exit: X,
	block_import: Arc<Mutex<I>>,
	inherent_data_providers: InherentDataProviders,
) -> ::client::error::Result<impl Future<Output=()> + Send+Unpin> where
	Block::Hash: Ord,
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + Send + Sync + 'static+Clone,
	N: Network<Block> + Send + Sync + Unpin +PollParameters,
	N::In: Send,
	SC: SelectChain<Block> + 'static,
	NumberFor<Block>: BlockNumberOps,
	DigestFor<Block>: Encode,
	RA: Send + Sync + 'static,
	X: Future<Output=()> + Clone + Send+Unpin,
	A: txpool::ChainApi,
	I:BlockImport<Block>+Send+Sync,
	RA: ConstructRuntimeApi<Block, Client<B, E, Block, RA>>,
	<Client<B, E, Block, RA> as ProvideRuntimeApi>::Api: BlockBuilderApi<Block>
{
	let (network_bridge, network_startup) = NetworkBridge::new(
		network,
		config.clone(),
		on_exit.clone(),
	);

	//let PersistentData { authority_set, set_state, consensus_changes } = persistent_data;



//	register_finality_tracker_inherent_data_provider(client.clone(), &inherent_data_providers)?;
	let (blk_out,tx_in) = global_communication(
					&client,
					&network_bridge,
				);


    let bworker= BadgerProposerWorker
                {
                 block_out: blk_out,
                 transaction_in: tx_in,
                 transaction_pool: t_pool.clone(),
                 network: network,
                 client: client.clone(),
                 block_import: block_import.clone(),
                 inherent_data_providers: inherent_data_providers,
				 ph:PhantomData,
				 phb:PhantomData
                };



   let  aggregate=bworker.get_aggregate();

 
	
	//.map(|_| ()).map_err(|e| 
	//    {
//			warn!("BADGER failed: {:?}", e);
//			telemetry!(CONSENSUS_WARN; "afg.badger_failed"; "e" => ?e);
//		}) ;

	let with_start = network_startup.then(move |()| aggregate);

	// Make sure that `telemetry_task` doesn't accidentally finish and kill grandpa.

	Ok(futures03::future::select(with_start,on_exit).then(|_| future::ready( () ) ))

}

