#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

use hbbft::crypto::{PublicKey as HBPublicKey, SecretKey, Signature, PK_SIZE};

use client::decl_runtime_apis;
use codec::{Codec, Decode, Encode, Error as CodecError, Input};
use rstd::vec::Vec;
#[cfg(feature = "std")]
use serde::{Deserialize, Serialize};
use sr_primitives::{
	traits::{DigestFor, NumberFor},
	ConsensusEngineId,
};

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug, Serialize, Deserialize)]
pub struct PublicKey(HBPublicKey);

impl Default for PublicKey {
	fn default() -> Self {
		Self(HBPublicKey::from_bytes([0u8; PK_SIZE]).unwrap())
	}
}

#[cfg(feature = "std")]
impl Encode for PublicKey {
	fn encode(&self) -> Vec<u8> {
		Encode::encode(&self.0.to_bytes())
	}
}

#[cfg(feature = "std")]
impl Decode for PublicKey {
	fn decode<I: Input>(input: &mut I) -> Result<Self, CodecError> {
		let input_bytes = Decode::decode(input)?;
		match HBPublicKey::from_bytes(&input_bytes) {
			Ok(p) => Ok(PublicKey(p)),
			Err(_) => Err(CodecError::from("Decode public key error")),
		}
	}
}

#[derive(Debug, Default)] // we derive Default in order to use the clear() method in Drop
pub struct Keypair {
	/// The secret half of this keypair.
	pub secret: SecretKey,
	/// The public half of this keypair.
	pub public: PublicKey,
}

#[cfg(feature = "std")]
pub type AuthorityPair = Keypair;

pub type AuthorityId = PublicKey;

pub type AuthoritySignature = Signature;

pub const MP_ECDSA_ENGINE_ID: ConsensusEngineId = *b"MPEC";

pub type AuthorityWeight = u64;

pub type AuthorityIndex = u64;

/// A scheduled change of authority set.
#[cfg_attr(feature = "std", derive(Debug, Serialize))]
#[derive(Clone, Eq, PartialEq, Encode, Decode)]
pub struct ScheduledChange<N> {
	/// The new authorities after the change, along with their respective weights.
	pub next_authorities: Vec<(AuthorityId, AuthorityWeight)>,
	/// The number of blocks to delay.
	pub delay: N,
}

#[cfg_attr(feature = "std", derive(Serialize, Debug))]
#[derive(Decode, Encode, PartialEq, Eq, Clone)]
pub enum ConsensusLog<N: Codec> {

	///  Request for a new key to be generated, with provided requestid
	#[codec(index = "1")]
	RequestForKeygen(Vec<u8>),

}

impl<N: Codec> ConsensusLog<N> {
	/// Try to cast the log entry as a contained signal.
	pub fn try_into_vec(self) -> Option<Vec<u8>> {
		match self {
			ConsensusLog::RequestForKeygen(change) => Some(change),
			_ => None,
		}
	}

	
}

pub const PENDING_CHANGE_CALL: &str = "hbbft_pending_change";
pub const AUTHORITIES_CALL: &str = "hbbft_authorities";
pub const GET_THRESHOLD_SIGNATURE_CALL: &str = "get_threshold_signature";

decl_runtime_apis! {
	/// APIs for integrating the GRANDPA finality gadget into runtimes.
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
		fn hbbft_pending_change(digest: &DigestFor<Block>)
			-> Option<ScheduledChange<NumberFor<Block>>>;


		fn hbbft_forced_change(digest: &DigestFor<Block>)
			-> Option<(NumberFor<Block>, ScheduledChange<NumberFor<Block>>)>;


		fn hbbft_authorities() -> Vec<(AuthorityId, AuthorityWeight)>;

		fn get_threshold_signature();
	}
}
