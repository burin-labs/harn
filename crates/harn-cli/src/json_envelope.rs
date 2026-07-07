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
/// E2.1 seeds the commands that ship a `schema_version` today (doctor,
/// session export, the provider catalog). New commands register here as
/// they migrate to [`JsonEnvelope`] — for example, the `skills` family
/// added in E3.2.
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
            command: "provider catalog show",
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
            command: "mcp status",
            schema_version: crate::commands::mcp::MCP_STATUS_SCHEMA_VERSION,
            description: "Per-server MCP readiness: transport, connection state, tool/resource/prompt counts, last error.",
            schema_json: None,
        },
        SchemaEntry {
            command: "mcp discover",
            schema_version: crate::commands::mcp::MCP_DISCOVERY_SCHEMA_VERSION,
            description:
                "Unofficial MCP endpoint discovery from /.well-known/mcp.json: source URL, found flag, and descriptor.",
            schema_json: None,
        },
        SchemaEntry {
            command: "run",
            schema_version: crate::commands::run::json_events::RUN_JSON_SCHEMA_VERSION,
            description: "Pipeline-run NDJSON event stream (stdout, stderr, transcript, tool, hook, persona, result, error).",
            schema_json: None,
        },
        SchemaEntry {
            command: "parse",
            schema_version: crate::commands::parse_tokens::PARSE_JSON_SCHEMA_VERSION,
            description: "Tagged Harn AST tree with byte spans for parser tooling.",
            schema_json: None,
        },
        SchemaEntry {
            command: "tokens",
            schema_version: crate::commands::parse_tokens::TOKENS_JSON_SCHEMA_VERSION,
            description: "Lexer token stream with source lexemes and byte spans.",
            schema_json: None,
        },
        SchemaEntry {
            command: "check",
            schema_version: crate::commands::check::CHECK_SCHEMA_VERSION,
            description: "Per-file static check results with diagnostics and summary counts.",
            schema_json: None,
        },
        SchemaEntry {
            command: "fmt",
            schema_version: crate::commands::check::FMT_SCHEMA_VERSION,
            description: "Per-file formatting result report for write and check modes.",
            schema_json: None,
        },
        SchemaEntry {
            command: "check provider-matrix",
            schema_version: crate::commands::check::provider_matrix::PROVIDER_MATRIX_SCHEMA_VERSION,
            description: "Provider/model capability matrix rows.",
            schema_json: None,
        },
        SchemaEntry {
            command: "provider catalog support",
            schema_version: crate::commands::provider_support::PROVIDER_SUPPORT_SCHEMA_VERSION,
            description: "Generated provider recommendation and support matrix.",
            schema_json: None,
        },
        SchemaEntry {
            command: "models batch plan",
            schema_version: 1,
            description:
                "Provider Batch API candidates plus Harn live-adapter support for offline workloads.",
            schema_json: None,
        },
        SchemaEntry {
            command: "models batch manifest",
            schema_version: 1,
            description:
                "Provider-neutral offline batch manifest summary and request groups.",
            schema_json: None,
        },
        SchemaEntry {
            command: "models batch prepare",
            schema_version: 1,
            description:
                "Provider-native batch request files, deterministic prepare receipt, and lifecycle state.",
            schema_json: None,
        },
        SchemaEntry {
            command: "models batch submit",
            schema_version: 1,
            description:
                "Batch submission receipt with provider job ids, dry-run operations, and lifecycle state.",
            schema_json: None,
        },
        SchemaEntry {
            command: "models batch status",
            schema_version: 1,
            description:
                "Provider batch status receipt with cached/dry-run validation and lifecycle counts.",
            schema_json: None,
        },
        SchemaEntry {
            command: "models batch cancel",
            schema_version: 1,
            description:
                "Batch cancellation receipt with redacted cancel operations, skipped-job reasons, and lifecycle counts.",
            schema_json: None,
        },
        SchemaEntry {
            command: "models batch download",
            schema_version: 1,
            description:
                "Provider result-file download receipt with artifact paths, hashes, and lifecycle counts.",
            schema_json: None,
        },
        SchemaEntry {
            command: "models lora plan",
            schema_version: 1,
            description: "Portable LoRA/QLoRA route plan: base model, tool-call format, trainer, data, eval, and launch contract.",
            schema_json: None,
        },
        SchemaEntry {
            command: "models lora inspect",
            schema_version: 1,
            description:
                "PEFT LoRA adapter compatibility report with base-model, provider, tool-call, and launch metadata.",
            schema_json: None,
        },
        SchemaEntry {
            command: "models lora export",
            schema_version: 1,
            description:
                "Trainer-ready LoRA dataset export report, including contract id, manifest paths, stats, and validation results.",
            schema_json: None,
        },
        SchemaEntry {
            command: "models lora manifest",
            schema_version: 1,
            description:
                "Canonical LoRA training-run manifest with route, data, artifact, serving, and promotion contracts.",
            schema_json: None,
        },
        SchemaEntry {
            command: "models lora preflight",
            schema_version: 1,
            description:
                "LoRA corpus readiness report before GPU training, including sequence-fit, tool-call shape, and threshold failures.",
            schema_json: None,
        },
        SchemaEntry {
            command: "models lora train",
            schema_version: 1,
            description:
                "LoRA trainer backend receipt with route contract, dataset hashes, backend argv, and post-training manifest commands.",
            schema_json: None,
        },
        SchemaEntry {
            command: "check connector-matrix",
            schema_version: crate::commands::check::connector_matrix::CONNECTOR_MATRIX_SCHEMA_VERSION,
            description: "Connector package capability matrix rows.",
            schema_json: None,
        },
        SchemaEntry {
            command: "test conformance",
            schema_version: crate::commands::test::CONFORMANCE_TEST_SCHEMA_VERSION,
            description:
                "Conformance test results with xfail accounting and a stable fixture snapshot key.",
            schema_json: None,
        },
        SchemaEntry {
            command: "test --json-out",
            schema_version: crate::test_report::USER_TEST_REPORT_SCHEMA_VERSION,
            description:
                "User-test report (`--json-out`): per-case name/file/classname/outcome/duration plus suite-level summary.",
            schema_json: None,
        },
        SchemaEntry {
            command: "time run",
            schema_version: crate::commands::time::TIME_RUN_SCHEMA_VERSION,
            description:
                "Per-phase wall-clock + cache hit/miss + per-LLM/tool-call latency for `harn run`.",
            schema_json: None,
        },
        SchemaEntry {
            command: "fix plan",
            schema_version: crate::commands::fix::FIX_PLAN_SCHEMA_VERSION,
            description: "Plan repair-bearing diagnostics without editing files.",
            schema_json: None,
        },
        SchemaEntry {
            command: "fix apply",
            schema_version: crate::commands::fix::FIX_APPLY_SCHEMA_VERSION,
            description: "Apply clean repair edits at or below a declared safety ceiling.",
            schema_json: None,
        },
        SchemaEntry {
            command: "skills list",
            schema_version: 1,
            description: "Canonical Harn skill corpus, frontmatter only.",
            schema_json: None,
        },
        SchemaEntry {
            command: "skills get",
            schema_version: 1,
            description: "One canonical skill's frontmatter (and body with --full).",
            schema_json: None,
        },
        SchemaEntry {
            command: "pack",
            schema_version: crate::commands::pack::PACK_SCHEMA_VERSION,
            description: "Signed-ready .harnpack run-bundle build summary.",
            schema_json: Some(crate::commands::pack::json_schema()),
        },
        SchemaEntry {
            command: "pack verify",
            schema_version: crate::commands::pack::PACK_VERIFY_SCHEMA_VERSION,
            description:
                "Result of verifying a .harnpack: bundle hash, signature, per-module hashes.",
            schema_json: Some(crate::commands::pack::verify_json_schema()),
        },
        SchemaEntry {
            command: "dev",
            schema_version: 1,
            description: "`harn dev --watch` incremental NDJSON event stream (ready / fingerprint_changed / rerun / diagnostics / tests).",
            schema_json: None,
        },
        SchemaEntry {
            command: "routes",
            schema_version: 1,
            description: "Static trigger route, budget, capability, and vendor-lock inventory.",
            schema_json: None,
        },
        SchemaEntry {
            command: "usage",
            schema_version: crate::commands::usage::USAGE_SCHEMA_VERSION,
            description:
                "LLM spend/usage rollup from the event log: per-group calls, cost_usd, tokens, cache telemetry, and time-series cumulatives.",
            schema_json: None,
        },
        SchemaEntry {
            command: "graph",
            schema_version: crate::commands::graph::GRAPH_SCHEMA_VERSION,
            description:
                "Static module graph with public symbols, imports, capabilities, effects, and host-call surface.",
            schema_json: None,
        },
        SchemaEntry {
            command: "lint",
            schema_version: crate::commands::check::LINT_SCHEMA_VERSION,
            description:
                "Per-file lint diagnostics with severity, fixable/fixed counts, and summary.",
            schema_json: None,
        },
        SchemaEntry {
            command: "replay",
            schema_version: crate::commands::replay::REPLAY_SCHEMA_VERSION,
            description:
                "Replay summary: per-stage status/outcome/branch, embedded fixture verdicts, and multi-run determinism.",
            schema_json: None,
        },
        SchemaEntry {
            command: "version",
            schema_version: crate::VERSION_SCHEMA_VERSION,
            description: "CLI build metadata: name, version, description.",
            schema_json: None,
        },
        SchemaEntry {
            command: "upgrade",
            schema_version: crate::commands::upgrade::UPGRADE_SCHEMA_VERSION,
            description:
                "Self-update probe (`--check`) or install summary: current, target, archive URL, install outcome.",
            schema_json: None,
        },
        SchemaEntry {
            command: "explain --catalog",
            schema_version: crate::commands::diagnostics_catalog::SCHEMA_VERSION,
            description:
                "Diagnostic-code catalog: per-code summary, repair, safety, related codes.",
            schema_json: None,
        },
        SchemaEntry {
            command: "mcp presets",
            schema_version: crate::commands::mcp::presets::MCP_PRESETS_SCHEMA_VERSION,
            description:
                "Canonical catalog of well-known MCP server presets (Notion, Linear, GitHub, filesystem): id, transport, command/url template, auth kind, and required placeholders.",
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
    fn catalog_includes_fix_plan() {
        let entries = catalog();
        let entry = entries
            .iter()
            .find(|entry| entry.command == "fix plan")
            .expect("fix plan schema should be registered");
        assert_eq!(
            entry.schema_version,
            crate::commands::fix::FIX_PLAN_SCHEMA_VERSION
        );
        let entry = entries
            .iter()
            .find(|entry| entry.command == "fix apply")
            .expect("fix apply schema should be registered");
        assert_eq!(
            entry.schema_version,
            crate::commands::fix::FIX_APPLY_SCHEMA_VERSION
        );
    }

    #[test]
    fn catalog_includes_models_lora_commands() {
        let entries = catalog();
        for command in [
            "models lora plan",
            "models lora inspect",
            "models lora export",
            "models lora manifest",
            "models lora preflight",
            "models lora train",
        ] {
            let entry = entries
                .iter()
                .find(|entry| entry.command == command)
                .unwrap_or_else(|| panic!("{command} schema should be registered"));
            assert_eq!(entry.schema_version, 1);
        }
    }

    #[test]
    fn catalog_includes_models_batch_commands() {
        let entries = catalog();
        for command in [
            "models batch plan",
            "models batch manifest",
            "models batch prepare",
            "models batch submit",
            "models batch status",
            "models batch cancel",
            "models batch download",
        ] {
            let entry = entries
                .iter()
                .find(|entry| entry.command == command)
                .unwrap_or_else(|| panic!("{command} schema should be registered"));
            assert_eq!(entry.schema_version, 1);
        }
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
