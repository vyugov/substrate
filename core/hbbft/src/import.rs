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

use std::{sync::Arc, collections::HashMap};

use log::{debug, trace, info};
use parity_codec::Encode;
use futures::sync::mpsc;
use parking_lot::RwLockWriteGuard;

use client::{blockchain, CallExecutor, Client};
use client::blockchain::HeaderBackend;
use client::backend::Backend;
use client::runtime_api::ApiExt;
use consensus_common::{
	BlockImport, Error as ConsensusError,
	ImportBlock, ImportResult, JustificationImport, well_known_cache_keys,
	SelectChain,
};
use fg_primitives::HbbftApi;
use runtime_primitives::Justification;
use runtime_primitives::generic::BlockId;
use runtime_primitives::traits::{
	Block as BlockT, DigestFor,
	Header as HeaderT, NumberFor, ProvideRuntimeApi,
};
use substrate_primitives::{H256, Blake2Hasher};

use crate::{Error, CommandOrError, NewAuthoritySet, VoterCommand};
use crate::authorities::{AuthoritySet, SharedAuthoritySet, DelayKind, PendingChange};
use crate::consensus_changes::SharedConsensusChanges;
use crate::environment::{finalize_block, is_descendent_of};
use crate::justification::GrandpaJustification;

/// A block-import handler for GRANDPA.
///
/// This scans each imported block for signals of changing authority set.
/// If the block being imported enacts an authority set change then:
/// - If the current authority set is still live: we import the block
/// - Otherwise, the block must include a valid justification.
///
/// When using GRANDPA, the block import worker should be using this block import
/// object.
pub struct BadgerBlockImport<B, E, Block: BlockT<Hash=H256>, RA, PRA, SC> {
	inner: Arc<Client<B, E, Block, RA>>,
	select_chain: SC,
	send_voter_commands: mpsc::UnboundedSender<VoterCommand<Block::Hash, NumberFor<Block>>>,
	consensus_changes: SharedConsensusChanges<Block::Hash, NumberFor<Block>>,
	api: Arc<PRA>,
}

impl<B, E, Block: BlockT<Hash=H256>, RA, PRA, SC> JustificationImport<Block>
	for BadgerBlockImport<B, E, Block, RA, PRA, SC> where
		NumberFor<Block>: grandpa::BlockNumberOps,
		B: Backend<Block, Blake2Hasher> + 'static,
		E: CallExecutor<Block, Blake2Hasher> + 'static + Clone + Send + Sync,
		DigestFor<Block>: Encode,
		RA: Send + Sync,
		PRA: ProvideRuntimeApi,
		PRA::Api: HbbftApi<Block>,
		SC: SelectChain<Block>,
{
	type Error = ConsensusError;

	fn on_start(&self) -> Vec<(Block::Hash, NumberFor<Block>)> {
		let mut out = Vec::new();
		out
	}

	fn import_justification(
		&self,
		hash: Block::Hash,
		number: NumberFor<Block>,
		justification: Justification,
	) -> Result<(), Self::Error> {
		self.import_justification(hash, number, justification, false)
	}
}

enum AppliedChanges<H, N> {
	Standard(bool), // true if the change is ready to be applied (i.e. it's a root)
	Forced(NewAuthoritySet<H, N>),
	None,
}

impl<H, N> AppliedChanges<H, N> {
	fn needs_justification(&self) -> bool {
		match *self {
			AppliedChanges::Standard(_) => true,
			AppliedChanges::Forced(_) | AppliedChanges::None => false,
		}
	}
}



impl<'a, Block: 'a + BlockT> PendingSetChanges<'a, Block> {
	// revert the pending set change explicitly.
	fn revert(self) { }

	fn defuse(mut self) -> (AppliedChanges<Block::Hash, NumberFor<Block>>, bool) {
		self.just_in_case = None;
		let applied_changes = ::std::mem::replace(&mut self.applied_changes, AppliedChanges::None);
		(applied_changes, self.do_pause)
	}
}

impl<'a, Block: 'a + BlockT> Drop for PendingSetChanges<'a, Block> {
	fn drop(&mut self) {
		if let Some((old_set, mut authorities)) = self.just_in_case.take() {
			*authorities = old_set;
		}
	}
}



impl<B, E, Block: BlockT<Hash=H256>, RA, PRA, SC> BlockImport<Block>
	for BadgerBlockImport<B, E, Block, RA, PRA, SC> where
		NumberFor<Block>: grandpa::BlockNumberOps,
		B: Backend<Block, Blake2Hasher> + 'static,
		E: CallExecutor<Block, Blake2Hasher> + 'static + Clone + Send + Sync,
		DigestFor<Block>: Encode,
		RA: Send + Sync,
		PRA: ProvideRuntimeApi,
		PRA::Api: HbbftApi<Block>,
{
	type Error = ConsensusError;

	fn import_block(&self, mut block: ImportBlock<Block>, new_cache: HashMap<well_known_cache_keys::Id, Vec<u8>>)
		-> Result<ImportResult, Self::Error>
	{
		let hash = block.post_header().hash();
		let number = block.header.number().clone(); 

		// early exit if block already in chain, otherwise the check for
		// authority changes will error when trying to re-import a change block
		#[allow(deprecated)]
		match self.inner.backend().blockchain().status(BlockId::Hash(hash)) {
			Ok(blockchain::BlockStatus::InChain) => return Ok(ImportResult::AlreadyInChain),
			Ok(blockchain::BlockStatus::Unknown) => {},
			Err(e) => return Err(ConsensusError::ClientImport(e.to_string()).into()),
		}

		// we don't want to finalize on `inner.import_block`
		let mut justification = block.justification.take();
		let enacts_consensus_change = !new_cache.is_empty();
		let import_result = self.inner.import_block(block, new_cache);

		let mut imported_aux = {
			match import_result {
				Ok(ImportResult::Imported(aux)) => aux,
				Ok(r) => {
					debug!(target: "afg", "Restoring old authority set after block import result: {:?}", r);
					return Ok(r);
				},
				Err(e) => {
					debug!(target: "afg", "Restoring old authority set after block import error: {:?}", e);
						return Err(ConsensusError::ClientImport(e.to_string()).into());
				},
			}
		};
    
		Ok(ImportResult::Imported(imported_aux))
	}

	fn check_block(
		&self,
		hash: Block::Hash,
		parent_hash: Block::Hash,
	) -> Result<ImportResult, Self::Error> {
		self.inner.check_block(hash, parent_hash)
	}
}

impl<B, E, Block: BlockT<Hash=H256>, RA, PRA, SC>
	BadgerBlockImport<B, E, Block, RA, PRA, SC>
{
	pub(crate) fn new(
		inner: Arc<Client<B, E, Block, RA>>,
		select_chain: SC,
		send_voter_commands: mpsc::UnboundedSender<VoterCommand<Block::Hash, NumberFor<Block>>>,
		consensus_changes: SharedConsensusChanges<Block::Hash, NumberFor<Block>>,
		api: Arc<PRA>,
	) -> BadgerBlockImport<B, E, Block, RA, PRA, SC> {
		BadgerBlockImport {
			inner,
			select_chain,
			send_voter_commands,
			consensus_changes,
			api,
		}
	}
}

impl<B, E, Block: BlockT<Hash=H256>, RA, PRA, SC>
	BadgerBlockImport<B, E, Block, RA, PRA, SC>
where
	NumberFor<Block>: grandpa::BlockNumberOps,
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + 'static + Clone + Send + Sync,
	RA: Send + Sync,
{

	/// Import a block justification and finalize the block.
	///
	/// If `enacts_change` is set to true, then finalizing this block *must*
	/// enact an authority set change, the function will panic otherwise.
	fn import_justification(
		&self,
		hash: Block::Hash,
		number: NumberFor<Block>,
		justification: Justification,
		enacts_change: bool,
	) -> Result<(), ConsensusError> {
		let justification = GrandpaJustification::decode_and_verify_finalizes(
			&justification,
			(hash, number),
			self.authority_set.set_id(),
			&self.authority_set.current_authorities(),
		);

		let justification = match justification {
			Err(e) => return Err(ConsensusError::ClientImport(e.to_string()).into()),
			Ok(justification) => justification,
		};

		let result = finalize_block(
			&*self.inner,
			&self.authority_set,
			&self.consensus_changes,
			None,
			hash,
			number,
			justification.into(),
		);

		match result {
			Err(CommandOrError::VoterCommand(command)) => {
				info!(target: "finality", "Imported justification for block #{} that triggers \
					command {}, signaling voter.", number, command);

				if let Err(e) = self.send_voter_commands.unbounded_send(command) {
					return Err(ConsensusError::ClientImport(e.to_string()).into());
				}
			},
			Err(CommandOrError::Error(e)) => {
				return Err(match e {
					Error::Grandpa(error) => ConsensusError::ClientImport(error.to_string()),
					Error::Network(error) => ConsensusError::ClientImport(error),
					Error::Blockchain(error) => ConsensusError::ClientImport(error),
					Error::Client(error) => ConsensusError::ClientImport(error.to_string()),
					Error::Safety(error) => ConsensusError::ClientImport(error),
					Error::Timer(error) => ConsensusError::ClientImport(error.to_string()),
				}.into());
			},
			Ok(_) => {
				assert!(!enacts_change, "returns Ok when no authority set change should be enacted; qed;");
			},
		}

		Ok(())
	}
}
