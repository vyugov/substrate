
use std::vec::Vec;

use sp_offchain::STORAGE_PREFIX;

fn get_storage_key(id: u64) -> Vec<u8>{
    let mut k = Vec::new();
    k.extend(&id.to_le_bytes());
    k
}
