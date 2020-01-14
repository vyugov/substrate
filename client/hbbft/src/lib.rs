use std::marker::{Send, Sync};
use std::pin::Pin;
use badger::crypto::SecretKeyShare;
use std::{fmt::Debug, hash::Hash, marker::PhantomData, sync::Arc, time::Duration, time::Instant};

use bincode;
use futures03::future::Future;
use futures03::prelude::*;
use futures03::task::Poll;
use futures_timer::Delay;
use hex;
use log::{debug, info, trace, warn};
use parity_codec::{Decode, Encode};
use parking_lot::Mutex;
use inherents::InherentIdentifier;
use keystore::KeyStorePtr;
use runtime_primitives::traits::{Block as BlockT, Header, NumberFor, ProvideRuntimeApi};

use block_builder::BlockBuilderApi;
use runtime_primitives::{
  generic::{self, BlockId}, 
  Justification,
};
pub mod communication;
use crate::communication::Network;
use badger::ConsensusProtocol;
use badger_primitives::HBBFT_AUTHORITIES_KEY;
use std::iter;

use sp_blockchain::{Error as ClientError, Result as ClientResult};

use client::blockchain::HeaderBackend;
use client::{blockchain, blockchain::ProvideCache, well_known_cache_keys, CallExecutor, Client};
use communication::NetworkBridge;
use communication::TransactionSet;
use communication::QHB;
use consensus_common::block_import::BlockImport;
use consensus_common::import_queue::{
  BasicQueue, BoxBlockImport, BoxFinalityProofImport, BoxJustificationImport, CacheKeyId, Verifier,
};
use consensus_common::BlockCheckParams;
use consensus_common::ImportResult;
pub use consensus_common::SyncOracle;
use consensus_common::{
  self, BlockImportParams, BlockOrigin, Error as ConsensusError, ForkChoiceStrategy, SelectChain,
};
use inherents::{InherentData, InherentDataProviders};
use sc_api::{AuxStore, Backend};
use sp_api::ConstructRuntimeApi;
use std::collections::HashMap;
use runtime_primitives::traits::DigestFor;
use substrate_primitives::{storage::StorageKey, Blake2Hasher, ExecutionContext, H256};
use substrate_telemetry::{telemetry, CONSENSUS_INFO, CONSENSUS_WARN};
use crate::communication::SendOut;
use txp::InPoolTransaction;
use txp::TransactionPool;

pub const INHERENT_IDENTIFIER: InherentIdentifier = *b"snakeshr";
use communication::BlockPusherMaker;
use network::ClientHandle as NetClient;
pub type BadgerImportQueue<B> = BasicQueue<B>;
pub mod aux_store;
pub mod rpc;
pub struct BadgerWorker<C, I, SO, Inbound, B: BlockT, A, Cl, BPM, Aux>
where
  A: TransactionPool,
  Cl: NetClient<B>,
  B::Hash: Ord,
  BPM: BlockPusherMaker<B>,
  Aux: AuxStore,
{
  pub client: Arc<C>,
  pub block_import: Arc<Mutex<I>>,
  pub network: NetworkBridge<B, Cl, BPM, Aux>,

  pub transaction_pool: Arc<A>,
  pub sync_oracle: SO,
  pub blocks_in: Inbound,
}

type Interval = Box<dyn Stream<Item = ()> + Unpin + Send + Sync>;

fn interval_at(start: Instant, duration: Duration) -> Interval
{
  let stream = futures03::stream::unfold(start, move |next| {
    let time_until_next = next.saturating_duration_since(Instant::now());

    Delay::new(time_until_next).map(move |_| Some(((), next + duration)))
  });

  Box::new(stream)
}

pub struct BadgerVerifier<C, Pub, Sig>
where
  Sig: Send + Sync,
  Pub: Send + Sync,
{
  _client: Arc<C>,
  _pub: PhantomData<Pub>,
  _sig: PhantomData<Sig>,
  _inherent_data_providers: inherents::InherentDataProviders,
}

impl<C, Pub, Sig> BadgerVerifier<C, Pub, Sig>
where
  Sig: Send + Sync,
  Pub: Send + Sync,
{
  fn _check_inherents<B: BlockT>(
    &self, _block: B, _block_id: BlockId<B>, _inherent_data: InherentData, _timestamp_now: u64,
  ) -> Result<(), String>
  where
    C: ProvideRuntimeApi,
    C::Api: BlockBuilderApi<B>,
  {
    //empty for now
    Ok(())
  }
}

//Block:BlockT

pub struct BadgerBlockImport<B, E, Block: BlockT<Hash = H256>, RA, SC>
{
  pub inner: Arc<Client<B, E, Block, RA>>,
  pub select_chain: SC,
  pub authority_set: aux_store::BadgerSharedAuthoritySet,
  pub send_on_import: ImportTx<Block>,
  //	consensus_changes: SharedConsensusChanges<Block::Hash, NumberFor<Block>>,
}

impl<B, E, Block: BlockT<Hash = H256>, RA, SC: Clone> Clone for BadgerBlockImport<B, E, Block, RA, SC>
{
  fn clone(&self) -> Self
  {
    BadgerBlockImport {
      inner: self.inner.clone(),
      select_chain: self.select_chain.clone(),
      authority_set: self.authority_set.clone(),
      send_on_import: self.send_on_import.clone(),
    }
  }
}

impl<B, E, Block: BlockT<Hash = H256>, RA, SC> BadgerBlockImport<B, E, Block, RA, SC>
{
  pub(crate) fn new(
    inner: Arc<Client<B, E, Block, RA>>,
    select_chain: SC,
    authority_set: aux_store::BadgerSharedAuthoritySet,
    send_on_import: ImportTx<Block>,
  ) -> BadgerBlockImport<B, E, Block, RA, SC>
  {
    BadgerBlockImport {
      inner,
      select_chain,
      authority_set,
      send_on_import,
    }
  }
}

impl<B, E, Block: BlockT<Hash = H256>, RA, SC> BlockImport<Block> for BadgerBlockImport<B, E, Block, RA, SC>
where
  //NumberFor<Block>: grandpa::BlockNumberOps,
  B: Backend<Block, Blake2Hasher> + 'static,
  E: CallExecutor<Block, Blake2Hasher> + 'static + Clone + Send + Sync,
  DigestFor<Block>: Encode,
  RA: Send + Sync,
  SC: SelectChain<Block> + Clone,
{
  type Error = ConsensusError;

  fn import_block(
    &mut self, block: BlockImportParams<Block>, new_cache: HashMap<well_known_cache_keys::Id, Vec<u8>>,
  ) -> Result<ImportResult, Self::Error>
  {
    let hash = block.post_header().hash();
    let number = block.header.number().clone();

    // early exit if block already in chain, probably a duplicate sent from another node
    match self.inner.status(BlockId::Hash(hash))
    {
      Ok(blockchain::BlockStatus::InChain) =>
      {
        info!("Block already in chain");
        return Ok(ImportResult::AlreadyInChain);
      }
      Ok(blockchain::BlockStatus::Unknown) =>
      {}
      Err(e) => return Err(ConsensusError::ClientImport(e.to_string()).into()),
    }
    info!("BLOCK Origin {:?} for {:?} {:?}", block.origin, hash, number);
    //we only import our own blocks, though catch up may be necessary later on
    match block.origin
    {
      // Genesis block built into the client.
      BlockOrigin::Genesis =>
      {
        info!("Importing Genesis");
      }
      // Block is part of the initial sync with the network.
      BlockOrigin::NetworkInitialSync | BlockOrigin::NetworkBroadcast =>
      {
        //resend through self/Own
        if block.justification.is_none()
        {
          info!("Unfinalizing block, rejecting?");
          return Ok(ImportResult::AlreadyInChain);
        }
        //match
        self.send_on_import.call(block);
        /*  {
          Ok(_) =>
          {}
          Err(e) =>
          {
            info!("Send failed with {:?}", e);
            return Err(ConsensusError::ClientImport(e.to_string()).into());
          }
        }*/
        //let import_result = (&*self.inner).import_block(block, new_cache);
        // return import_result;
        return Ok(ImportResult::AlreadyInChain);
        // info!("Sync Block Justification: {:?}",&justification);
        // let bh=block.header.hash().clone();
        //let bn=block.header.number().clone();
      }
      // Block was broadcasted on the network.

      // Block that was received from the network and validated in the consensus process.
      BlockOrigin::ConsensusBroadcast =>
      {
        return Ok(ImportResult::AlreadyInChain);
      }
      // Block that was collated by this node.
      BlockOrigin::Own =>
      {}
      // Block was imported from a file.
      BlockOrigin::File =>
      {
        //ignore?
        return Ok(ImportResult::AlreadyInChain);
      }
    }

    // we  want to finalize on `inner.import_block`, probably
    //let mut justification = block.justification.take();
    //let enacts_consensus_change = !new_cache.is_empty();
    let import_result = (&*self.inner).import_block(block, new_cache);
    import_result
  }

  fn check_block(&mut self, block: BlockCheckParams<Block>) -> Result<ImportResult, Self::Error>
  {
    self.inner.check_block(block)
  }
}

impl<B: BlockT, C, Pub, Sig> Verifier<B> for BadgerVerifier<C, Pub, Sig>
where
  C: ProvideRuntimeApi + Send + Sync + sc_api::AuxStore + ProvideCache<B>,
  C::Api: BlockBuilderApi<B>,
  //DigestItemFor<B>: CompatibleDigestItem<P>,
  Pub: Send + Sync + Hash + Eq + Clone + Decode + Encode + Debug,
  Sig: Encode + Decode + Send + Sync,
{
  fn verify(
    &mut self, origin: BlockOrigin, header: B::Header, justification: Option<Justification>,
    body: Option<Vec<B::Extrinsic>>,
  ) -> Result<(BlockImportParams<B>, Option<Vec<(CacheKeyId, Vec<u8>)>>), String>
  {
    // dummy for the moment
    //let hash = header.hash();
    //let parent_hash = *header.parent_hash();
    let import_block = BlockImportParams {
      origin,
      header: header,
      post_digests: vec![],
      body,
      finalized: false,
      justification,
      allow_missing_state: true,
      auxiliary: Vec::new(),
      fork_choice: ForkChoiceStrategy::LongestChain,
      import_existing: false,
    };
    info!("VERIFIER BADGER");
    Ok((import_block, None))
  }
}

/// Register the aura inherent data provider, if not registered already.
fn register_badger_inherent_data_provider(
  inherent_data_providers: &InherentDataProviders, _slot_duration: u64,
) -> Result<(), consensus_common::Error>
{
  if !inherent_data_providers.has_provider(&srml_timestamp::INHERENT_IDENTIFIER)
  {
    inherent_data_providers
      .register_provider(sp_timestamp::InherentDataProvider)
      .map_err(Into::into)
      .map_err(consensus_common::Error::InherentData)
  }
  else
  {
    Ok(())
  }
}

/// Start an import queue for the Badger consensus algorithm.
pub fn badger_import_queue<B, C, Pub, Sig>(
  block_import: BoxBlockImport<B>, justification_import: Option<BoxJustificationImport<B>>,
  finality_proof_import: Option<BoxFinalityProofImport<B>>, client: Arc<C>,
  inherent_data_providers: InherentDataProviders,
) -> Result<BadgerImportQueue<B>, consensus_common::Error>
where
  B: BlockT,
  C: 'static + ProvideRuntimeApi + ProvideCache<B> + Send + Sync + AuxStore,
  C::Api: BlockBuilderApi<B>,
  //DigestItemFor<B>: CompatibleDigestItem<P>,
  Pub: Clone + Eq + Send + Sync + Hash + Debug + Encode + Decode + 'static,
  Sig: Encode + Decode + Send + Sync + 'static,
{
  register_badger_inherent_data_provider(&inherent_data_providers, 1)?;
  //initialize_authorities_cache(&*client)?;

  let verifier = BadgerVerifier::<C, Pub, Sig> {
    _client: client.clone(),
    _inherent_data_providers: inherent_data_providers,
    _pub: PhantomData,
    _sig: PhantomData,
  };

  Ok(BasicQueue::new(
    verifier,
    block_import,
    justification_import,
    finality_proof_import,
  ))
}

use client::light::fetcher::StorageProof;

use badger_primitives::AuthorityList;
/// Badger authority set getter? .
pub trait AuthoritySetGetter<Block: BlockT>: Send + Sync
{
  /// Read HBBFT_AUTHORITIES_KEY from storage at given block.
  fn authorities(&self, block: &BlockId<Block>) -> ClientResult<AuthorityList>;
  /// Prove storage read of HBBFT_AUTHORITIES_KEY at given block.
  fn prove_authorities(&self, block: &BlockId<Block>) -> ClientResult<StorageProof>;
}

/// Client-based implementation of AuthoritySetForFinalityProver.
impl<B, E, Block: BlockT<Hash = H256>, RA> AuthoritySetGetter<Block> for Client<B, E, Block, RA>
where
  B: Backend<Block, Blake2Hasher> + Send + Sync + 'static,
  E: CallExecutor<Block, Blake2Hasher> + 'static + Clone + Send + Sync,
  RA: Send + Sync,
{
  fn authorities(&self, block: &BlockId<Block>) -> ClientResult<AuthorityList>
  {
    let storage_key = StorageKey(HBBFT_AUTHORITIES_KEY.to_vec());
    self
      .storage(block, &storage_key)?
      .and_then(|encoded| AuthorityList::decode(&mut encoded.0.as_slice()).ok())
      .map(|versioned| versioned.into())
      .ok_or(ClientError::InvalidAuthoritiesSet)
  }

  fn prove_authorities(&self, block: &BlockId<Block>) -> ClientResult<StorageProof>
  {
    self.read_proof(block, iter::once(HBBFT_AUTHORITIES_KEY))
  }
}

/// Configuration for the Badger service.
#[derive(Clone)]
pub struct Config
{
  /// Some local identifier of the node.
  pub name: Option<String>,
  pub batch_size: u32,
  //	pub initial_validators: BTreeMap<PeerIdW, PublicKey>, replaced by session aspects
  //	pub node_indices: BTreeMap<PeerIdW, usize>, unnecessary
}

fn _secret_share_from_string(st: &str) -> Result<SecretKeyShare, Error>
{
  let data = hex::decode(st)?;
  match bincode::deserialize(&data)
  {
    Ok(val) => Ok(val),
    Err(_) => return Err(Error::Badger("secret key share binary invalid".to_string())),
  }
}

impl Config
{
  fn _name(&self) -> &str
  {
    self.name.as_ref().map(|s| s.as_str()).unwrap_or("<unknown>")
  }
}

/// Errors that can occur while voting in BADGER.
#[derive(Debug)]
pub enum Error
{
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
  fn from(e: hex::FromHexError) -> Self
  {
    Error::Badger(e.to_string())
  }
}
impl From<ClientError> for Error
{
  fn from(e: ClientError) -> Self
  {
    Error::Client(e)
  }
}

/// Something which can determine if a block is known.
pub trait BlockStatus<Block: BlockT>
{
  /// Return `Ok(Some(number))` or `Ok(None)` depending on whether the block
  /// is definitely known and has been imported.
  /// If an unexpected error occurs, return that.
  fn block_number(&self, hash: Block::Hash) -> Result<Option<NumberFor<Block>>, Error>;
}
pub trait BlockNumberOps:
  std::fmt::Debug
  + std::cmp::Ord
  + std::ops::Add<Output = Self>
  + std::ops::Sub<Output = Self>
  + num::One
  + num::Zero
  + num::AsPrimitive<usize>
{
}

impl<T> BlockNumberOps for T
where
  T: std::fmt::Debug,
  T: std::cmp::Ord,
  T: std::ops::Add<Output = Self>,
  T: std::ops::Sub<Output = Self>,
  T: num::One,
  T: num::Zero,
  T: num::AsPrimitive<usize>,
{
}

impl<B, E, Block: BlockT<Hash = H256>, RA> BlockStatus<Block> for Arc<Client<B, E, Block, RA>>
where
  B: Backend<Block, Blake2Hasher>,
  E: CallExecutor<Block, Blake2Hasher> + Send + Sync,
  RA: Send + Sync,
  NumberFor<Block>: BlockNumberOps,
{
  fn block_number(&self, hash: Block::Hash) -> Result<Option<NumberFor<Block>>, Error>
  {
    self
      .block_number_from_id(&BlockId::Hash(hash))
      .map_err(|e| Error::Blockchain(format!("{:?}", e)))
  }
}
//use communication::BadgerHandler;

/// Parameters used to run Honeyed Badger.
pub struct BadgerStartParams<Block: BlockT<Hash = H256>, N: Network<Block>, X>
{
  /// Configuration for the Badger service.
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
  //	pub telemetry_on_connect: Option<TelemetryOnConnect>,
  ph: PhantomData<Block>,
}

pub struct TxStream<A, B, E, RA, Block: BlockT<Hash = H256>>
where
  A: TransactionPool,
  B: Backend<Block, Blake2Hasher> + Send + Sync + 'static,
  E: CallExecutor<Block, Blake2Hasher> + Send + Sync + Clone + 'static,
  RA: ConstructRuntimeApi<Block, Client<B, E, Block, RA>>,
{
  pub transaction_pool: Arc<A>,
  pub client: Arc<Client<B, E, Block, RA>>,
}

impl<A, B, E, RA, Block: BlockT<Hash = H256>> Stream for TxStream<A, B, E, RA, Block>
where
  A: TransactionPool,
  <A as TransactionPool>::Block: BlockT<Hash = H256>,
  B: Backend<Block, Blake2Hasher> + Send + Sync + 'static,
  E: CallExecutor<Block, Blake2Hasher> + Send + Sync + Clone + 'static,
  RA: ConstructRuntimeApi<Block, Client<B, E, Block, RA>>,
{
  type Item = TransactionSet;
  fn poll_next(self: Pin<&mut Self>, _cx: &mut futures03::task::Context) -> Poll<Option<Self::Item>>
  {
    trace!("BADgER! Polled stream!");
    let pending_iterator = self.transaction_pool.ready();
    let batch: Vec<_> = pending_iterator.map(|a| a.data().encode()).collect();
    //let pending_iterator = self.transaction_pool.ready();

    let pending_iterator = self.transaction_pool.ready();
    let hashes: Vec<_> = pending_iterator.map(|a| a.hash().clone()).collect();
    if batch.len() == 0
    {
      return Poll::Pending;
    }
    info!("BADgER! Ready stream!");
    //let best_block_hash = self.client.info().chain.best_hash;
    self.transaction_pool.remove_invalid(&hashes);
    Poll::Ready(Some(batch))
  }
}

pub struct BadgerProposerWorker<S, Block: BlockT<Hash = H256>, I, B, E, RA, SC>
where
  S: Stream<Item = <QHB as ConsensusProtocol>::Output>,
  //TF: Sink<TransactionSet>+Unpin,
  //A: txpool::ChainApi,
  B: Backend<Block, Blake2Hasher> + Send + Sync + 'static,
  E: CallExecutor<Block, Blake2Hasher> + Send + Sync + Clone + 'static,
  RA: ConstructRuntimeApi<Block, Client<B, E, Block, RA>>,
  //N : Network<Block> + Send + Sync + 'static,
  NumberFor<Block>: BlockNumberOps,
  SC: SelectChain<Block> + Clone,
  I: BlockImport<Block>,
{
  pub block_out: S,
  //pub transaction_in: TF,
  //pub transaction_out: TxStream<A>,//Arc<TransactionPool<A>>,
  //pub network: N,
  pub client: Arc<Client<B, E, Block, RA>>,
  pub block_import: Arc<Mutex<I>>,
  pub inherent_data_providers: InherentDataProviders,
  pub select_chain: SC,
  phb: PhantomData<Block>,
}

use crate::aux_store::GenesisAuthoritySetProvider;

//Block:BlockT
pub type BlockSentInfo<Block> = BlockImportParams<Block>;

pub trait AssignedCaller<Block: BlockT>
{
  fn call(&self, block: BlockSentInfo<Block>);
  fn assign(&self, new_call: Box<dyn Fn(BlockSentInfo<Block>) + Send + Sync>);
}
//#[derive(Clone)]
pub struct DefCall<Block: BlockT>
{
  call: parking_lot::RwLock<Option<Box<dyn Fn(BlockSentInfo<Block>) + Send + Sync>>>,
}
impl<Block: BlockT> DefCall<Block>
{
  pub fn new() -> Arc<Self>
  {
    Arc::new(DefCall {
      call: parking_lot::RwLock::new(None),
    })
  }
}

impl<Block: BlockT> AssignedCaller<Block> for Arc<DefCall<Block>>
{
  fn call(&self, block: BlockSentInfo<Block>)
  {
    if let Some(ref cl) = *self.call.read()
    {
      cl(block);
    }
  }
  fn assign(&self, new_call: Box<dyn Fn(BlockSentInfo<Block>) + Send + Sync>)
  {
    *self.call.write() = Some(new_call);
  }
}
pub type ImportRx<Block> = Arc<DefCall<Block>>; //mpsc::UnboundedReceiver<BlockSentInfo<Block>>;
pub type ImportTx<Block> = Arc<DefCall<Block>>; //mpsc::UnboundedSender<BlockSentInfo<Block>>;

pub fn block_importer<B, E, Block: BlockT<Hash = H256>, RA, SC>(
  client: Arc<Client<B, E, Block, RA>>, genesis_authorities_provider: &dyn GenesisAuthoritySetProvider<Block>,
  select_chain: SC,
) -> Result<(BadgerBlockImport<B, E, Block, RA, SC>, ImportRx<Block>), ClientError>
where
  B: Backend<Block, Blake2Hasher> + 'static,
  E: CallExecutor<Block, Blake2Hasher> + 'static + Clone + Send + Sync,
  RA: Send + Sync,
  SC: SelectChain<Block>,
{
  let chain_info = client.chain_info();
  let _genesis_hash = chain_info.genesis_hash;

  let persistent_data = aux_store::loads_auth_set(&*client, || {
    let authorities = genesis_authorities_provider.get()?;
    telemetry!(CONSENSUS_INFO; "afg.loading_authorities";
      "authorities_len" => ?authorities.len()
    );
    Ok(authorities)
  })?;

  // let (import_commands_tx, import_commands_rx) = mpsc::unbounded();
  let caller = DefCall::new();
  Ok((
    BadgerBlockImport::new(
      client.clone(),
      select_chain.clone(),
      persistent_data.clone(),
      caller.clone(),
    ),
    caller,
  ))
}

use block_builder::BlockBuilder;
use communication::BadgerTransaction;
use futures03::stream::StreamExt;
use sc_api::backend::Finalizer;
use std::collections::VecDeque;

use communication::BatchProcessor;
use communication::BlockPushResult;

pub struct BlockUtil<B, E, Block, RA, SC, I>
where
  Block: BlockT<Hash = H256>,
  Block::Hash: Ord,
  B: Backend<Block, Blake2Hasher> + 'static,
  E: CallExecutor<Block, Blake2Hasher> + Send + Sync + 'static + Clone,
  SC: SelectChain<Block> + 'static + Unpin,
  NumberFor<Block>: BlockNumberOps,
  DigestFor<Block>: Encode,
  RA: Send + Sync + 'static,
  I: BlockImport<Block> + Send + Sync + 'static,
  RA: ConstructRuntimeApi<Block, Client<B, E, Block, RA>>,
  <Client<B, E, Block, RA> as ProvideRuntimeApi>::Api: BlockBuilderApi<Block, Error = sp_blockchain::Error>,
{
  pub client: Arc<Client<B, E, Block, RA>>,
  pub block_import: Arc<Mutex<I>>,
  pub selch: SC,
  //phantom: PhantomData<BPusher<'a,Block,B,E,RA>>,
}
impl<B, E, Block, RA, SC, I> BlockUtil<B, E, Block, RA, SC, I>
where
  Block: BlockT<Hash = H256>,
  Block::Hash: Ord,
  B: Backend<Block, Blake2Hasher> + 'static,
  E: CallExecutor<Block, Blake2Hasher> + Send + Sync + 'static + Clone,
  SC: SelectChain<Block> + 'static + Unpin,
  NumberFor<Block>: BlockNumberOps,
  DigestFor<Block>: Encode,
  RA: Send + Sync + 'static,
  I: BlockImport<Block> + Send + Sync + 'static,
  RA: ConstructRuntimeApi<Block, Client<B, E, Block, RA>>,
  <Client<B, E, Block, RA> as ProvideRuntimeApi>::Api: BlockBuilderApi<Block, Error = sp_blockchain::Error>,
{
  pub fn process_data(
    block_builder: &mut BlockBuilder<Block, Client<B, E, Block, RA>>, pending: &mut BadgerTransaction,
  ) -> BlockPushResult
  {
    let data: Result<<Block as BlockT>::Extrinsic, _> = Decode::decode(&mut pending.as_slice());
    let data = match data
    {
      Ok(val) => val,
      Err(_) =>
      {
        info!("Data decoding error");
        return BlockPushResult::InvalidData;
      }
    };
    match block_builder::BlockBuilder::push(block_builder, data)
    {
      Ok(()) =>
      {
        debug!("[{:?}] bytes Pushed to the block.", pending.len());
        return BlockPushResult::Pushed;
      }

      Err(sp_blockchain::Error::ApplyExtrinsicFailed(sp_blockchain::ApplyExtrinsicFailed::Validity(e)))
        if e.exhausted_resources() =>
      {
        return BlockPushResult::BlockFull;
      }
      Err(e) =>
      {
        info!("[{:?}] Invalid transaction: {}", pending.len(), e);
        return BlockPushResult::BlockError;
      }
    }
  }
}

impl<B, E, Block, RA, SC, I> BlockPusherMaker<Block> for BlockUtil<B, E, Block, RA, SC, I>
where
  Block: BlockT<Hash = H256>,
  Block::Hash: Ord,
  B: Backend<Block, Blake2Hasher> + 'static,
  E: CallExecutor<Block, Blake2Hasher> + Send + Sync + 'static + Clone,
  SC: SelectChain<Block> + 'static + Unpin,
  NumberFor<Block>: BlockNumberOps,
  DigestFor<Block>: Encode,
  RA: Send + Sync + 'static,
  I: BlockImport<Block> + Send + Sync + 'static,
  RA: ConstructRuntimeApi<Block, Client<B, E, Block, RA>>,
  <Client<B, E, Block, RA> as ProvideRuntimeApi>::Api: BlockBuilderApi<Block, Error = sp_blockchain::Error>,
{
  fn process_all(
    &mut self, is: BlockId<Block>, digest: generic::Digest<Block::Hash>, locked: &mut VecDeque<BadgerTransaction>,
    batched: impl Iterator<Item = BadgerTransaction>,
  ) -> Result<Block, ()>
  {
    let mut block_builder = match self.client.new_block_at(&is, digest)
    {
      Ok(val) => val,
      Err(_) =>
      {
        warn!("Error in new_block_at");
        return Err(());
      }
    };
    let mut block_done = false;
    let mut cnt: u64 = 0;

    while locked.len() > 0
    {
      let mut trans = locked.pop_back().unwrap();
      match BlockUtil::<B, E, Block, RA, SC, I>::process_data(&mut block_builder, &mut trans)
      {
        BlockPushResult::BlockFull =>
        {
          if cnt > 0
          {
            locked.push_back(trans);
            block_done = true;
            break;
          }
        }
        BlockPushResult::Pushed =>
        {
          cnt += 1;
        }
        _ =>
        {}
      }
    }
    for mut pending in batched
    {
      if block_done
      {
        locked.push_back(pending);
      }
      else
      {
        match BlockUtil::<B, E, Block, RA, SC, I>::process_data(&mut block_builder, &mut pending)
        {
          BlockPushResult::BlockFull =>
          {
            if cnt > 0
            {
              locked.push_back(pending);
              block_done = true;
            }
            else
            {
              info!("Overlarge transaction, ignoring");
            }
          }
          BlockPushResult::Pushed =>
          {
            cnt += 1;
          }
          _ =>
          {}
        }
      }
    }
    debug!("Block is done, proceed with proposing.");
    let block = match block_builder.bake()
    {
      Ok(val) => Ok(val),
      Err(e) =>
      {
        warn!("Block baking error {:?}", e);
        return Err(());
      }
    };
    block
  }

  fn best_chain(&self) -> Result<<Block as BlockT>::Header, ()>
  {
    let chain_head = match self.selch.best_chain()
    {
      Ok(x) => x,
      Err(e) =>
      {
        warn!(target: "formation", "Unable to author block, no best block header: {:?}", e);
        return Err(());
      }
    };
    Ok(chain_head)
  }
  fn import_block(&self, import_block: BlockImportParams<Block>) -> Result<(), ()>
  {
    {
      let eh = import_block.header.parent_hash().clone();
      if let Err(e) = self.block_import.lock().import_block(import_block, Default::default())
      {
        warn!(target: "badger", "Error with block built on {:?}: {:?}",eh, e);
        telemetry!(CONSENSUS_WARN; "mushroom.err_with_block_built_on";
			"hash" => ?eh, "err" => ?e);
        return Err(());
      }
      Ok(())
    }
  }
}
use communication::BadgerStream;
use sc_network_ranting::Network as RantingNetwork;

pub struct Cwrap<B, E, Block: BlockT, RA>
{
  pub client: Arc<Client<B, E, Block, RA>>,
}
impl<B, E, Block, RA> AuxStore for Cwrap<B, E, Block, RA>
where
  Block: BlockT<Hash = H256>,
  Block::Hash: Ord,
  B: Backend<Block, Blake2Hasher> + 'static,
  E: CallExecutor<Block, Blake2Hasher> + Send + Sync + 'static + Clone,
  RA: Send + Sync + 'static,
  RA: ConstructRuntimeApi<Block, Client<B, E, Block, RA>>,
  <Client<B, E, Block, RA> as ProvideRuntimeApi>::Api: BlockBuilderApi<Block, Error = sp_blockchain::Error>,
{
  fn insert_aux<
    'a,
    'b: 'a,
    'c: 'a,
    I: IntoIterator<Item = &'a (&'c [u8], &'c [u8])>,
    D: IntoIterator<Item = &'a &'b [u8]>,
  >(
    &self, insert: I, delete: D,
  ) -> sp_blockchain::Result<()>
  {
    (&*self.client).insert_aux(insert, delete)
  }

  /// Query auxiliary data from key-value store.
  fn get_aux(&self, key: &[u8]) -> sp_blockchain::Result<Option<Vec<u8>>>
  {
    (&*self.client).get_aux(key)
  }
}
/// Run a HBBFT churn as a task. Provide configuration and a link to a
/// block import worker that has already been instantiated with `block_import`.
pub fn run_honey_badger<B, E, Block: BlockT<Hash = H256>, N, RA, SC, X, I, A, Sp>(
  client: Arc<Client<B, E, Block, RA>>, t_pool: Arc<A>, config: Config, network: N, on_exit: X,
  block_import: Arc<Mutex<I>>, inherent_data_providers: InherentDataProviders, selch: SC, keystore: KeyStorePtr,
  executor: Sp, receiver: ImportRx<Block>, _node_key: Option<String>, _dev_seed: Option<String>,
) -> ClientResult<impl Future<Output = ()> + Send + Unpin>
where
  Sp: futures03::task::Spawn + 'static,
  Block::Hash: Ord,
  B: Backend<Block, Blake2Hasher> + 'static,
  E: CallExecutor<Block, Blake2Hasher> + Send + Sync + 'static + Clone,
  N: RantingNetwork<Block> + Send + Sync + Unpin + Clone + 'static,
  //N::In: Send,
  SC: SelectChain<Block> + 'static + Unpin,
  NumberFor<Block>: BlockNumberOps,
  DigestFor<Block>: Encode,
  RA: Send + Sync + 'static,
  X: futures03::future::Future<Output = ()> + Send + Unpin,
  A: TransactionPool + 'static,
  <A as TransactionPool>::Block: BlockT<Hash = H256>,
  I: BlockImport<Block> + Send + Sync + 'static,
  RA: ConstructRuntimeApi<Block, Client<B, E, Block, RA>>,
  <Client<B, E, Block, RA> as ProvideRuntimeApi>::Api: BlockBuilderApi<Block, Error = sp_blockchain::Error>,
{
  let genesis_authorities_provider = &*client.clone();
  let persistent_data = aux_store::load_persistent_badger(
    &*client,
    || {
      let authorities = genesis_authorities_provider.get()?;
      telemetry!(CONSENSUS_INFO; "afg.loading_authorities";
        "authorities_len" => ?authorities.len()
      );
      Ok(authorities)
    },
    keystore.clone(),
  )?;
  info!("Badger AUTH {:?}", &persistent_data.authority_set.inner);

  //let auth_ref:Arc<parking_lot::RwLock<aux_store::AuthoritySet>>=persistent_data.authority_set.inner.clone();
  let ccln = client.clone();
  let mjust = Box::new(move |hash: &Block::Hash, justification| {
    match ccln
      .lock_import_and_run(|import_op| ccln.apply_finality(import_op, BlockId::Hash(hash.clone()), justification, true))
    {
      Ok(_) => true,
      Err(e) =>
      {
        info!("Eror finalizing  {:?}", e);
        false
      }
    }
  });

  let cclient = client.clone();
  let block_handler = BlockUtil {
    client: client.clone(),
    block_import: block_import.clone(),
    selch: selch.clone(),
  };
  let (network_bridge, network_startup) = NetworkBridge::new(
    network,
    config.clone(),
    keystore.clone(),
    persistent_data,
    client.clone(),
    mjust,
    block_handler,
    //Cwrap {
 //     client: cclient.clone(),
    //},
    cclient.clone(),
    &executor,
  );
  let net_arc = Arc::new(network_bridge);
  let blk_out = BadgerStream { wrap: net_arc.clone() };
  let txcopy = net_arc.clone();
  let tx_in_arc = txcopy.clone();
  let tx_out = TxStream {
    transaction_pool: t_pool.clone(),
    client: client.clone(),
  };
  let sender = tx_out.for_each(move |data: std::vec::Vec<std::vec::Vec<u8>>| {
    {
      match tx_in_arc.send_out(data)
      {
        Ok(_) =>
        {}
        Err(_) =>
        {
          debug!("Well, this is weird");
        }
      }
    }
    future::ready(())
  });

  let ping_sel = selch.clone();
  let sec_net = net_arc.clone();
  /*let importer = receiver.for_each(move |blki| {
    info!(
      "External block import: {:?} {:?}",
      &blki.header.hash(),
      &blki.header.number()
    );
    sec_net.on_block_imported(blki);
    future::ready(())
  });*/
  receiver.assign(Box::new(move |blki| {
    info!(
      "External block import: {:?} {:?}",
      &blki.header.hash(),
      &blki.header.number()
    );
    sec_net.on_block_imported(blki);
  }));

  let batch_receiver = blk_out.for_each(move |batch| {
    net_arc.process_batch(batch);

    future::ready(())
  });

  let with_start = network_startup.then(move |()| futures03::future::join(sender, batch_receiver));
  //   network_startup.then(move |()| futures03::future::join(futures03::future::join(sender, receiver), importer));

  let ping_client = client.clone();
  // Delay::new(Duration::from_secs(1)).then(|_| {
  let ping = interval_at(Instant::now(), Duration::from_millis(11500)).for_each(move |_| {
    //put inherents here for now TODO: decide what to do with inherents.
    let chain_head = match ping_sel.best_chain()
    {
      Ok(x) => x,
      Err(e) =>
      {
        warn!(target: "formation", "Unable to author block, no best block header: {:?}", e);
        return future::ready(());
      }
    };
    let parent_hash = chain_head.hash();
    //let  pnumber = *chain_head.number();
    let parent_id = BlockId::hash(parent_hash);

    let inherent_data = match inherent_data_providers.create_inherent_data()
    {
      Ok(id) => id,
      Err(_) => return future::ready(()), //future::err(err),
    };
    //empty for now?

    // We don't check the API versions any further here since the dispatch compatibility
    // check should be enough.
    // do this only once?
    let inh = ping_client.runtime_api().inherent_extrinsics_with_context(
      &parent_id,
      ExecutionContext::BlockConstruction,
      inherent_data,
    );
    if let Ok(res) = inh
    {
      //let mut lock = txcopy;
      info!("This many INHERS {:?}", res.len());
      for extrinsic in res
      {
        info!("INHER {:?}", &extrinsic);
        match txcopy.send_out(vec![extrinsic.encode().into_iter().collect()])
        {
          Ok(_) =>
          {}
          Err(_) =>
          {
            warn!("Error in ping sendout");
            return future::ready(());
          }
        }
      }
    }
    else
    {
      info!("Inherent panic {:?}", inh);
    }
    info!("Ping done");
    future::ready(())
  });

  //let jn = ping.merge(t_pool.clone().import_notification_stream());
  let pinged = futures03::future::select(with_start, ping);
  // Make sure that `telemetry_task` doesn't accidentally finish and kill grandpa.

  Ok(
    futures03::future::select(
      on_exit.then(|_| {
        info!("READY");
        future::ready(())
      }),
      pinged,
    )
    .then(|_| future::ready(())),
  )
}
