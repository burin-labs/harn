pub(in crate::llm) fn stable_hash(val: &serde_json::Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let canonical = serde_json::to_string(val).unwrap_or_default();
    canonical.hash(&mut hasher);
    hasher.finish()
}
