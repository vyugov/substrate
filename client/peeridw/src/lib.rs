use std::cmp::{Ord, Ordering, PartialOrd};
use std::fmt;

use derive_more::{From, Into};
use codec::{Decode, Encode, Error as CodecError, Input};

#[cfg(feature = "std")]
use serde::de::{Deserializer,Error as SerdeError,SeqAccess,Visitor};
#[cfg(feature = "std")]
use serde::{ser::SerializeSeq,Serializer};

#[cfg(feature = "std")]
use serde::{Deserialize, Serialize};

use network::PeerId;

#[derive(Clone, Debug, Hash, PartialEq, Eq, From, Into)]
#[repr(transparent)]
pub struct PeerIdW(pub PeerId);

pub trait PeerIdTrait {}


pub trait IsSamePeer<PId:PeerIdTrait>
{
  fn is_same_peer(&self,peer:&PId) ->bool;
}

impl PeerIdTrait for PeerIdW {}
impl PeerIdTrait for PeerId {}
impl IsSamePeer<PeerId> for PeerIdW
{
  fn is_same_peer(&self,peer:&PeerId) ->bool
  {
    self.0==*peer
  }
}


impl IsSamePeer<PeerIdW> for PeerId
{
  fn is_same_peer(&self,peer:&PeerIdW) ->bool
  {
    *self==peer.0
  }
}


impl PartialOrd for PeerIdW
{
  fn partial_cmp(&self, other: &Self) -> Option<Ordering>
  {
    Some(self.cmp(other))
  }
}
use rand::Rng;
impl rand::distributions::Distribution<PeerIdW> for rand::distributions::Standard
{
  fn sample<R: Rng + ?Sized>(&self, _rng: &mut R) -> PeerIdW
  {
    PeerIdW(PeerId::random())
  }
}

impl rand::distributions::Distribution<PeerIdW> for PeerIdW
{
  fn sample<R: Rng + ?Sized>(&self, _rng: &mut R) -> PeerIdW
  {
    PeerIdW(PeerId::random())
  }
}

/*
impl From<&PeerId> for &PeerIdW
{
  fn from(id: &PeerId) -> Self {
    &PeerIdW{0:*id}
}
}*/
impl AsRef<PeerId> for PeerIdW
{
  fn as_ref(&self) -> &PeerId
  {
    &self.0
  }
}

impl Ord for PeerIdW
{
  fn cmp(&self, other: &Self) -> Ordering
  {
    self.0.to_base58().cmp(&other.0.to_base58())
  }
}

impl Encode for PeerIdW
{
  fn encode(&self) -> Vec<u8>
  {
    let bytes = self.0.as_bytes();
    Encode::encode(bytes)
  }
}

impl Decode for PeerIdW
{
  fn decode<I: Input>(value: &mut I) -> Result<Self, CodecError>
  {
    let decoded: Vec<u8> = Decode::decode(value)?;
    let pid = PeerId::from_bytes(decoded).unwrap();
    Ok(Self(pid))
  }
}

#[cfg(feature = "std")]
impl Serialize for PeerIdW
{
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    let e = self.0.as_bytes();

    let mut seq = serializer.serialize_seq(Some(e.len()))?;
    for s in e
    {
      seq.serialize_element(s)?;
    }
    seq.end()
  }
}

struct PeerIdVisitor;

#[cfg(feature = "std")]
impl<'de> Visitor<'de> for PeerIdVisitor
{
  type Value = PeerIdW;

  fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result
  {
    formatter.write_str("sequence of bytes in PeerId shape")
  }

  fn visit_seq<M>(self, mut access: M) -> Result<Self::Value, M::Error>
  where
    M: SeqAccess<'de>,
  {
    let mut buf = Vec::<u8>::with_capacity(access.size_hint().unwrap_or(0));

    while let Some(value) = access.next_element()?
    {
      buf.push(value)
    }

    PeerId::from_bytes(buf)
      .map(|pid| PeerIdW(pid))
      .map_err(|_| M::Error::custom("PeerId from_bytes failed"))
  }
}

#[cfg(feature = "std")]
impl<'de> Deserialize<'de> for PeerIdW
{
  fn deserialize<D>(deserializer: D) -> Result<PeerIdW, D::Error>
  where
    D: Deserializer<'de>,
  {
    deserializer.deserialize_seq(PeerIdVisitor)
  }
}
