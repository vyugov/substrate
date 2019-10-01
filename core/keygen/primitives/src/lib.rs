#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(feature = "std")]
use serde::Serialize;

use client::decl_runtime_apis;
use codec::{Codec, Decode, Encode, Error as CodecError, Input};
use rstd::vec::Vec;
use sr_primitives::{
	traits::{DigestFor, NumberFor},
	ConsensusEngineId,
};

pub const MP_ECDSA_ENGINE_ID: ConsensusEngineId = *b"MPEC";

pub type AuthorityWeight = u64;

pub type AuthorityIndex = u64;


#[cfg_attr(feature = "std", derive(Serialize, Debug))]
#[derive(Decode, Encode, PartialEq, Eq, Clone)]
pub enum ConsensusLog {

	///  Request for a new key to be generated, with provided requestid
	#[codec(index = "1")]
	RequestForKeygen( (u64,Vec<u8>)),

}

impl ConsensusLog  {
	/// Try to cast the log entry as a contained signal.
	pub fn try_into_vec(self) -> Option< (u64,Vec<u8>) > {
		match self {
			ConsensusLog::RequestForKeygen( ( id,change) ) => Some((id,change)),
			_ => None,
		}
	}

	
}

pub const PENDING_CHANGE_CALL: &str = "hbbft_pending_change";
pub const AUTHORITIES_CALL: &str = "hbbft_authorities";
pub const GET_THRESHOLD_SIGNATURE_CALL: &str = "get_threshold_signature";

decl_runtime_apis! { // TODO implement srml module
	#[api_version(2)]
	pub trait MpecApi {
		fn get_threshold_signature(data :Vec<u8>) -> Vec<u8>;
	}
}
