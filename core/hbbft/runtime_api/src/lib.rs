#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;
use client::decl_runtime_apis;

decl_runtime_apis! {
    
    #[api_version(2)]
    pub trait HbbftApi {
    }
}

/*
#[cfg(feature = "std")]
use serde::Serialize;


use parity_codec::{Encode, Decode, Codec,Input};
use client::decl_runtime_apis;
use sr_primitives::{ConsensusEngineId, traits::{DigestFor, NumberFor}};

pub type PAuthorityId=([u8;32],[u8;32]);
/// The index of an authority.
pub type AuthorityIndex = u64;

/// A scheduled change of authority set.
#[cfg_attr(feature = "std", derive(Debug, Serialize))]
#[derive(Clone, Eq, PartialEq, Encode, Decode)]
pub struct ScheduledChange<N> {
	/// The new authorities after the change, along with their respective weights.
	pub next_authorities: Vec<PAuthorityId>,
	/// The number of blocks to delay.
	pub delay: N,
}

/// An consensus log item for HBBFT..?.
#[cfg_attr(feature = "std", derive(Debug, Serialize))]
#[derive(Decode, Encode, PartialEq, Eq, Clone)]
pub enum ConsensusLog<N: Codec> {
	/// Schedule an authority set change.
	///
	/// Precedence towards earlier or later digest items can be given
	/// based on the rules of the chain.
	///
	/// No change should be scheduled if one is already and the delay has not
	/// passed completely.
	///
	/// This should be a pure function: i.e. as long as the runtime can interpret
	/// the digest type it should return the same result regardless of the current
	/// state.
	#[codec(index = "1")]
	ScheduledChange(ScheduledChange<N>),
	/// Force an authority set change.
	///
	/// Forced changes are applied after a delay of _imported_ blocks,
	/// while pending changes are applied after a delay of _finalized_ blocks.
	///
	/// Precedence towards earlier or later digest items can be given
	/// based on the rules of the chain.
	///
	/// No change should be scheduled if one is already and the delay has not
	/// passed completely.
	///
	/// This should be a pure function: i.e. as long as the runtime can interpret
	/// the digest type it should return the same result regardless of the current
	/// state.
	#[codec(index = "2")]
	ForcedChange(N, ScheduledChange<N>),
	/// Note that the authority with given index is disabled until the next change.
	#[codec(index = "3")]
	OnDisabled(AuthorityIndex),
}

impl<N: Codec> ConsensusLog<N> {
	/// Try to cast the log entry as a contained signal.
	pub fn try_into_change(self) -> Option<ScheduledChange<N>> {
		match self {
			ConsensusLog::ScheduledChange(change) => Some(change),
			ConsensusLog::ForcedChange(_, _) | ConsensusLog::OnDisabled(_) => None,
		}
	}

	/// Try to cast the log entry as a contained forced signal.
	pub fn try_into_forced_change(self) -> Option<(N, ScheduledChange<N>)> {
		match self {
			ConsensusLog::ForcedChange(median, change) => Some((median, change)),
			ConsensusLog::ScheduledChange(_) | ConsensusLog::OnDisabled(_) => None,
		}
	}
}

/// WASM function call to check for pending changes.
pub const PENDING_CHANGE_CALL: &str = "badger_pending_change";
/// WASM function call to get current GRANDPA authorities.
pub const AUTHORITIES_CALL: &str = "badger_authorities"; */
/*
decl_runtime_apis! {
	/// APIs for integrating the Badger finality gadget into runtimes.
	/// This should be implemented on the runtime side.
	///
	/// This is primarily used for negotiating authority-set changes for the
	/// gadget. GRANDPA uses a signaling model of changing authority sets:
	/// changes should be signaled with a delay of N blocks, and then automatically
	/// applied in the runtime after those N blocks have passed.
	///
	/// The consensus protocol will coordinate the handoff externally.
	#[api_version(2)]
	pub trait HbbftApi {
		/// Check a digest for pending changes.
		/// Return `None` if there are no pending changes.
		///
		/// Precedence towards earlier or later digest items can be given
		/// based on the rules of the chain.
		///
		/// No change should be scheduled if one is already and the delay has not
		/// passed completely.
		///
		/// This should be a pure function: i.e. as long as the runtime can interpret
		/// the digest type it should return the same result regardless of the current
		/// state.
		fn badger_pending_change(digest: &DigestFor<Block>)
			-> Option<ScheduledChange<NumberFor<Block>>>;

		/// Check a digest for forced changes.
		/// Return `None` if there are no forced changes. Otherwise, return a
		/// tuple containing the pending change and the median last finalized
		/// block number at the time the change was signaled.
		///
		/// Added in version 2.
		///
		/// Forced changes are applied after a delay of _imported_ blocks,
		/// while pending changes are applied after a delay of _finalized_ blocks.
		///
		/// Precedence towards earlier or later digest items can be given
		/// based on the rules of the chain.
		///
		/// No change should be scheduled if one is already and the delay has not
		/// passed completely.
		///
		/// This should be a pure function: i.e. as long as the runtime can interpret
		/// the digest type it should return the same result regardless of the current
		/// state.
		fn badger_forced_change(digest: &DigestFor<Block>)
			-> Option<(NumberFor<Block>, ScheduledChange<NumberFor<Block>>)>;

		/// Get the current GRANDPA authorities and weights. This should not change except
		/// for when changes are scheduled and the corresponding delay has passed.
		///
		/// When called at block B, it will return the set of authorities that should be
		/// used to finalize descendants of this block (B+1, B+2, ...). The block B itself
		/// is finalized by the authorities from block B-1.
		fn badger_authorities() -> Vec<PAuthorityId>;
	}
}
*/