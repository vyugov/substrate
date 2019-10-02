// Copyright 2017-2019 Parity Technologies (UK) Ltd.
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

//! Badger Consensus module for runtime.
//!
//! This manages the Badger authority set ready for the native code, or does it? We'll see.
//! These authorities are only for GRANDPA finality, not for consensus overall.
//!
//! In the future, it will also handle misbehavior reports, and on-chain
//! finality notifications.
//!
//! For full integration with GRANDPA, the `GrandpaApi` should be implemented.
//! The necessary items are re-exported via the `fg_primitives` crate.

#![cfg_attr(not(feature = "std"), no_std)]

// re-export since this is necessary for `impl_apis` in runtime.
//pub use substrate_badger_primitives as fg_primitives;

use codec::{self as codec, Decode, Encode, Error,Codec};
use rstd::prelude::*;
use sr_primitives::{
  generic::{DigestItem, OpaqueDigestItemId},
  traits::Zero,
  Perbill,
};
use srml_support::{
  decl_event, decl_module, decl_storage, dispatch::Result, storage::StorageMap,
  storage::StorageValue,
};
use substrate_mpecdsa_primitives::{MP_ECDSA_ENGINE_ID,ConsensusLog,MAIN_DB_PREFIX};


#[cfg(feature = "std")]
use serde::{Serialize,Deserialize};

use sr_primitives::ConsensusEngineId;


//use fg_primitives::HBBFT_ENGINE_ID;
//pub use fg_primitives::{AuthorityId, ConsensusLog};
use system::{ensure_signed, DigestOf};

//#[derive(Decode, Encode, PartialEq, Eq, Clone,Hash)]
pub type AuthorityId = ([u8; 32],[u8; 16]);





mod mock;

pub trait Trait: system::Trait
{
  /// The event type of this module.
  type Event: From<Event> + Into<<Self as system::Trait>::Event>;
}



decl_event!(
// #[derive(Debug)]
  pub enum Event
  {
    /// New authority set has been applied.
    NewAuthorities(Vec<AuthorityId>),
  }
);

decl_storage! {
  trait Store for Module<T: Trait> as BadgerFinality {
    /// The current authority set.
    ReqIds get(requests): Vec<u64>;
    RequestResults: map u64 => Option<Option<[u8;32]>>;
    //CurrentSetId get(current_set_id) build(|_| 0): u64;
  
  }
  add_extra_genesis {
  //  config(requests): Vec<u64>;
  //  build(|config| Module::<T>::initialize_authorities(&config.authorities))
  }
}

decl_module! {
  pub struct Module<T: Trait> for enum Call where origin: T::Origin {
    fn deposit_event() = default;

    fn on_finalize(block_number: T::BlockNumber) {

          //TODO: check if I need to add anything.

        }

    //	Self::deposit_log(ConsensusLog::Resume(delay));
	fn send_log(origin,req_id:u64) ->Result
	{
	let who =	ensure_signed(origin)?;
  let mut a:Vec<u8>=MAIN_DB_PREFIX.to_vec();
  let mut b=req_id.encode();
  a.append(&mut b);
  	let kind = primitives::offchain::StorageKind::PERSISTENT;
  runtime_io::local_storage_set(kind,&a,b"new");
    if <Self as Store>::RequestResults::exists(req_id)
    {
      return Err("Duplicate request ID");
    }
    let a:Option<[u8;32]>=None;
    <Self as Store>::RequestResults::insert(req_id,&a);
    Self::deposit_log(ConsensusLog::RequestForKeygen((req_id, [2;32].to_vec())));
	Ok(())
	}
		
  }
}

impl<T: Trait> Module<T>
{
  
  
  /// Deposit one of this module's logs.
  fn deposit_log(log: ConsensusLog)
  {
    let log: DigestItem<T::Hash> = DigestItem::Consensus(MP_ECDSA_ENGINE_ID, log.encode());
    <system::Module<T>>::deposit_log(log.into());
  }


}

impl<T: Trait> Module<T>
{
  /// Attempt to extract a Keygen log from a generic digest.
  pub fn keygen_log(digest: &DigestOf<T>) -> Option<ConsensusLog>
  {
    let id = OpaqueDigestItemId::Consensus(&MP_ECDSA_ENGINE_ID);
    digest.convert_first(|l| l.try_to::<ConsensusLog>(id))
  }

    
}

