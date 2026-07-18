use sha2::{Digest, Sha256};

pub(crate) fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

pub fn hash_content_bytes(bytes: &[u8]) -> String {
    hash_bytes(bytes)
}
