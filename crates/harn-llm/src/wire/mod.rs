//! Provider request encoding with resolved policy and deferred audit delivery.

pub mod openai;
pub mod openai_normalize;
pub mod prompt_cache;
pub mod thinking;

/// Encoded wire body and schema changes for the host's audit sink.
pub struct EncodedRequest {
    pub body: serde_json::Value,
    pub schema_audits: Vec<std::collections::BTreeMap<String, serde_json::Value>>,
}
