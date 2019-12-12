#![cfg_attr(not(feature = "std"), no_std)]
#![allow(clippy::type_complexity)]

use codec::{Decode, Encode};

use app_crypto::RuntimeAppPublic;
use primitives::{crypto::KeyTypeId, offchain::StorageKind};
use rstd::prelude::*;
use runtime_io::offchain::local_storage_get;
use sp_mpc::{ConsensusLog, MPC_ENGINE_ID};
use sp_runtime::{
	generic::DigestItem,
	traits::{IdentifyAccount, Member, One, SimpleArithmetic, StaticLookup, Zero},
	RuntimeDebug,
};
use support::{
	debug, decl_event, decl_module, decl_storage, dispatch::Result, ensure, traits::Time, Parameter,
};
use system::{
	ensure_signed,
	offchain::{CreateTransaction, SubmitSignedTransaction},
};

pub const KEY_TYPE: KeyTypeId = KeyTypeId(*b"mpc_");
pub mod crypto {
	use super::KEY_TYPE;
	use sp_runtime::app_crypto::{app_crypto, sr25519};
	app_crypto!(sr25519, KEY_TYPE);
}

pub trait Trait: system::Trait {
	type Event: From<Event<Self>> + Into<<Self as system::Trait>::Event>;

	type Call: From<Call<Self>>;

	type SubmitTransaction: SubmitSignedTransaction<Self, <Self as Trait>::Call>;
}

decl_storage! {
	trait Store for Module<T: Trait> as Mpc {
		Authorities get(authorities): Vec<T::AccountId>;

		Results get(fn result_of): map u64 => Vec<u8>;

		Requests get(fn request_of): map u64 => Vec<u8>;

		ReqIds: Vec<u64>;
	}
}

decl_module! {
	pub struct Module<T: Trait> for enum Call where origin: T::Origin {
		fn deposit_event() = default;

		fn request_sig(origin, id: u64, data: Vec<u8>) -> Result {
			ensure_signed(origin)?;
			// ensure!(!<Requests>::exists(id), "req id exists");
			// ensure!(!<Results>::exists(id), "req id exists");
			Self::_request_sig(id, data);
			Ok(())
		}


		pub fn save_sig(origin, id: u64, data: Vec<u8>) -> Result {
			ensure_signed(origin)?;
			// ensure!(!<Requests>::exists(id), "req id exists");
			// ensure!(!<Results>::exists(id), "req id exists");
			<Results>::insert(id, data);
			debug::warn!("save sig ok");
			Ok(())
		}

		fn offchain_worker(_now: T::BlockNumber) {
			debug::RuntimeLogger::init();
			if let Some(value) = local_storage_get(StorageKind::PERSISTENT, &[1u8]) {
				// LOCAL?
				Self::submit_result(1, value);
				debug::warn!("insert ok");
			} else {
				debug::warn!("nothing");
			}
		}

		pub fn add_authority(origin, who: T::AccountId) -> Result {
			let _me = ensure_signed(origin)?; // ensure root

			if !Self::is_authority(&who){
				<Authorities<T>>::mutate(|l| l.push(who));
			}

			Ok(())
		}
	}
}

decl_event!(
	pub enum Event<T>
	where
		AccountId = <T as system::Trait>::AccountId,
	{
		Test(u32, AccountId),
	}
);

impl<T: Trait> Module<T> {
	fn submit_result(id: u64, data: Vec<u8>) {
		let call = Call::save_sig(id, data);
		let res = T::SubmitTransaction::submit_signed(call);

		if res.is_empty() {
			debug::error!("No local accounts found.");
		} else {
			debug::info!("Sent transactions from: {:?}", res);
		}
	}

	fn is_authority(who: &T::AccountId) -> bool {
		Self::authorities().into_iter().find(|i| i == who).is_some()
	}

	fn _request_sig(id: u64, data: Vec<u8>) {
		Self::_deposit_log(ConsensusLog::RequestForSig(id, data));
	}

	fn _deposit_log(log: ConsensusLog) {
		let log: DigestItem<T::Hash> = DigestItem::Consensus(MPC_ENGINE_ID, log.encode());
		<system::Module<T>>::deposit_log(log.into());
	}
}
