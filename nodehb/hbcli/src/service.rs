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

#![warn(unused_extern_crates)]

//! Service and ServiceFactory implementation. Specialized wrapper over substrate service.

use std::sync::Arc;
use std::time::Duration;

use badger::{badger_import_queue,  BadgerImportQueue,Config as BadgerConfig,run_honey_badger};
use badger_primitives::{SignatureWrap};
use client::{self, LongestChain};
use badger::{self,};// FinalityProofProvider as BadgerFinalityProofProvider,};
use badger_primitives::{PublicKeyShareWrap,SecretKeyShareWrap,SecretKeyWrap, PublicKeyWrap, PublicKeySetWrap};
use hb_node_executor;
use primitives::Pair;
use futures::prelude::*;
use hb_node_primitives::{Block};
use hb_node_runtime::{GenesisConfig, RuntimeApi};
use substrate_service::{
	FactoryFullConfiguration, LightComponents, FullComponents, FullBackend,
	FullClient, LightClient, LightBackend, FullExecutor, LightExecutor,
	error::{Error as ServiceError},
};
use transaction_pool::{self, txpool::{Pool as TransactionPool}};
use inherents::InherentDataProviders;
use network::construct_simple_protocol;
use substrate_service::construct_service_factory;
use log::info;
use substrate_service::TelemetryOnConnect;

construct_simple_protocol! {
	/// Demo protocol attachment for substrate.
	pub struct NodeProtocol where Block = Block { }
}

/// Node specific configuration
pub struct NodeConfig {
	/// grandpa connection to import block
	// FIXME #1134 rather than putting this on the config, let's have an actual intermediate setup state
	pub inherent_data_providers: InherentDataProviders,
	num_validators: usize,
	secret_key_share: Option<SecretKeyShareWrap>,
	//node_key: SecretKeyWrap,
//	public_key_set: PublicKeySetWrap,

}

impl Default for NodeConfig {
	fn default() -> NodeConfig {
		NodeConfig {
			grandpa_import_setup: None,
			inherent_data_providers: InherentDataProviders::new(),
			num_validators:4,
			secret_key_share:None,
		}
	}
}
/*

pub fn run_honey_badger<B, E, Block: BlockT<Hash=H256>, N, RA, SC, X, I,  A>(
	client : Arc<Client<B, E, Block, RA>>,
	t_pool: Arc<TransactionPool<A>>,
	config: Config,
    network:N,
    on_exit: X,
	block_import: Arc<Mutex<I>>,
	inherent_data_providers: InherentDataProviders,
	selch:SC,
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
	A: txpool::ChainApi+'static,
	I:BlockImport<Block>+Send+Sync+'static,
	RA: ConstructRuntimeApi<Block, Client<B, E, Block, RA>>,
	<Client<B, E, Block, RA> as ProvideRuntimeApi>::Api: BlockBuilderApi<Block>
*/ 
construct_service_factory! {
	struct Factory {
		Block = Block,
		RuntimeApi = RuntimeApi,
		NetworkProtocol = NodeProtocol { |config| Ok(NodeProtocol::new()) },
		RuntimeDispatch = node_executor::Executor,
		FullTransactionPoolApi = transaction_pool::ChainApi<client::Client<FullBackend<Self>, FullExecutor<Self>, Block, RuntimeApi>, Block>
			{ |config, client| Ok(TransactionPool::new(config, transaction_pool::ChainApi::new(client))) },
		LightTransactionPoolApi = transaction_pool::ChainApi<client::Client<LightBackend<Self>, LightExecutor<Self>, Block, RuntimeApi>, Block>
			{ |config, client| Ok(TransactionPool::new(config, transaction_pool::ChainApi::new(client))) },
		Genesis = GenesisConfig,
		Configuration = NodeConfig,
		FullService = FullComponents<Self>
			{ |config: FactoryFullConfiguration<Self>|
				FullComponents::<Factory>::new(config) },
		AuthoritySetup = {
			|mut service: Self::FullService| {

				    let client = service.client();
                    let t_pool = service.transaction_pool();
					let select_chain = service.select_chain()
						.ok_or(ServiceError::SelectChainRequired)?;
					let badger = run_honey_badger(
						client,
						t_pool,
						BadgerConfig::from_json_file_with_name("/store/nodess.json",&service.config.name),
						service.network(),
						service.on_exit(),
						Box::new(client.clone()), //block_import?
						service.config.custom.inherent_data_providers.clone(),
						select_chain,
					)?;
                    service.spawn_task(Box::new(badger));

				Ok(service)
			}
		},
		LightService = LightComponents<Self>
			{ |config| <LightComponents<Factory>>::new(config) },
		FullImportQueue = BadgerImportQueue<Self::Block>
			{ |config: &mut FactoryFullConfiguration<Self> , client: Arc<FullClient<Self>>, select_chain: Self::SelectChain| {
			   
			    let block_import=client.clone();
				/*let (block_import, link_half) =
					grandpa::block_import::<_, _, _, RuntimeApi, FullClient<Self>, _>(
						client.clone(), client.clone(), select_chain
					)?;*/
				let justification_import = block_import.clone();

				badger_import_queue::<_, _, PublicKeyWrap,SignatureWrap>(
					Box::new(block_import),
					Some(Box::new(justification_import)),
					None,
					None,
					client,
					config.custom.inherent_data_providers.clone(),
				).map_err(Into::into)
			}},
		LightImportQueue = BadgerImportQueue<Self::Block>
			{ |config: &FactoryFullConfiguration<Self>, client: Arc<LightClient<Self>>| {
				#[allow(deprecated)]
   			    let block_import=client.clone();
				/*let (block_import, link_half) =
					grandpa::block_import::<_, _, _, RuntimeApi, FullClient<Self>, _>(
						client.clone(), client.clone(), select_chain
					)?;*/
				let justification_import = block_import.clone();
				badger_import_queue::<_, _, PublicKeyWrap,SignatureWrap>(
					Box::new(block_import),
					Some(Box::new(justification_import)),
					None,
					None,
					client,
					config.custom.inherent_data_providers.clone(),
				).map_err(Into::into)
			}},
		SelectChain = LongestChain<FullBackend<Self>, Self::Block>
			{ |config: &FactoryFullConfiguration<Self>, client: Arc<FullClient<Self>>| {
				#[allow(deprecated)]
				Ok(LongestChain::new(client.backend().clone()))
			}
		},
		FinalityProofProvider = { |_client: Arc<FullClient<Self>>| {
			Ok(None)
		}},
	}
}

