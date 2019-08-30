use bincode;
use codec::{Decode, Encode, Error as CodecError, Input};

use rand::rngs::{OsRng, StdRng};
use rand::{RngCore, SeedableRng};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2018::party_i::{
	KeyGenBroadcastMessage1 as KeyGenCommit, KeyGenDecommitMessage1 as KeyGenDecommit,
};

use network::PeerId;

type PeerIndex = u16;

pub type MessageWithSender = (Message, Option<PeerId>);
pub type MessageWithReceiver = (Message, Option<PeerId>);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum KeyGenMessage {
	Commit(KeyGenCommit),
	Decommit(KeyGenDecommit),
	VSS,
	SecretShares,
	Proof,
}

impl Encode for KeyGenMessage {
	fn encode(&self) -> Vec<u8> {
		let encoded = bincode::serialize(&self).unwrap();
		let bytes = encoded.as_slice();
		Encode::encode(&bytes)
	}
}

impl Decode for KeyGenMessage {
	fn decode<I: Input>(value: &mut I) -> Result<Self, CodecError> {
		let decoded: Vec<u8> = Decode::decode(value)?;
		let bytes = decoded.as_slice();
		Ok(bincode::deserialize(bytes).unwrap())
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SignMessage {
	BroadCast,
	Decommit,
	Proof,
}

impl Encode for SignMessage {
	fn encode(&self) -> Vec<u8> {
		let encoded = bincode::serialize(&self).unwrap();
		let bytes = encoded.as_slice();
		Encode::encode(&bytes)
	}
}

impl Decode for SignMessage {
	fn decode<I: Input>(value: &mut I) -> Result<Self, CodecError> {
		let decoded: Vec<u8> = Decode::decode(value)?;
		let bytes = decoded.as_slice();
		Ok(bincode::deserialize(bytes).unwrap())
	}
}

#[derive(Clone, Debug, Serialize, Deserialize, Encode, Decode)]
pub enum ConfirmPeersMessage {
	Confirming(u16, u64), // from_index, hash
	Confirmed(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, Encode, Decode)]
pub enum Message {
	ConfirmPeers(ConfirmPeersMessage),
	KeyGen(KeyGenMessage),
	Sign(SignMessage),
}

#[cfg(test)]
mod tests {
	use super::*;

	use std::collections::BTreeMap;
	#[test]
	fn test_message_encode_decode() {
		let kgm = KeyGenMessage::VSS;
		let encoded: Vec<u8> = kgm.encode();
		let decoded = KeyGenMessage::decode(&mut encoded.as_slice()).unwrap();
		assert_eq!(kgm, decoded);
	}
}
