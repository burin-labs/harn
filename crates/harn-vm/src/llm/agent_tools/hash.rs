pub(in crate::llm) fn stable_hash(val: &serde_json::Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let canonical = crate::canonical_json::to_vec(val);
    canonical.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_hash_ignores_object_key_order() {
        let first: serde_json::Value = serde_json::from_str(r#"{"zeta":1,"alpha":2}"#).unwrap();
        let second: serde_json::Value = serde_json::from_str(r#"{"alpha":2,"zeta":1}"#).unwrap();

        assert_eq!(stable_hash(&first), stable_hash(&second));
    }
}
