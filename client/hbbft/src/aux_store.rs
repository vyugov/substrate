// Copyright 2019 Parity Technologies (UK) Ltd.
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

//! Schema for stuff in the aux-db.
use std::collections::{BTreeMap,};
use parity_codec::{Decode, Encode};
use parking_lot::RwLock;
use runtime_primitives::Justification;
use sc_api::{AuxStore, Backend};
use sp_blockchain::{Error as ClientError, Result as ClientResult};
use std::fmt::Debug;
use std::sync::Arc;
//use fork_tree::ForkTree;
use badger_primitives::AuthorityId;
use badger_primitives::AuthorityPair;
use badger_primitives::{AuthorityList, EraId};
use keystore::KeyStorePtr;
use log::info;
use runtime_primitives::traits::Block as BlockT;
use sc_peerid_wrapper::PeerIdW;
//use crate::authorities::{AuthoritySet, SharedAuthoritySet, PendingChange, DelayKind};

use substrate_primitives::crypto::Pair;

const VERSION_KEY: &[u8] = b"hbbft_schema_version";
//const SET_STATE_KEY: &[u8] = b"grandpa_completed_round";
const AUTHORITY_SET_KEY: &[u8] = b"hbbft_authorities";

const VOTE_KEY: &[u8] = b"hbbft_current_vote"; //to save current vote for change?  change is a list of public keys, same as auth set

const PEER_MAP_KEY: &[u8] = b"hbbft_peer_to_auth_map";

const CURRENT_VERSION: u32 = 0;

#[derive(Debug, Clone, Encode, Decode)]
pub struct AuthoritySet
{
  pub current_authorities: AuthorityList,
  pub self_id: AuthorityId,
  pub era: EraId,
}



pub(crate) fn load_decode<B: AuxStore, T: Decode>(backend: &B, key: &[u8]) -> ClientResult<Option<T>>
{
  match backend.get_aux(key)?
  {
    None => Ok(None),
    Some(t) => T::decode(&mut &t[..])
      .map_err(|e| ClientError::Backend(format!("SNAKE DB is corrupted: {}", e.what())))
      .map(Some),
  }
}
pub struct BadgerSharedAuthoritySet
{
  pub inner: Arc<RwLock<AuthoritySet>>,
}
impl BadgerSharedAuthoritySet
{
  pub fn verify_full_justification<B: BlockT>(&self, just_dat: Justification) -> bool
  {
    use crate::communication::gossip::BadgerFullJustification;
    let just: BadgerFullJustification<B> = match Decode::decode(&mut &just_dat[..])
    {
      Ok(dat) => dat,
      Err(_) => return false,
    };
    if !just.verify()
    {
      return false;
    }

    let mut count_total = 0;
    let mut count_accepted = 0;
    for authority in self.inner.read().current_authorities.iter()
    {
      count_total += 1;
      if just.commits.iter().find(|x| x.validator == *authority).is_some()
      {
        count_accepted += 1;
      }
    }
    let tolerated = count_total - badger::util::max_faulty(count_total);

    count_accepted >= tolerated
  }
}

impl Clone for BadgerSharedAuthoritySet
{
  fn clone(&self) -> Self
  {
    BadgerSharedAuthoritySet {
      inner: self.inner.clone(),
    }
  }
}
impl From<AuthoritySet> for BadgerSharedAuthoritySet
{
  fn from(set: AuthoritySet) -> Self
  {
    BadgerSharedAuthoritySet {
      inner: Arc::new(RwLock::new(set)),
    }
  }
}
/// Persistent data kept between runs.
pub struct BadgerPersistentData
{
  pub authority_set: BadgerSharedAuthoritySet,
  pub change_vote: Option<BadgerSharedAuthoritySet>,
}

pub struct BadgerAuthorityMap<I:AuxStore>
{
 pub auths:BTreeMap<PeerIdW,AuthorityId>,
 backend: Arc<I>
}

impl<I:AuxStore>  BadgerAuthorityMap<I>
{
  pub fn load_or_create(backend:Arc<I>) ->RwLock<Self>
  {
    let map= match load_decode(&*backend, PEER_MAP_KEY)
    {
    Ok(res) => match res
    {
      Some(mres) => mres,
      None => BTreeMap::new()
    },
    Err(_) =>
    {
      BTreeMap::new()

    }

    };
    RwLock::new(BadgerAuthorityMap
    {
      auths: map,
      backend:backend.clone()
    })
  }
  pub fn save(&mut self)
  {
    match self.backend.insert_aux(&[(PEER_MAP_KEY, self.auths.encode().as_slice())], &[])
    {
     Ok(_) => {},
     Err(e) =>info!("Error saving auth map into database: {:?}",e),
    };
  }
  pub fn insert(&mut self,peer:&PeerIdW,auth:&AuthorityId)
  {
    if let Some( current)= self.auths.get_mut(peer)
    {
      if current==auth //no change
      {
        return;
      }
      (*current)=auth.clone();
    }
    else
    {
       self.auths.insert(peer.clone(),auth.clone());
    }
    self.save();
  }
  pub fn get_auth(&self,peer:&PeerIdW)->Option<AuthorityId>
  {
   match self.auths.get(peer)
   {
     Some(k) =>Some(k.clone()),
     None =>None
   }
  }
  pub fn get_peer(&self,auth:&AuthorityId)->Option<PeerIdW>
  {
   match self.auths.iter().find ( | (_,v) | *v==auth )
   {
     Some(k) =>Some(k.0.clone()),
     None =>None
   }
  }
  pub fn get_peer_list(&self,list:&AuthorityList)-> Vec<PeerIdW>
  {
    list.iter().filter_map( |auth| self.auths.iter().find ( | (_,v) | *v==auth ).map( |(k,_)| k.clone()) ).collect()
  }
}

/// Added so that I could access authority set without initializing whole badger
pub fn loads_auth_set<B, G>(backend: &B, genesis_authorities: G) -> ClientResult<BadgerSharedAuthoritySet>
where
  B: AuxStore,
  G: FnOnce() -> ClientResult<AuthorityList>,
{
  let version: Option<u32> = load_decode(backend, VERSION_KEY)?;
  match version
  {
    None | Some(CURRENT_VERSION) =>
    {
      if let Some(set) = load_decode::<_, AuthoritySet>(backend, AUTHORITY_SET_KEY)?
      {
        return Ok(set.into());
      }
    }

    Some(other) => return Err(ClientError::Backend(format!("Unsupported BADGER DB version: {:?}", other)).into()),
  };
  info!(target: "afg", "Loading Badger authority set, no init.");

  let genesis_authorities = genesis_authorities()?;
  let genesis_set = AuthoritySet {
    current_authorities: genesis_authorities,
    era: 0,
    self_id: Default::default(),
  };
  return Ok(genesis_set.into());
}

/// Load or initialize persistent data from backend.
pub fn load_persistent_badger<B, G>(
  backend: &B, genesis_authorities: G, keystore: KeyStorePtr,
) -> ClientResult<BadgerPersistentData>
where
  B: AuxStore,
  G: FnOnce() -> ClientResult<AuthorityList>,
{
  let version: Option<u32> = load_decode(backend, VERSION_KEY)?;

  let change_vote: Option<BadgerSharedAuthoritySet> = match load_decode::<_, AuthoritySet>(backend, VOTE_KEY)?
  {
    Some(aset) => Some(aset.into()),
    None => None,
  };

  match version
  {
    None | Some(CURRENT_VERSION) =>
    {
      if let Some(set) = load_decode::<_, AuthoritySet>(backend, AUTHORITY_SET_KEY)?
      {
        return Ok(BadgerPersistentData {
          authority_set: set.into(),
          change_vote: change_vote.into(),
        });
      }
    }

    Some(other) => return Err(ClientError::Backend(format!("Unsupported BADGER DB version: {:?}", other)).into()),
  }

  // genesis.
  info!(target: "afg", "Loading Badger authority set \
		from genesis on what appears to be first startup.");

  let genesis_authorities = genesis_authorities()?;
  let s_id = genesis_authorities
    .iter()
    .find(|x| {
      keystore
        .read()
        .key_pair_by_type::<AuthorityPair>(*x, app_crypto::key_types::HB_NODE)
        .is_ok()
    })
    .cloned()
    .unwrap_or_else(|| keystore.write().generate::<AuthorityPair>().unwrap().public());
  let genesis_set = AuthoritySet {
    current_authorities: genesis_authorities,
    era: 0,
    self_id: s_id,
  };

  backend.insert_aux(&[(AUTHORITY_SET_KEY, genesis_set.encode().as_slice())], &[])?;

  Ok(BadgerPersistentData {
    authority_set: genesis_set.into(),
    change_vote: None,
  })
}

/// Update the authority set on disk after a change.
pub fn update_authority_set<F, R>(set: &AuthoritySet, write_aux: F) -> R
where
  F: FnOnce(&[(&'static [u8], &[u8])]) -> R,
{
  // write new authority set state to disk.
  let encoded_set = set.encode();
  write_aux(&[(AUTHORITY_SET_KEY, &encoded_set[..])])
}

/// Update the authority set on disk after a change.
pub fn update_vote<F, R, D>(vote_set: &Option<AuthoritySet>, write_aux: F, delete_aux: D) -> R
where
  F: FnOnce(&[(&'static [u8], &[u8])]) -> R,
  D: FnOnce(&[&'static [u8]]) -> R,
{
  // write new vote set state to disk.
  if let Some(vote) = vote_set
  {
    let encoded_set = vote.encode();
    write_aux(&[(VOTE_KEY, &encoded_set[..])])
  }
  else
  {
    delete_aux(&[VOTE_KEY])
  }
}

#[cfg(test)]
pub(crate) fn load_authorities<B: AuxStore, H: Decode, N: Decode>(backend: &B) -> Option<AuthoritySet<H, N>>
{
  load_decode::<_, AuthoritySet<H, N>>(backend, AUTHORITY_SET_KEY).expect("backend error")
}

#[cfg(test)]
mod test
{
  use super::*;
  use fg_primitives::AuthorityId;
  use primitives::H256;
  use test_client;

  #[test]
  fn load_decode_from_v0_migrates_data_format()
  {
    let client = test_client::new();

    let authorities = vec![(AuthorityId::default(), 100)];
    let set_id = 3;
    let round_number: RoundNumber = 42;
    let round_state = RoundState::<H256, u64> {
      prevote_ghost: Some((H256::random(), 32)),
      finalized: None,
      estimate: None,
      completable: false,
    };

    {
      let authority_set = V0AuthoritySet::<H256, u64> {
        current_authorities: authorities.clone(),
        pending_changes: Vec::new(),
        set_id,
      };

      let voter_set_state = (round_number, round_state.clone());

      client
        .insert_aux(
          &[
            (AUTHORITY_SET_KEY, authority_set.encode().as_slice()),
            (SET_STATE_KEY, voter_set_state.encode().as_slice()),
          ],
          &[],
        )
        .unwrap();
    }

    assert_eq!(load_decode::<_, u32>(&client, VERSION_KEY).unwrap(), None,);

    // should perform the migration
    load_persistent::<test_client::runtime::Block, _, _>(&client, H256::random(), 0, || unreachable!()).unwrap();

    assert_eq!(load_decode::<_, u32>(&client, VERSION_KEY).unwrap(), Some(2),);

    let PersistentData {
      authority_set,
      set_state,
      ..
    } = load_persistent::<test_client::runtime::Block, _, _>(&client, H256::random(), 0, || unreachable!()).unwrap();

    assert_eq!(
      *authority_set.inner().read(),
      AuthoritySet {
        current_authorities: authorities.clone(),
        pending_standard_changes: ForkTree::new(),
        pending_forced_changes: Vec::new(),
        set_id,
      },
    );

    let mut current_rounds = CurrentRounds::new();
    current_rounds.insert(round_number + 1, HasVoted::No);

    assert_eq!(
      &*set_state.read(),
      &VoterSetState::Live {
        completed_rounds: CompletedRounds::new(
          CompletedRound {
            number: round_number,
            state: round_state.clone(),
            base: round_state.prevote_ghost.unwrap(),
            votes: vec![],
          },
          set_id,
          &*authority_set.inner().read(),
        ),
        current_rounds,
      },
    );
  }

  #[test]
  fn load_decode_from_v1_migrates_data_format()
  {
    let client = test_client::new();

    let authorities = vec![(AuthorityId::default(), 100)];
    let set_id = 3;
    let round_number: RoundNumber = 42;
    let round_state = RoundState::<H256, u64> {
      prevote_ghost: Some((H256::random(), 32)),
      finalized: None,
      estimate: None,
      completable: false,
    };

    {
      let authority_set = AuthoritySet::<H256, u64> {
        current_authorities: authorities.clone(),
        pending_standard_changes: ForkTree::new(),
        pending_forced_changes: Vec::new(),
        set_id,
      };

      let voter_set_state = V1VoterSetState::Live(round_number, round_state.clone());

      client
        .insert_aux(
          &[
            (AUTHORITY_SET_KEY, authority_set.encode().as_slice()),
            (SET_STATE_KEY, voter_set_state.encode().as_slice()),
            (VERSION_KEY, 1u32.encode().as_slice()),
          ],
          &[],
        )
        .unwrap();
    }

    assert_eq!(load_decode::<_, u32>(&client, VERSION_KEY).unwrap(), Some(1),);

    // should perform the migration
    load_persistent::<test_client::runtime::Block, _, _>(&client, H256::random(), 0, || unreachable!()).unwrap();

    assert_eq!(load_decode::<_, u32>(&client, VERSION_KEY).unwrap(), Some(2),);

    let PersistentData {
      authority_set,
      set_state,
      ..
    } = load_persistent::<test_client::runtime::Block, _, _>(&client, H256::random(), 0, || unreachable!()).unwrap();

    assert_eq!(
      *authority_set.inner().read(),
      AuthoritySet {
        current_authorities: authorities.clone(),
        pending_standard_changes: ForkTree::new(),
        pending_forced_changes: Vec::new(),
        set_id,
      },
    );

    let mut current_rounds = CurrentRounds::new();
    current_rounds.insert(round_number + 1, HasVoted::No);

    assert_eq!(
      &*set_state.read(),
      &VoterSetState::Live {
        completed_rounds: CompletedRounds::new(
          CompletedRound {
            number: round_number,
            state: round_state.clone(),
            base: round_state.prevote_ghost.unwrap(),
            votes: vec![],
          },
          set_id,
          &*authority_set.inner().read(),
        ),
        current_rounds,
      },
    );
  }
}

/// Provider for the Grandpa authority set configured on the genesis block.
pub trait GenesisAuthoritySetProvider<Block: BlockT>
{
  /// Get the authority set at the genesis block.
  fn get(&self) -> Result<AuthorityList, ClientError>;
}
use client::CallExecutor;
use client::Client;
use runtime_primitives::generic::BlockId;
use substrate_primitives::Blake2Hasher;
use substrate_primitives::H256; //Pair
                                //use substrate_primitives::
use runtime_primitives::traits::Zero;
use state_machine::ExecutionStrategy;

impl<B, E, Block: BlockT<Hash = H256>, RA> GenesisAuthoritySetProvider<Block> for Client<B, E, Block, RA>
where
  B: Backend<Block, Blake2Hasher> + Send + Sync + 'static,
  E: CallExecutor<Block, Blake2Hasher> + 'static + Clone + Send + Sync,
  RA: Send + Sync,
{
  fn get(&self) -> Result<AuthorityList, ClientError>
  {
    // This implementation uses the Badger runtime API instead of reading directly from the
    // `HBBFT_AUTHORITIES_KEY` as the data may have been migrated since the genesis block of... well, not really
    // the chain, whereas the runtime API is backwards compatible.
    self
      .executor()
      .call(
        &BlockId::Number(Zero::zero()),
        "BadgerApi_badger_authorities",
        &[],
        ExecutionStrategy::NativeElseWasm,
        None,
      )
      .and_then(|call_result| {
        Decode::decode(&mut &call_result[..])
          .map_err(|err| ClientError::CallResultDecode("failed to decode HBBFT authorities set proof".into(), err))
      })
  }
}
