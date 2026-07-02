//! Shared host-environment custody contracts.
//!
//! Harn embedders often need a script or remote runtime to name a host
//! environment class without ever seeing the values in that class. The common
//! pattern is a named environment palette: the orchestrator sends class names
//! over the wire, the host resolves values locally, and receipts/audit trails
//! keep only class names.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Canonical custody mode for a host-held named environment palette.
pub const HOST_ENV_CUSTODY_MODE_NAMED_ENV_PALETTE: &str = "named_env_palette";
/// Wire policy stating that only env class names may cross a protocol boundary.
pub const HOST_ENV_CUSTODY_WIRE_CLASS_NAMES_ONLY: &str = "class_names_only";
/// Host policy stating that credential values are resolved only in the host.
pub const HOST_ENV_CUSTODY_HOST_VALUES_ONLY: &str = "host_values_only";
/// Sandbox policy stating that secret values are never sandbox-visible.
pub const HOST_ENV_CUSTODY_SANDBOX_NO_SECRET_VALUES: &str = "no_secret_values";

/// Serializable contract for a host-resolved named environment palette.
///
/// The contract intentionally carries class names only. It is suitable for
/// protocol metadata, receipts, audit logs, and checkpoint records. It is not a
/// secret container and rejects unknown JSON fields so callers cannot smuggle
/// secret-looking side fields into a validated custody object.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HostEnvCustodyContract {
    /// Custody mode. Currently only `named_env_palette` is accepted.
    pub mode: String,
    /// Env classes whose values are issued by the orchestrator but injected
    /// only into the host process, never into the VM/script payload.
    pub orchestrator_issued_host_env_classes: Vec<String>,
    /// Env classes whose values are held and resolved by the host itself.
    pub host_held_env_classes: Vec<String>,
    /// Env classes the host may expose to a sandboxed command. These should be
    /// credential-free classes such as `agent` or `verify`.
    pub sandbox_visible_env_classes: Vec<String>,
    /// Wire policy. Currently only `class_names_only` is accepted.
    pub wire_value_policy: String,
    /// Host value policy. Currently only `host_values_only` is accepted.
    pub host_value_policy: String,
    /// Sandbox value policy. Currently only `no_secret_values` is accepted.
    pub sandbox_value_policy: String,
}

impl Default for HostEnvCustodyContract {
    fn default() -> Self {
        Self {
            mode: HOST_ENV_CUSTODY_MODE_NAMED_ENV_PALETTE.to_string(),
            orchestrator_issued_host_env_classes: Vec::new(),
            host_held_env_classes: Vec::new(),
            sandbox_visible_env_classes: Vec::new(),
            wire_value_policy: HOST_ENV_CUSTODY_WIRE_CLASS_NAMES_ONLY.to_string(),
            host_value_policy: HOST_ENV_CUSTODY_HOST_VALUES_ONLY.to_string(),
            sandbox_value_policy: HOST_ENV_CUSTODY_SANDBOX_NO_SECRET_VALUES.to_string(),
        }
    }
}

impl HostEnvCustodyContract {
    /// Trim, sort, deduplicate, and validate class-name lists.
    pub fn normalized(mut self) -> Result<Self, HostEnvCustodyError> {
        validate_literal(
            "host_env_custody.mode",
            &self.mode,
            HOST_ENV_CUSTODY_MODE_NAMED_ENV_PALETTE,
        )?;
        validate_literal(
            "host_env_custody.wire_value_policy",
            &self.wire_value_policy,
            HOST_ENV_CUSTODY_WIRE_CLASS_NAMES_ONLY,
        )?;
        validate_literal(
            "host_env_custody.host_value_policy",
            &self.host_value_policy,
            HOST_ENV_CUSTODY_HOST_VALUES_ONLY,
        )?;
        validate_literal(
            "host_env_custody.sandbox_value_policy",
            &self.sandbox_value_policy,
            HOST_ENV_CUSTODY_SANDBOX_NO_SECRET_VALUES,
        )?;
        self.orchestrator_issued_host_env_classes = normalize_env_classes(
            "host_env_custody.orchestrator_issued_host_env_classes",
            self.orchestrator_issued_host_env_classes,
        )?;
        self.host_held_env_classes = normalize_env_classes(
            "host_env_custody.host_held_env_classes",
            self.host_held_env_classes,
        )?;
        self.sandbox_visible_env_classes = normalize_env_classes(
            "host_env_custody.sandbox_visible_env_classes",
            self.sandbox_visible_env_classes,
        )?;
        Ok(self)
    }
}

/// Error returned when a custody contract is not class-name-only metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostEnvCustodyError {
    message: String,
}

impl HostEnvCustodyError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Human-readable validation failure.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for HostEnvCustodyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HostEnvCustodyError {}

fn validate_literal(
    field: &str,
    value: &str,
    expected: &'static str,
) -> Result<(), HostEnvCustodyError> {
    if value == expected {
        return Ok(());
    }
    Err(HostEnvCustodyError::new(format!(
        "{field} must be `{expected}`"
    )))
}

fn normalize_env_classes(
    field: &str,
    values: Vec<String>,
) -> Result<Vec<String>, HostEnvCustodyError> {
    let mut normalized = BTreeSet::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        validate_env_class_name(field, value)?;
        normalized.insert(value.to_string());
    }
    Ok(normalized.into_iter().collect())
}

fn validate_env_class_name(field: &str, value: &str) -> Result<(), HostEnvCustodyError> {
    if value.len() > 96 {
        return Err(HostEnvCustodyError::new(format!(
            "{field} entries must be at most 96 bytes"
        )));
    }
    if !value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'_' | b'-' | b'/')
    }) {
        return Err(HostEnvCustodyError::new(format!(
            "{field} entries must be env_class names, not key/value payloads"
        )));
    }
    if looks_like_secret_value(value) {
        return Err(HostEnvCustodyError::new(format!(
            "{field} entries must be env_class names, not credential values"
        )));
    }
    Ok(())
}

fn looks_like_secret_value(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("github_pat_")
        || lower.starts_with("ghp_")
        || lower.starts_with("gho_")
        || lower.starts_with("ghu_")
        || lower.starts_with("ghs_")
        || lower.starts_with("ghr_")
        || lower.starts_with("glpat-")
        || lower.starts_with("xoxb-")
        || lower.starts_with("xoxp-")
        || lower.starts_with("xapp-")
        || lower.starts_with("sk-")
        || value.starts_with("-----BEGIN")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn host_env_custody_normalizes_class_names() {
        let contract = HostEnvCustodyContract {
            orchestrator_issued_host_env_classes: vec![
                " github.scoped ".to_string(),
                "model.provider".to_string(),
                "github.scoped".to_string(),
            ],
            host_held_env_classes: vec!["customer.registry".to_string()],
            sandbox_visible_env_classes: vec!["verify".to_string(), "agent".to_string()],
            ..HostEnvCustodyContract::default()
        }
        .normalized()
        .expect("contract");

        assert_eq!(
            contract.orchestrator_issued_host_env_classes,
            vec!["github.scoped".to_string(), "model.provider".to_string()]
        );
        assert_eq!(
            contract.sandbox_visible_env_classes,
            vec!["agent".to_string(), "verify".to_string()]
        );
        assert_eq!(
            serde_json::to_value(&contract).expect("json"),
            json!({
                "mode": "named_env_palette",
                "orchestrator_issued_host_env_classes": ["github.scoped", "model.provider"],
                "host_held_env_classes": ["customer.registry"],
                "sandbox_visible_env_classes": ["agent", "verify"],
                "wire_value_policy": "class_names_only",
                "host_value_policy": "host_values_only",
                "sandbox_value_policy": "no_secret_values"
            })
        );
    }

    #[test]
    fn host_env_custody_rejects_secret_like_values() {
        let error =
            HostEnvCustodyContract {
                orchestrator_issued_host_env_classes: vec![
                    "ghp_abcdefghijklmnopqrstuvwxyz".to_string()
                ],
                ..HostEnvCustodyContract::default()
            }
            .normalized()
            .expect_err("credential-looking values are rejected");

        assert_eq!(
            error.message(),
            "host_env_custody.orchestrator_issued_host_env_classes entries must be env_class names, not credential values"
        );
    }

    #[test]
    fn host_env_custody_rejects_key_value_payloads() {
        let error = HostEnvCustodyContract {
            host_held_env_classes: vec!["OPENAI_API_KEY=sk-test".to_string()],
            ..HostEnvCustodyContract::default()
        }
        .normalized()
        .expect_err("env assignments are rejected");

        assert_eq!(
            error.message(),
            "host_env_custody.host_held_env_classes entries must be env_class names, not key/value payloads"
        );
    }

    #[test]
    fn host_env_custody_rejects_unknown_json_fields() {
        let error = serde_json::from_value::<HostEnvCustodyContract>(json!({
            "mode": "named_env_palette",
            "token": "ghp_should_not_parse"
        }))
        .expect_err("unknown fields rejected");

        assert!(
            error.to_string().contains("unknown field `token`"),
            "unexpected error: {error}"
        );
    }
}
