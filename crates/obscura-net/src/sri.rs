use sha2::{Sha256, Digest};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

pub fn verify_integrity(data: &[u8], integrity: &str) -> bool {
    let parts: Vec<&str> = integrity.splitn(2, '-').collect();
    if parts.len() != 2 { return false; }
    let (algo, expected_b64) = (parts[0], parts[1]);
    match algo {
        "sha256" => {
            let mut hasher = Sha256::new();
            hasher.update(data);
            let hash = BASE64.encode(hasher.finalize());
            hash == expected_b64
        }
        _ => false, // sha384, sha512 not supported yet
    }
}