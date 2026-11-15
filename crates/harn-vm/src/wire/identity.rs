use serde_json::Value;
use sha2::{Digest, Sha256};

/// All wire, catalog and codegen identities use the VM's canonical JSON bytes.
pub(crate) fn digest(value: &Value) -> String {
    format!(
        "sha256:{}",
        hex::encode(Sha256::digest(crate::canonical_json::to_vec(value)))
    )
}
