#![cfg_attr(not(feature = "std"), no_std)]
#![allow(clippy::type_complexity)]

use codec::{Decode, Encode};

// use primitives::offchain::StorageKind;
use rstd::prelude::*;
use runtime_io::storage::{get as local_storage_get};
use sp_mpc::{ConsensusLog, MPC_ENGINE_ID};
use sp_runtime::{
	generic::DigestItem,
	traits::{Member, One, SimpleArithmetic, StaticLookup, Zero},
	RuntimeDebug,
};
use support::{
	debug, decl_event, decl_module, decl_storage, dispatch::Result, ensure, traits::Time, Parameter,
};
use system::ensure_signed;

pub trait Trait: system::Trait {
	type Event: From<Event<Self>> + Into<<Self as system::Trait>::Event>;
}

decl_storage! {
	trait Store for Module<T: Trait> as Mpc {
		Results get(fn result_of): map u64 => Vec<u8>;
	}
}

decl_module! {
	pub struct Module<T: Trait> for enum Call where origin: T::Origin {
		fn deposit_event() = default;

		fn send_log(origin) {
			ensure_signed(origin)?;
			Self::_send_log();
		}

		fn offchain_worker(_block_number: T::BlockNumber) {
			debug::RuntimeLogger::init();
			if let Some(value) = local_storage_get(&[1u8]) {
				<Results>::insert(1, value);
				debug::warn!("insert ok");
			} else {
				debug::warn!("nothing");
			}
		}
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

impl<T: Trait> Module<T> {
	fn _send_log() {
		Self::_deposit_log(ConsensusLog::RequestForKeygen(1, [0u8].to_vec()));
	}

	fn _deposit_log(log: ConsensusLog) {
		let log: DigestItem<T::Hash> = DigestItem::Consensus(MPC_ENGINE_ID, log.encode());
		<system::Module<T>>::deposit_log(log.into());
	}
}
