use bincode;
use codec::{Decode, Encode, Error as CodecError, Input};
use rand::rngs::{OsRng, StdRng};
use rand::{RngCore, SeedableRng};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::peer::Index as PeerIndex;

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BroadCastMessage {
	index: PeerIndex,
	msg: u32,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DecommitMessage {
	index: PeerIndex,
	msg: u32,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum KeyGenMessage {
	BroadCast(BroadCastMessage),
	Decommit(DecommitMessage),
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

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum SignMessage {
	Commit,
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

impl Decode for SignMessage{
 	fn decode<I: Input>(value: &mut I) -> Result<Self, CodecError> {
		let decoded: Vec<u8> = Decode::decode(value)?;
		let bytes = decoded.as_slice();
		Ok(bincode::deserialize(bytes).unwrap())
	}
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, Encode, Decode)]
pub enum Message {
	KeyGen(KeyGenMessage),
	Sign(SignMessage),
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, Encode, Decode)]
pub struct SignedMessage {
	pub message: Message,
	pub sig: u64,
	pub index: u16,
}

#[cfg(test)]
mod tests {
	use super::*;

	use std::collections::BTreeMap;
	#[test]
	fn test_message_encode_decode() {
		let kgm = KeyGenMessage::BroadCast {
			index: 0u32,
			msg: 1u32
		};
		let encoded: Vec<u8> = kgm.encode();
		let decoded = KeyGenMessage::decode(&mut encoded.as_slice()).unwrap();
		assert_eq!(kgm, decoded);
	}
}
