//! Task-create retry identity shared by native and hosted admission adapters.
//!
//! Authentication supplies the actor. Storage atomically binds this identity
//! to one task before dispatch; transport responses are never cached here.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub use harn_session_store::task::{TaskAdmission, TASK_RETRY_RETENTION_MS};
const MAX_KEY_BYTES: usize = 255;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TaskRetryPolicy {
    #[default]
    Unsupported,
    TaskCreate,
}

impl TaskRetryPolicy {
    pub const fn supported_method(self) -> Option<&'static str> {
        match self {
            Self::Unsupported => None,
            Self::TaskCreate => Some("POST"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskRetryError {
    InvalidKey,
    InvalidActor,
    InvalidTarget,
}

impl std::fmt::Display for TaskRetryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidKey => "idempotency key must contain 1 to 255 printable ASCII bytes",
            Self::InvalidActor => "task retry actor must have a non-empty scheme and subject",
            Self::InvalidTarget => "task retry session and workspace identities must be non-empty",
        })
    }
}

impl std::error::Error for TaskRetryError {}

#[derive(Clone, PartialEq, Eq)]
pub struct TaskRetryKey(String);

impl TaskRetryKey {
    pub fn parse(raw: &str) -> Result<Self, TaskRetryError> {
        // Check before trimming so a control byte at either edge is rejected.
        if raw.bytes().any(|byte| !(b' '..=b'~').contains(&byte)) {
            return Err(TaskRetryError::InvalidKey);
        }
        let normalized = raw.trim();
        if normalized.is_empty() || normalized.len() > MAX_KEY_BYTES {
            return Err(TaskRetryError::InvalidKey);
        }
        Ok(Self(normalized.to_owned()))
    }
}

impl std::fmt::Debug for TaskRetryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TaskRetryKey([private])")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TaskRetryActor {
    scheme: String,
    subject: String,
}

impl<'de> Deserialize<'de> for TaskRetryActor {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            scheme: String,
            subject: String,
        }
        let fields = Fields::deserialize(deserializer)?;
        Self::new(fields.scheme, fields.subject).map_err(serde::de::Error::custom)
    }
}

impl TaskRetryActor {
    /// Only the authentication owner should project a verified principal here.
    pub fn new(
        scheme: impl Into<String>,
        subject: impl Into<String>,
    ) -> Result<Self, TaskRetryError> {
        let actor = Self {
            scheme: scheme.into(),
            subject: subject.into(),
        };
        if actor.scheme.trim().is_empty() || actor.subject.trim().is_empty() {
            return Err(TaskRetryError::InvalidActor);
        }
        Ok(actor)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRetryScope(String);

impl TaskRetryScope {
    pub fn for_task_create(
        actor: TaskRetryActor,
        workspace_id: Option<&str>,
        session_id: &str,
        key: TaskRetryKey,
    ) -> Result<Self, TaskRetryError> {
        if session_id.trim().is_empty() || workspace_id.is_some_and(|id| id.trim().is_empty()) {
            return Err(TaskRetryError::InvalidTarget);
        }
        // A structured target unifies top-level and session-scoped routes and
        // cannot collide through punctuation inside a principal or resource ID.
        let identity = json!({
            "schema": "harn.task_retry.v1",
            "actor": { "scheme": actor.scheme, "subject": actor.subject },
            "workspace_id": workspace_id,
            "method": TaskRetryPolicy::TaskCreate.supported_method().expect("task create method"),
            "target": { "operation": "tasks.create", "session_id": session_id },
            "key": key.0,
        });
        Ok(Self(digest(&identity)))
    }

    pub fn digest(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRequestFingerprint(String);

impl TaskRequestFingerprint {
    /// Hash the accepted request before redaction or host policy injection.
    pub fn from_value(input: &Value) -> Self {
        Self(digest(input))
    }

    pub fn as_hex(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRetryRequest {
    scope: TaskRetryScope,
    fingerprint: TaskRequestFingerprint,
}

impl TaskRetryRequest {
    pub fn new(scope: TaskRetryScope, fingerprint: TaskRequestFingerprint) -> Self {
        Self { scope, fingerprint }
    }

    pub fn scope(&self) -> &TaskRetryScope {
        &self.scope
    }

    pub fn fingerprint(&self) -> &TaskRequestFingerprint {
        &self.fingerprint
    }
}

fn digest(value: &Value) -> String {
    hex::encode(Sha256::digest(crate::canonical_json::to_vec(value)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(
        scheme: &str,
        actor: &str,
        workspace: Option<&str>,
        session: &str,
        key: &str,
    ) -> TaskRetryScope {
        TaskRetryScope::for_task_create(
            TaskRetryActor::new(scheme, actor).unwrap(),
            workspace,
            session,
            TaskRetryKey::parse(key).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn task_retry_key_normalizes_spaces_and_rejects_invalid_present_values() {
        assert_eq!(
            TaskRetryKey::parse(" key ").unwrap(),
            TaskRetryKey::parse("key").unwrap()
        );
        assert!(TaskRetryKey::parse(&"x".repeat(255)).is_ok());
        for invalid in ["", " ", "x\t", "\nx", "x\u{7f}", "é", &"x".repeat(256)] {
            assert_eq!(
                TaskRetryKey::parse(invalid),
                Err(TaskRetryError::InvalidKey)
            );
        }
    }

    #[test]
    fn task_retry_identity_separates_each_authenticated_resource_dimension() {
        let first = scope("api_key", "actor", None, "session", "key");
        assert_eq!(first, scope("api_key", "actor", None, "session", " key "));
        for other in [
            scope("device", "actor", None, "session", "key"),
            scope("api_key", "other", None, "session", "key"),
            scope("api_key", "actor", Some("default"), "session", "key"),
            scope("api_key", "actor", None, "other", "key"),
            scope("api_key", "actor", None, "session", "other"),
        ] {
            assert_ne!(first, other);
        }
        assert_ne!(
            scope("a:b", "c", None, "session", "key"),
            scope("a", "b:c", None, "session", "key")
        );
    }

    #[test]
    fn task_request_fingerprint_ignores_json_object_order_but_preserves_input() {
        let a: Value =
            serde_json::from_str(r#"{"input":{"text":"secret-one"},"metadata":{"b":2,"a":1}}"#)
                .unwrap();
        let b: Value = serde_json::from_str(
            r#"{ "metadata": {"a":1,"b":2}, "input": {"text":"secret-one"} }"#,
        )
        .unwrap();
        assert_eq!(
            TaskRequestFingerprint::from_value(&a),
            TaskRequestFingerprint::from_value(&b)
        );
        assert_ne!(
            TaskRequestFingerprint::from_value(&a),
            TaskRequestFingerprint::from_value(
                &json!({"input":{"text":"secret-two"},"metadata":{"a":1,"b":2}})
            )
        );
        assert_eq!(TaskRetryPolicy::default().supported_method(), None);
        assert_eq!(TaskRetryPolicy::TaskCreate.supported_method(), Some("POST"));
    }
}
