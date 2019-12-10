#![cfg_attr(not(feature = "std"), no_std)]

use codec::{Codec, Decode, Encode};
use rstd::vec::Vec;
use sp_runtime::{ConsensusEngineId, RuntimeDebug};

pub const MPC_ENGINE_ID: ConsensusEngineId = *b"MPCE";

#[derive(Decode, Encode, RuntimeDebug)]
pub enum ConsensusLog {
	#[codec(index = "1")]
	RequestForKeygen(u64, Vec<u8>),
}

// sp_api::decl_runtime_apis! {
// 	pub trait MpcApi {
// 		fn test();
// 	}
// }
