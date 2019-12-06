#![cfg_attr(not(feature = "std"), no_std)]
#![allow(clippy::type_complexity)]
use codec::{Decode, Encode};

use rstd::prelude::*;
use runtime_io::hashing::sha2_256;
use sp_runtime::{
	traits::{Member, One, SimpleArithmetic, StaticLookup, Zero},
	RuntimeDebug,
};
use support::{
	decl_event, decl_module, decl_storage, dispatch::Result, ensure, traits::Time, Parameter,
};
use system::ensure_signed;

pub trait Trait: system::Trait {
	type Event: From<Event<Self>> + Into<<Self as system::Trait>::Event>;
}

decl_storage! {
	trait Store for Module<T: Trait> as Mpc {}
}

decl_module! {
	pub struct Module<T: Trait> for enum Call where origin: T::Origin {
		fn deposit_event() = default;

	}
}

decl_event!(
	pub enum Event<T>
	where
		AccountId = <T as system::Trait>::AccountId,
	{
		SomethingStored(u32, AccountId),
	}
);
