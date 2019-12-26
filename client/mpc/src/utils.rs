
use std::vec::Vec;

use crate::RequestId;

fn get_storage_key(id: RequestId) -> Vec<u8>{
    let mut k = Vec::new();
    k.extend(&id.to_le_bytes());
    k
}
