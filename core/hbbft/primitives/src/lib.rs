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

//! Primitives for GRANDPA integration, suitable for WASM compilation.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(feature = "std")]
use serde::Serialize;

#[cfg(feature = "std")]
use serde::Deserialize;
//#[cfg(feature = "std")]
//use serde::Deserialize;


use parity_codec::{Encode, Decode, Codec,Input};
use sr_primitives::{ConsensusEngineId, traits::{DigestFor, NumberFor}};
use client::decl_runtime_apis;
use rstd::vec::Vec;
use badger::crypto::{ PublicKey, PublicKeySet, PublicKeyShare, SecretKey, SecretKeyShare,Signature,};
/// The hbbft crypto scheme defined via the keypair type.
#[cfg(feature = "std")]
pub type AuthorityPair = (PublicKey,SecretKey);



use bincode;
use threshold_crypto::serde_impl::SerdeSecret;


pub const SK_SIZE: usize = 32;


#[derive(Debug,Clone,PartialEq,Eq)]
pub struct SecretKeyWrap(pub SecretKey);

impl parity_codec::Encode for SecretKeyWrap {
 
	fn encode_to<T: parity_codec::Output>(&self, dest: &mut T) {
		//let len = <C::Affine as CurveAffine>::Compressed::size();
		 for bt in bincode::serialize(&SerdeSecret(&self.0)).expect("Falied serialize").iter()
		 {
			 dest.push_byte(*bt);
		 }
	
	}
}
impl parity_codec::Decode for SecretKeyWrap {
  fn decode<I: Input>(value: &mut I) -> Result<Self,parity_codec::Error>
  {
	   let mut mt:[u8; SK_SIZE]= [0;SK_SIZE ];
	  match value.read(&mut mt)
	  {
		  Ok(_) => {},
		  Err(_) => return Err( "Error decoding field SecretKeyWrap".into())
		}

   match bincode::deserialize(&mt)
   {
	   Ok(val) => Ok(Self { 0: val}),
	   Err(_)  => Err( "Error decoding field SecretKeyWrap".into())
   }
  }
}

#[derive(Debug,Serialize,Deserialize,Clone,PartialEq,Eq,Hash)]
pub struct PublicKeySetWrap( pub PublicKeySet);


#[derive(Debug,Serialize,Deserialize,Clone,PartialEq,Eq,Hash)]
pub struct PublicKeyWrap(pub PublicKey);

use badger::crypto::PK_SIZE;
use badger::crypto::SIG_SIZE;


//use badger::pairing::{CurveAffine, CurveProjective, EncodedPoint};

impl parity_codec::Encode for PublicKeyWrap {
 
	fn encode_to<T: parity_codec::Output>(&self, dest: &mut T) {
		//let len = <C::Affine as CurveAffine>::Compressed::size();
      for byte in self.0.to_bytes().iter()
	 {
		dest.push_byte(*byte);
	 }
	}
}
impl parity_codec::Decode for PublicKeyWrap {
  fn decode<I: Input>(value: &mut I) -> Result<Self,parity_codec::Error>
  {
	  let mut mt:[u8; PK_SIZE]= [0;PK_SIZE ];
	  match value.read(&mut mt)
	  {
		  Ok(_) => {},
		  Err(_) => return Err( "Error decoding field SecretKeyWrap".into())
		}
		match PublicKey::from_bytes(&mt) 
		{
        Ok(pk) => Ok(Self { 0: pk}),
        Err(_) =>  Err( "Error decoding field PublicKeyWrap".into())
		}
  
  }
}

#[derive(Debug,Serialize,Clone,PartialEq,Eq,Hash)]
pub struct PublicKeyShareWrap(PublicKeyShare);


impl parity_codec::Encode for PublicKeyShareWrap {
 
	fn encode_to<T: parity_codec::Output>(&self, dest: &mut T) {
		//let len = <C::Affine as CurveAffine>::Compressed::size();
      for byte in self.0.to_bytes().iter()
	 {
		dest.push_byte(*byte);
	 }
	}
}
impl parity_codec::Decode for PublicKeyShareWrap {
  fn decode<I: Input>(value: &mut I) -> Result<Self,parity_codec::Error>
  {
	  let mut mt:[u8; PK_SIZE]= [0;PK_SIZE ];
	  match value.read(&mut mt)
	  {
		  Ok(_) => {},
		  Err(_) => return Err( "Error decoding field SecretKeyWrap".into())
		}
		match PublicKeyShare::from_bytes(&mt) 
		{
        Ok(pk) => Ok(Self { 0: pk}),
        Err(_) => Err( "Error decoding field PublicKeyShareWrap".into())
		}
  
  }
}

#[derive(Debug,Clone,PartialEq,Eq)]
pub struct SecretKeyShareWrap(pub SecretKeyShare);

impl parity_codec::Encode for SecretKeyShareWrap 
{ 
	fn encode_to<T: parity_codec::Output>(&self, dest: &mut T) 
	{
     for bt in bincode::serialize(&SerdeSecret(&self.0)).expect("Falied serialize").iter()
		 {
			 dest.push_byte(*bt);
		 }
	}
}
impl parity_codec::Decode for SecretKeyShareWrap {
  fn decode<I: Input>(value: &mut I) -> Result<Self,parity_codec::Error>
  {
	let mut mt:[u8; SK_SIZE]= [0;SK_SIZE ];
	  match value.read(&mut mt)
	  {
		  Ok(_) => {},
		  Err(_) => return Err( "Error decoding field SecretKeyWrap".into())
		}
   match bincode::deserialize(&mt)
   {
	   Ok(val) => Ok(Self { 0: val}),
	   Err(_)  => Err( "Error decoding field SecretKeyShareWrap".into())
   }
  
  }
}

/// Identity of a HBBFT  proposer.
pub type AuthorityId = PublicKeyWrap;


#[derive(Debug,Serialize,Clone,PartialEq,Eq,Hash)]
pub struct SignatureWrap(pub Signature);


impl parity_codec::Encode for SignatureWrap {
 
	fn encode_to<T: parity_codec::Output>(&self, dest: &mut T) {
		//let len = <C::Affine as CurveAffine>::Compressed::size();
      for byte in self.0.to_bytes().iter()
	 {
		dest.push_byte(*byte);
	 }
	}
}
impl parity_codec::Decode for SignatureWrap {
  fn decode<I: Input>(value: &mut I) -> Result<Self,parity_codec::Error>
  {
	  let mut mt:[u8; SIG_SIZE]= [0;SIG_SIZE ];
	  match value.read(&mut mt)
	  {
		  Ok(_) => {},
		  Err(_) => return Err( "Error decoding field SecretKeyWrap".into())
		}
		match Signature::from_bytes(&mt) 
		{
        Ok(pk) => Ok(Self { 0: pk}),
        Err(_) => Err( "Error decoding field SignatureWrap".into())
		}
  
  }
}

/*pub struct NetworkInfo<N> {
    /// This node's ID.
    our_id: N,
    /// The number _N_ of nodes in the network. Equal to the size of `public_keys`.
    num_nodes: usize,
    /// The number _f_ of faulty nodes that can be tolerated. Less than a third of _N_.
    num_faulty: usize,
    /// Whether this node is a validator. This is true if `public_keys` contains our own ID.
    is_validator: bool,
    /// This node's secret key share. Only validators have one.
    secret_key_share: Option<SecretKeyShare>,
    /// This node's secret key.
    secret_key: SecretKey,
    /// The public key set for threshold cryptography. Each validator has a secret key share.
    public_key_set: PublicKeySet,
    /// The validators' public key shares, computed from `public_key_set`.
    public_key_shares: BTreeMap<N, PublicKeyShare>,
    /// The validators' public keys.
    public_keys: BTreeMap<N, PublicKey>,
    /// The indices in the list of sorted validator IDs.
    node_indices: BTreeMap<N, usize>,
}
*/

/// Signature for a HBBFT  proposer.
pub type AuthoritySignature = SignatureWrap;

/// The `ConsensusEngineId` of GRANDPA.
pub const HBBFT_ENGINE_ID: ConsensusEngineId = *b"BDGR";

/// The weight of an authority.
//pub type AuthorityWeight = u64;

/// The index of an authority.
pub type AuthorityIndex = u64;

/// A scheduled change of authority set.
#[cfg_attr(feature = "std", derive(Debug, Serialize))]
#[derive(Clone, Eq, PartialEq, Encode, Decode)]
pub struct ScheduledChange<N> {
	/// The new authorities after the change, along with their respective weights.
	pub next_authorities: Vec<AuthorityId>,
	/// The number of blocks to delay.
	pub delay: N,
}

/// An consensus log item for HBBFT..?.
#[cfg_attr(feature = "std", derive(Serialize, Debug))]
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
pub const AUTHORITIES_CALL: &str = "badger_authorities";

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
		fn badger_authorities() -> Vec<AuthorityId>;
	}
}
