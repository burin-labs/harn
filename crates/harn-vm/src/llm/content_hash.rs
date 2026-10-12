//! Stable content digests for retained LLM context.
//!
//! Hashing happens after the active redaction policy so transcript payloads,
//! assembly manifests, and served-context receipts all name the same retained
//! bytes. Keep these helpers as the only owner of that surface.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub(crate) fn stable_redacted_string_hash(value: &str) -> String {
    let policy = crate::redact::current_policy();
    let redacted = match policy.redact_json(&serde_json::Value::String(value.to_string())) {
        serde_json::Value::String(redacted) => redacted,
        other => crate::canonical_json::to_string(&other),
    };
    stable_content_hash(redacted.as_bytes())
}

pub(crate) fn stable_redacted_json_hash(value: &serde_json::Value) -> String {
    let redacted = crate::redact::current_policy().redact_json(value);
    let encoded = crate::canonical_json::to_vec(&redacted);
    stable_content_hash(&encoded)
}

fn stable_content_hash(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

/// A value's canonical JSON after redaction, and its content hash.
pub(crate) struct RedactedContent {
    canonical: Box<[u8]>,
    hash: String,
}

impl RedactedContent {
    fn compute(value: &serde_json::Value, config: &crate::redact::RedactionConfig) -> Self {
        let canonical = crate::canonical_json::to_vec(&config.policy().redact_json(value));
        let hash = stable_content_hash(&canonical);
        Self {
            canonical: canonical.into_boxed_slice(),
            hash,
        }
    }

    /// Equal to [`stable_redacted_json_hash`] of the value.
    pub(crate) fn hash(&self) -> &str {
        &self.hash
    }
}

/// Redacted contents of `values`, in order.
///
/// Every LLM call re-sends the whole transcript, so redacting and hashing it
/// from scratch makes each turn cost more than the last. Results are memoized
/// by raw content. A value whose redaction recorded an audit event is
/// recomputed on every call, so audit emission is unchanged.
pub(crate) fn redacted_contents(values: &[serde_json::Value]) -> Vec<Rc<RedactedContent>> {
    let config = crate::redact::RedactionConfig::current();
    REDACTED_CONTENT_MEMO.with(|memo| {
        let mut memo = memo.borrow_mut();
        if memo.config.as_ref() != Some(&config) {
            memo.entries.clear();
            memo.config = Some(config.clone());
        }
        values
            .iter()
            .map(|value| {
                let key = content_fingerprint(value);
                if let Some(content) = memo.entries.get(&key) {
                    return content;
                }
                let audits_before = crate::redact::audit_events_emitted();
                let content = Rc::new(RedactedContent::compute(value, &config));
                if crate::redact::audit_events_emitted() == audits_before {
                    let weight = content.canonical.len();
                    memo.entries.insert(key, Rc::clone(&content), weight);
                }
                content
            })
            .collect()
    })
}

/// Equal to [`stable_redacted_json_hash`] of the array the contents were
/// computed from. Redaction maps array elements independently and canonical
/// JSON joins element encodings with commas, so the array's redacted bytes
/// are the element bytes in brackets.
pub(crate) fn redacted_sequence_hash(contents: &[Rc<RedactedContent>]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"[");
    for (index, content) in contents.iter().enumerate() {
        if index > 0 {
            hasher.update(b",");
        }
        hasher.update(&content.canonical);
    }
    hasher.update(b"]");
    format!("blake3:{}", hasher.finalize().to_hex())
}

struct RedactedContentMemo {
    config: Option<crate::redact::RedactionConfig>,
    entries: ContentMemo<Rc<RedactedContent>>,
}

thread_local! {
    static REDACTED_CONTENT_MEMO: RefCell<RedactedContentMemo> = RefCell::new(RedactedContentMemo {
        config: None,
        entries: ContentMemo::new(REDACTED_CONTENT_MEMO_BYTES),
    });
    static FINGERPRINT_BUFFER: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

const REDACTED_CONTENT_MEMO_BYTES: usize = 32 * 1024 * 1024;

/// Collision-resistant identity of a JSON value's content. Every node is
/// framed with a type tag and length, so distinct values never feed the same
/// byte stream. Object members are fed in map order; a different order only
/// costs a memo miss.
pub(crate) type ContentFingerprint = [u8; 32];

pub(crate) fn content_fingerprint(value: &serde_json::Value) -> ContentFingerprint {
    fn frame(buffer: &mut Vec<u8>, tag: u8, bytes: &[u8]) {
        buffer.push(tag);
        buffer.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        buffer.extend_from_slice(bytes);
    }
    fn feed(buffer: &mut Vec<u8>, value: &serde_json::Value) {
        match value {
            serde_json::Value::Null => buffer.push(b'n'),
            serde_json::Value::Bool(value) => buffer.push(if *value { b't' } else { b'f' }),
            serde_json::Value::Number(number) => frame(buffer, b'#', number.to_string().as_bytes()),
            serde_json::Value::String(text) => frame(buffer, b's', text.as_bytes()),
            serde_json::Value::Array(items) => {
                buffer.push(b'[');
                buffer.extend_from_slice(&(items.len() as u64).to_le_bytes());
                for item in items {
                    feed(buffer, item);
                }
            }
            serde_json::Value::Object(members) => {
                buffer.push(b'{');
                buffer.extend_from_slice(&(members.len() as u64).to_le_bytes());
                for (key, member) in members {
                    frame(buffer, b'k', key.as_bytes());
                    feed(buffer, member);
                }
            }
        }
    }
    // One hash over a framed buffer is several times faster than streaming
    // many small updates into the hasher.
    FINGERPRINT_BUFFER.with(|buffer| {
        let mut buffer = buffer.borrow_mut();
        buffer.clear();
        feed(&mut buffer, value);
        let fingerprint = *blake3::hash(&buffer).as_bytes();
        if buffer.capacity() > FINGERPRINT_BUFFER_RETAINED_BYTES {
            *buffer = Vec::new();
        }
        fingerprint
    })
}

const FINGERPRINT_BUFFER_RETAINED_BYTES: usize = 1024 * 1024;

/// Two-generation memo keyed by content fingerprint. When the newer
/// generation exceeds its weight budget it replaces the older one, so entries
/// used in the last generation survive and the memo stays bounded.
pub(crate) struct ContentMemo<V> {
    current: HashMap<ContentFingerprint, (V, usize)>,
    previous: HashMap<ContentFingerprint, (V, usize)>,
    current_weight: usize,
    budget: usize,
}

impl<V: Clone> ContentMemo<V> {
    pub(crate) fn new(budget: usize) -> Self {
        Self {
            current: HashMap::new(),
            previous: HashMap::new(),
            current_weight: 0,
            budget,
        }
    }

    pub(crate) fn get(&mut self, key: &ContentFingerprint) -> Option<V> {
        if let Some((value, _)) = self.current.get(key) {
            return Some(value.clone());
        }
        let (value, weight) = self.previous.remove(key)?;
        self.insert(*key, value.clone(), weight);
        Some(value)
    }

    pub(crate) fn insert(&mut self, key: ContentFingerprint, value: V, weight: usize) {
        if self.current_weight.saturating_add(weight) > self.budget && !self.current.is_empty() {
            self.previous = std::mem::take(&mut self.current);
            self.current_weight = 0;
        }
        self.current_weight = self.current_weight.saturating_add(weight);
        self.current.insert(key, (value, weight));
    }

    pub(crate) fn clear(&mut self) {
        self.current.clear();
        self.previous.clear();
        self.current_weight = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacted_json_hash_ignores_object_key_order() {
        let first: serde_json::Value = serde_json::from_str(r#"{"zeta":1,"alpha":2}"#).unwrap();
        let second: serde_json::Value = serde_json::from_str(r#"{"alpha":2,"zeta":1}"#).unwrap();

        assert_eq!(
            stable_redacted_json_hash(&first),
            stable_redacted_json_hash(&second)
        );
    }

    fn transcript() -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({"role": "user", "content": "read the file"}),
            serde_json::json!({"role": "assistant", "content": "", "tool_calls": [{"id": "c1", "name": "read"}]}),
            serde_json::json!({"role": "tool_result", "content": "token AKIAABCDEFGHIJKLMNOP leaked", "api_key": "plain"}),
        ]
    }

    #[test]
    fn memoized_contents_hash_exactly_like_the_uncached_path() {
        let values = transcript();
        let contents = redacted_contents(&values);
        for (value, content) in values.iter().zip(&contents) {
            assert_eq!(content.hash(), stable_redacted_json_hash(value));
        }
        let again = redacted_contents(&values);
        assert_eq!(
            redacted_sequence_hash(&again),
            stable_redacted_json_hash(&serde_json::Value::Array(values)),
            "the streamed sequence hash must equal the whole-array hash"
        );
    }

    #[test]
    fn only_audit_free_contents_are_reused() {
        let values = transcript();
        let first = redacted_contents(&values);
        let audits = crate::redact::audit_events_emitted();
        let second = redacted_contents(&values);

        assert!(
            Rc::ptr_eq(&first[0], &second[0]),
            "secret-free content is memoized"
        );
        assert!(
            !Rc::ptr_eq(&first[2], &second[2]),
            "content whose redaction was audited is recomputed"
        );
        assert!(
            crate::redact::audit_events_emitted() > audits,
            "recomputing secret-bearing content audits it again"
        );
    }

    #[test]
    fn a_policy_change_invalidates_memoized_contents() {
        let values = transcript();
        let redacted = redacted_contents(&values);
        let _guard = crate::redact::PolicyGuard::new(crate::redact::RedactionPolicy::passthrough());
        let raw = redacted_contents(&values);

        assert_ne!(redacted[2].hash(), raw[2].hash());
        assert_eq!(raw[2].hash(), stable_redacted_json_hash(&values[2]));
    }

    #[test]
    fn content_memo_stays_within_two_generations() {
        let mut memo = ContentMemo::new(2);
        for index in 0..10u8 {
            memo.insert([index; 32], index, 1);
        }
        assert!(memo.current.len() + memo.previous.len() <= 4);
        assert_eq!(memo.get(&[9; 32]), Some(9));
        assert_eq!(memo.get(&[0; 32]), None);
    }
}
