#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;


pub mod app {
	use sr_primitives::app_crypto::{app_crypto, hbbft_thresh, key_types::HB_NODE};
	app_crypto!(hbbft_thresh, HB_NODE);
}

#[cfg(feature = "std")]
pub type AuthorityPair = app::Pair;

pub type AuthorityId = app::Public;

pub type AuthoritySignature = app::Signature;

pub const HBBFT_ENGINE_ID: ConsensusEngineId = *b"BDGR";
