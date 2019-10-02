#![feature(trait_alias)]

use std::{
	collections::{BTreeMap, VecDeque},
	fmt::Debug,
	marker::PhantomData,
	sync::Arc,
	time::{Duration, Instant},
};
use client::backend::OffchainStorage;
use primitives::offchain::StorageKind;

use codec::{Decode, Encode};
use curv::cryptographic_primitives::proofs::sigma_dlog::DLogProof;
use curv::cryptographic_primitives::secret_sharing::feldman_vss::VerifiableSS;
use curv::FE;

#[cfg(not(feature = "upgraded"))]
use futures::{prelude::*, stream::Fuse, sync::mpsc};

#[cfg(feature = "upgraded")]
use futures03::{prelude::*, stream::Fuse, channel::mpsc,Poll,core_reexport::pin::Pin,task::Context,Future};

pub const LOCAL_DB: &str = "LOCAL (fork-aware) DB";
pub const STORAGE_PREFIX: &[u8] = b"storage";

use sr_primitives::traits::Header;

use keystore::KeyStorePtr;
use log::{debug, error, info};
use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2018::party_i::{
	KeyGenBroadcastMessage1 as KeyGenCommit, KeyGenDecommitMessage1 as KeyGenDecommit, Keys,
	Parameters, SharedKeys,
};
use parking_lot::RwLock;
use tokio_executor::DefaultExecutor;
use tokio_timer::Interval;

use client::blockchain::HeaderBackend;
use client::{
	backend::Backend, error::Error as ClientError, error::Result as ClientResult, BlockchainEvents,
	CallExecutor, Client,
};
use consensus_common::SelectChain;
use inherents::InherentDataProviders;
use network::{self, PeerId};
use primitives::{Blake2Hasher, H256};
use sr_primitives::generic::BlockId;
use sr_primitives::{generic::{ OpaqueDigestItemId}, Justification};
use sr_primitives::traits::{Block as BlockT, DigestFor, NumberFor, ProvideRuntimeApi};
use substrate_telemetry::{telemetry, CONSENSUS_DEBUG, CONSENSUS_INFO, CONSENSUS_WARN};

mod communication;
mod periodic_stream;
mod shared_state;
mod signer;

use communication::{
	gossip::GossipMessage,
	message::{
		ConfirmPeersMessage, KeyGenMessage, Message, MessageWithSender, PeerIndex, SignMessage,
	},
	Network, NetworkBridge,
};
use periodic_stream::PeriodicStream;
use shared_state::{load_persistent, set_signers, SharedState};
use signer::Signer;

use mpe_primitives::MP_ECDSA_ENGINE_ID;
use mpe_primitives::ConsensusLog;


type Count = u16;

#[derive(Debug)]
pub enum Error {
	Network(String),
	Client(ClientError),
}

#[derive(Clone, Debug)]
pub struct NodeConfig {
	pub threshold: Count,
	pub players: Count,
	pub name: Option<String>,
}

impl NodeConfig {
	pub fn name(&self) -> &str {
		self.name
			.as_ref()
			.map(|s| s.as_str())
			.unwrap_or("<unknown>")
	}
}

#[derive(Debug, Default)]
pub struct Confirmations {
	pub peer: Count,
	pub com_decom: Count,
	pub secret_share: Count,
	pub vss: Count,
	pub proof: Count,
}

#[derive(Debug)]
pub struct KeyGenState {
	pub complete: bool,
	pub confirmations: Confirmations,
	pub local_key: Option<Keys>,
	pub commits: BTreeMap<PeerIndex, KeyGenCommit>,
	pub decommits: BTreeMap<PeerIndex, KeyGenDecommit>,
	pub vsss: BTreeMap<PeerIndex, VerifiableSS>,
	pub secret_shares: BTreeMap<PeerIndex, FE>,
	pub proofs: BTreeMap<PeerIndex, DLogProof>,
	pub shared_keys: Option<SharedKeys>, //public key is shared_keys.y?
}

impl Default for KeyGenState {
	fn default() -> Self {
		Self {
			complete: false,
			confirmations: Confirmations::default(),
			local_key: None,
			commits: BTreeMap::new(),
			decommits: BTreeMap::new(),
			vsss: BTreeMap::new(),
			secret_shares: BTreeMap::new(),
			proofs: BTreeMap::new(),
			shared_keys: None,
		}
	}
}

use communication::MWRSink;
use communication::MWSStream;
use communication::MWSSink;

fn global_comm<Block, N>(
	bridge: &NetworkBridge<Block, N>,
) -> (
	impl MWSStream,
	impl MWRSink,
)
where
	Block: BlockT<Hash = H256>+Unpin,
	N:Network<Block>,
	<N as Network<Block>>::In: Unpin,
{
	let (global_in, global_out) = bridge.global();
	let global_in = PeriodicStream::<Block, _, MessageWithSender>::new(global_in);
	(global_in, global_out)
}

pub(crate) struct Environment<B, E, Block: BlockT, N: Network<Block>, RA,Storage> {
	pub client: Arc<Client<B, E, Block, RA>>,
	pub config: NodeConfig,
	pub bridge: NetworkBridge<Block, N>,
	pub state: Arc<RwLock<KeyGenState>>,
	pub offchain:ARc<RwLock<Storage>>,
}

#[cfg(not(feature = "upgraded"))]
pub trait ErrorFuture=Future<Item = (), Error = Error>;

#[cfg(feature = "upgraded")]
pub trait ErrorFuture=futures03::Future<Output = Result<(),Error>>;

#[cfg(not(feature = "upgraded"))]
pub trait EmptyFuture=Future<Item = (), Error = ()>;

#[cfg(feature = "upgraded")]
pub trait EmptyFuture=futures03::Future<Output = Result<(),()>>;

#[cfg(not(feature = "upgraded"))]
use futures::empty as fempty;

#[cfg(not(feature = "upgraded"))]
impl futures::Future for EmptyWrapper
{
	type Item=();
	type Error=Error;

  fn poll (&mut self) ->  Result<Async<Self::Item>, Self::Error>
  {
   Ok(Async::Ready(()))
  }
}



#[cfg(not(feature = "upgraded"))]
pub fn get_empty()->  KeyBox
 {
	 Box::new(EmptyWrapper{})
 }

struct EmptyWrapper;

#[cfg(feature = "upgraded")]
impl futures03::Future for EmptyWrapper
{
  type Output= Result<(),Error>;
  fn poll (self:Pin <&mut Self>,_cx: &mut Context<'_>) -> Poll<Self::Output>
  {
	  Poll::Ready(Ok(()))
  }
}

#[cfg(feature = "upgraded")]
type KeyBox=Pin<Box<dyn  ErrorFuture+ Send >>;



#[cfg(not(feature = "upgraded"))]
type KeyBox=Box<dyn  ErrorFuture+ Send + 'static>;



#[cfg(feature = "upgraded")]
pub fn get_empty()->  KeyBox
 {
	 Box::pin(Box::new(EmptyWrapper{}))
 }

struct KeyGenWork<B, E, Block: BlockT, N: Network<Block>, RA,Storage> 
where Storage:OffchainStorage
{
	key_gen: KeyBox,
	env: Arc<Environment<B, E, Block, N, RA,Storage>>,
}
//client::import_notification_stream 
impl<B, E, Block, N, RA,Storage> KeyGenWork<B, E, Block, N, RA,Storage>
where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + Send + Sync + 'static,
	Block: BlockT<Hash = H256>+Unpin,
	Block::Hash: Ord,
	N: Network<Block> + Sync+Unpin,
	N::In: Send + 'static+Unpin,
	RA: Send + Sync + 'static,
	Storage:OffchainStorage+'static,
{
	fn new(
		client: Arc<Client<B, E, Block, RA>>,
		config: NodeConfig,
		bridge: NetworkBridge<Block, N>,
	   db:Storage,
	) -> Self {
		let state = KeyGenState::default();

		let env = Arc::new(Environment {
			client,
			config,
			bridge,
			state: Arc::new(RwLock::new(state)),
			offchain:db,
		});
		let mut work = Self {
			key_gen: get_empty(),
			env,
		};
		work.rebuild();
		work
	}

	fn rebuild(&mut self) {
		let should_rebuild = true;
		if should_rebuild {
			let (incoming, outgoing) = global_comm(&self.env.bridge);
			let signer = Signer::new(self.env.clone(), incoming, outgoing);
			self.key_gen = new_signer(signer);
		} else {
			self.key_gen=get_empty();
		}
	}
	pub fn local_storage_set(&mut self, kind: StorageKind, key: &[u8], value: &[u8]) {
		match kind {
			StorageKind::PERSISTENT => self.env.offchain.write().set(STORAGE_PREFIX, key, value),
			StorageKind::LOCAL => {},
		}
	}
}




#[cfg(not(feature = "upgraded"))]
fn new_signer<B,E,Block:BlockT,N: Network<Block>, RA, In, Out,Storage>(s:Signer<B,E,Block,N,RA,In,Out,Storage>) ->Box<Signer<B,E,Block,N,RA,In,Out,Storage>>
where 	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + Send + Sync + 'static,
	Block: BlockT<Hash = H256>,
	Block::Hash: Ord,
	N: Network<Block> + Sync,
	N::In: Send + 'static,
	RA: Send + Sync + 'static,
	In: MWSStream,
	Out: MWSSink,
{
	Box::new(s)
}

#[cfg(feature = "upgraded")]
fn new_signer<B,E,Block:BlockT,N: Network<Block>, RA, In, Out,Storage>(s:Signer<B,E,Block,N,RA,In,Out,Storage>) ->Pin<Box<Signer<B,E,Block,N,RA,In,Out,Storage>>>
where	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + Send + Sync + 'static,
	Block: BlockT<Hash = H256>,
	Block::Hash: Ord,
	N: Network<Block> + Sync,
	N::In: Send + 'static,
	RA: Send + Sync + 'static,
	In: MWSStream,
	Out: MWSSink,
	Storage:OffchainStorage,
{
	Box::pin(s)
}



#[cfg(not(feature = "upgraded"))]
impl<B, E, Block, N, RA,Storage> Future for KeyGenWork<B, E, Block, N, RA,Storage>
where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + 'static + Send + Sync,
	Block: BlockT<Hash = H256>,
	Block::Hash: Ord,
	N: Network<Block> + Send + Sync + 'static,
	N::In: Send + 'static,
	RA: Send + Sync + 'static,
	Storage: OffchainStorage,
{
	type Item = ();
	type Error = Error;

	fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
		match self.key_gen.poll() {
			Ok(Async::NotReady) => {
				return Ok(Async::NotReady);
			}
			Ok(Async::Ready(())) => {
				return Ok(Async::Ready(()));
			}
			Err(e) => {
				// return inner observer error
				println!("error from poll {:?}", e);
				return Err(e);
			}
		}
	}
}

#[cfg(feature = "upgraded")]
impl<B, E, Block, N, RA,Storage> futures03::Future for KeyGenWork<B, E, Block, N, RA,Storage>
where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + 'static + Send + Sync,
	Block: BlockT<Hash = H256>,
	Block::Hash: Ord,
	N: Network<Block> + Send + Sync + 'static,
	N::In: Send + 'static+Unpin,
	RA: Send + Sync + 'static,
	Storage: OffchainStorage,
{
	type Output = Result<(),Error>;
	
	fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
		match Pin::new(&mut self.as_mut().key_gen).poll(cx) {
			Poll::Pending => {
				return Poll::Pending;
			}
			Poll::Ready(dat) => {
				return Poll::Ready(dat);
			}
		}
	}
}

pub fn init_shared_state<B, E, Block, RA>(client: Arc<Client<B, E, Block, RA>>) -> SharedState
where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + 'static + Clone + Send + Sync,
	Block: BlockT<Hash = H256>,
	Block::Hash: Ord,
	RA: Send + Sync + 'static,
{
	let persistent_data: SharedState = load_persistent(&**client.backend()).unwrap();
	persistent_data
}

//use futures::{Future, Stream};

fn transform_u64_to_array_of_u8(x:u64) -> [u8;8] {
	let o1 : u8 = ((x >> 56) & 0xff) as u8;
    let o2 : u8 = ((x >> 48) & 0xff) as u8;
    let o3 : u8 = ((x >> 40) & 0xff) as u8;
    let o4 : u8 = ((x >> 32) & 0xff) as u8;
    
	let b1 : u8 = ((x >> 24) & 0xff) as u8;
    let b2 : u8 = ((x >> 16) & 0xff) as u8;
    let b3 : u8 = ((x >> 8) & 0xff) as u8;
    let b4 : u8 = (x & 0xff) as u8;
    return [o1,o2,o3,o4,b1, b2, b3, b4]
}


use futures::stream::Stream as OldStream;
use futures03::{StreamExt as _, TryStreamExt as _};
 use primitives::traits::BareCryptoStore;
use mpe_primitives::ConsensusLog::RequestForKeygen;
use primitives::crypto::key_types::ECDSA_SHARED;


pub fn run_key_gen<B, E, Block, N, RA>(
	local_peer_id: PeerId,
	keystore: KeyStorePtr,
	client: Arc<Client<B, E, Block, RA>>,
	network: N,
	backend:Arc<B>,
) -> ClientResult<impl EmptyFuture + Send + 'static>
where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + 'static + Send + Sync,
	Block: BlockT<Hash = H256>+Unpin,
	Block::Hash: Ord,
	N: Network<Block> + Send + Sync + 'static+Unpin,
	N::In: Send + 'static+Unpin,
	RA: Send + Sync + 'static,

{
	let config = NodeConfig {
		name: None,
		threshold: 1,
		players: 4,
	};

	//let keystore = keystore.read();

	// let persistent_data: SharedState = load_persistent(&**client.backend()).unwrap();
	// println!("{:?}", persistent_data);
	// println!("Local peer ID {:?}", current_id.as_bytes());

	// let mut signers = persistent_data.signer_set;
	// let current_id = current_id.into_bytes();
	// if !signers.contains(&current_id) {
	// 	// if our id is not in it, add our self
	// 	signers.push(current_id);
	// 	set_signers(&**client.backend(), signers);
	// }
	let bridge = NetworkBridge::new(network, config.clone(), local_peer_id);
    let streamer=client.clone().import_notification_stream().for_each(move |n| {
		info!(target: "keygen", "HEADER {:?}, looking for consensus message", &n.header);
		// spawn any futures that were created in the previous setup steps
    /*	for task in tasks_to_spawn.drain(..) {
      service.spawn_task(
        task.select(service.on_exit())
          .map(|_| ())
          .map_err(|_| ())
      );
    }*/

        for log in n.header.digest().logs() 
		{
		 info!(target: "keygen", "Checking log {:?}, looking for consensus message", log);
		 match log.try_as_raw(OpaqueDigestItemId::Consensus(&MP_ECDSA_ENGINE_ID))
		 {
			Some(data) => { 
				info!("Got log id! {:?}",data);
		       let log_inner = log.try_to::<ConsensusLog>(OpaqueDigestItemId::Consensus(&MP_ECDSA_ENGINE_ID));
                info!("Got log inner! {:?}",log_inner);
				if let Some(log_in)=log_inner 
				{
		       match log_in
			   {
				   RequestForKeygen((id,data)) =>
				   { 
					   keystore.read().initiate_request(&id.to_be_bytes(),ECDSA_SHARED);
				   },
				   _ =>{}
			   }
				}
			 },
			None => {}
	     }
		//..ECDSA_SHARED KeyTypeId
	    }
		info!(target: "substrate-log", "Imported with log called  #{} ({})", n.header.number(), n.hash);
		futures03::future::ready( ())
//		Ok(())
	});
	
	let key_gen_work = KeyGenWork::new(client, config, bridge,backend.offchain_storage().expect("Need offchain for keygen work")).map_err(|e| error!("Error {:?}", e));

    #[cfg(not(feature = "upgraded"))]
	return Ok(key_gen_work);
	//futures03::future::select(key_gen_work,streamer);
	#[cfg(feature = "upgraded")]
	Ok(futures03::future::select(key_gen_work,streamer).then(|_| futures03::future::ready( Ok(())) ))//key_gen_work)
}

#[cfg(test)]
mod tests;
