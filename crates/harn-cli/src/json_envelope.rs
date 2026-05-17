//! Canonical JSON envelope for `harn` CLI commands.
//!
//! Every `--json` mode returns a [`JsonEnvelope<T>`] — a versioned
//! wrapper that exposes `schemaVersion`, `ok`, and either `data` or
//! `error`. Soft signals attach as `warnings` so `ok: true` stays
//! stable as long as the command succeeds.
//!
//! Schema versions are per-command and monotonically increasing.
//! [`catalog`] returns the registry consumed by `harn --json-schemas`.
//! New commands extend the catalog (and bump their own
//! [`JsonOutput::SCHEMA_VERSION`]) when their JSON shape changes in a
//! way agents need to detect.
//!
//! See epic #1753 (`--json` everywhere) for the broader contract.

use serde::{Deserialize, Serialize};

/// Schema version of the `harn --json-schemas` catalog itself. Bump
/// when the shape of [`SchemaEntry`] or the catalog envelope changes.
pub const CATALOG_SCHEMA_VERSION: u32 = 1;

/// Versioned wrapper for every `--json` CLI output. All five fields
/// are always serialized so consumers can rely on a flat shape:
/// missing payloads surface as `null` and the empty `warnings` array
/// is `[]` rather than absent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonEnvelope<T: Serialize> {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub ok: bool,
    pub data: Option<T>,
    pub error: Option<JsonError>,
    #[serde(default)]
    pub warnings: Vec<JsonWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonError {
    pub code: String,
    pub message: String,
    /// Free-form structured context. `null` when the error has no
    /// structured payload — the field is always present so consumers
    /// can read `error.details` without an existence check.
    #[serde(default)]
    pub details: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonWarning {
    pub code: String,
    pub message: String,
}

/// Implemented by every CLI command that exposes a `--json` mode. The
/// associated `SCHEMA_VERSION` is also surfaced in [`catalog`] so
/// agents can negotiate per-command compatibility without parsing
/// every payload.
pub trait JsonOutput {
    const SCHEMA_VERSION: u32;
    type Data: Serialize;
    fn into_envelope(self) -> JsonEnvelope<Self::Data>;
}

impl<T: Serialize> JsonEnvelope<T> {
    pub fn ok(schema_version: u32, data: T) -> Self {
        Self {
            schema_version,
            ok: true,
            data: Some(data),
            error: None,
            warnings: Vec::new(),
        }
    }

    pub fn err(
        schema_version: u32,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> JsonEnvelope<T> {
        Self {
            schema_version,
            ok: false,
            data: None,
            error: Some(JsonError {
                code: code.into(),
                message: message.into(),
                details: serde_json::Value::Null,
            }),
            warnings: Vec::new(),
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        if let Some(err) = self.error.as_mut() {
            err.details = details;
        }
        self
    }

    pub fn with_warning(mut self, code: impl Into<String>, message: impl Into<String>) -> Self {
        self.warnings.push(JsonWarning {
            code: code.into(),
            message: message.into(),
        });
        self
    }
}

/// One row of the `harn --json-schemas` catalog. `schema_json` is
/// inline when small; richer schemas live behind a future
/// `schema_url` field documented per-command.
#[derive(Debug, Clone, Serialize)]
pub struct SchemaEntry {
    pub command: &'static str,
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub description: &'static str,
    #[serde(skip_serializing_if = "Option::is_none", rename = "schemaJson")]
    pub schema_json: Option<serde_json::Value>,
}

/// Static catalog of commands that already emit a stable JSON shape.
///
/// E2.1 only registers the commands that ship a `schema_version` in
/// their JSON today (doctor, session export, the provider catalog).
/// E2.2–E2.6 will add `run`, `test`, `parse`, `tokens`, `graph`, and
/// `fmt` as they migrate to [`JsonEnvelope`].
pub fn catalog() -> Vec<SchemaEntry> {
    vec![
        SchemaEntry {
            command: "doctor",
            schema_version: crate::commands::doctor::DOCTOR_SCHEMA_VERSION,
            description: "Capability matrix: host, per-target buildability, per-provider reachability, per-stdlib-effect availability.",
            schema_json: None,
        },
        SchemaEntry {
            command: "session export",
            schema_version: 1,
            description: "Portable Harn session bundle export.",
            schema_json: None,
        },
        SchemaEntry {
            command: "provider-catalog",
            schema_version: 1,
            description: "Resolved provider/model catalog snapshot.",
            schema_json: None,
        },
        SchemaEntry {
            command: "connect status",
            schema_version: 1,
            description: "Outbound-connector readiness report.",
            schema_json: None,
        },
        SchemaEntry {
            command: "connect setup-plan",
            schema_version: 1,
            description: "Step-by-step plan to bring a connector online.",
            schema_json: None,
        },
        SchemaEntry {
            command: "run",
            schema_version: crate::commands::run::json_events::RUN_JSON_SCHEMA_VERSION,
            description: "Pipeline-run NDJSON event stream (stdout, stderr, transcript, tool, hook, persona, result, error).",
            schema_json: None,
        },
        SchemaEntry {
            command: "time run",
            schema_version: crate::commands::time::TIME_RUN_SCHEMA_VERSION,
            description:
                "Per-phase wall-clock + cache hit/miss + per-LLM/tool-call latency for `harn run`.",
            schema_json: None,
        },
    ]
}

/// Encode an envelope as JSON. Uses pretty form so humans tailing the
/// terminal can still read it; agents `jq`-pipe either form.
pub fn to_string_pretty<T: Serialize>(envelope: &JsonEnvelope<T>) -> String {
    serde_json::to_string_pretty(envelope).expect("JsonEnvelope serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Serialize)]
    struct Payload {
        value: u32,
    }

    #[test]
    fn ok_envelope_round_trips() {
        let env = JsonEnvelope::ok(7, Payload { value: 42 });
        let v: serde_json::Value = serde_json::to_value(&env).unwrap();
        assert_eq!(v["schemaVersion"], 7);
        assert_eq!(v["ok"], true);
        assert_eq!(v["data"]["value"], 42);
        // All envelope fields are always serialized; absent payloads
        // surface as JSON `null` / `[]`.
        assert!(v["error"].is_null());
        assert_eq!(v["warnings"], json!([]));
    }

    #[test]
    fn err_envelope_carries_details() {
        let env: JsonEnvelope<()> = JsonEnvelope::err(2, "io", "disk full")
            .with_details(json!({ "path": "/var/log/harn" }));
        let v: serde_json::Value = serde_json::to_value(&env).unwrap();
        assert_eq!(v["schemaVersion"], 2);
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "io");
        assert_eq!(v["error"]["message"], "disk full");
        assert_eq!(v["error"]["details"]["path"], "/var/log/harn");
        assert!(v["data"].is_null());
    }

    #[test]
    fn warnings_serialize_when_present() {
        let env = JsonEnvelope::ok(1, Payload { value: 1 })
            .with_warning("deprecated.flag", "--format=json is deprecated");
        let v: serde_json::Value = serde_json::to_value(&env).unwrap();
        assert_eq!(v["warnings"][0]["code"], "deprecated.flag");
        assert_eq!(v["warnings"][0]["message"], "--format=json is deprecated");
    }

    #[test]
    fn catalog_is_nonempty_and_unique() {
        let entries = catalog();
        assert!(!entries.is_empty(), "catalog should ship with E2.1 seeds");
        let mut commands: Vec<_> = entries.iter().map(|e| e.command).collect();
        commands.sort();
        let unique_count = {
            let mut deduped = commands.clone();
            deduped.dedup();
            deduped.len()
        };
        assert_eq!(commands.len(), unique_count, "command names must be unique");
    }

    #[test]
    fn schema_versions_are_positive() {
        for entry in catalog() {
            assert!(
                entry.schema_version >= 1,
                "{} should have schemaVersion >= 1",
                entry.command
            );
        }
    }
}
