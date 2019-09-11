#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

use client::decl_runtime_apis;
use codec::{Codec, Decode, Encode, Error as CodecError, Input};
use rstd::vec::Vec;
use sr_primitives::{
	traits::{DigestFor, NumberFor},
	ConsensusEngineId,
};

pub const MP_ECDSA_ENGINE_ID: ConsensusEngineId = *b"MPEC";

pub const GET_THRESHOLD_SIGNATURE_CALL: &str = "get_threshold_signature";

decl_runtime_apis! { // TODO implement srml module
	#[api_version(2)]
	pub trait MpecApi {
		fn get_threshold_signature(data :Vec<u8>) -> Vec<u8>;
	}
}
