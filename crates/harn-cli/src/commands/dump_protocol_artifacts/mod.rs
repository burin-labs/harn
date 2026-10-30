//! Generate the published Harn protocol contract artifacts.
//!
//! The protocol conformance matrix pins the adapter behavior. This command
//! turns the same wire vocabulary into checked-in JSON Schema copies plus
//! TypeScript, Swift, Python, Go, and Rust definitions that downstream hosts
//! can consume without mirroring Harn enums by hand.
//!
//! The generator is split by concern: [`constants`] holds the wire
//! vocabulary, [`values`] derives canonical value lists from the live Harn
//! enums, [`support`] carries shared codegen text helpers, [`manifest`]
//! emits the manifest/README/round-trip meta artifacts, and one module per
//! target language owns that language's emitter.

mod activity;
mod agent_event_payloads;
mod agents_api;
mod canonical_history;
mod connector_setup;
mod constants;
mod external_action;
mod harn_records;
mod inference_admission;
mod manifest;
mod plan_records;
mod prepared_session;
mod prompt_correlation;
mod provider_usage;
mod recap_records;
mod records;
mod schema_records;
mod session_recap;
mod session_update_payloads;
mod session_update_rust_validation;
mod session_update_validation;
mod support;
mod values;

mod go;
mod python;
mod run_session_view;
mod rust;
mod swift;
mod typescript;

#[cfg(test)]
mod tests;

use std::fs;
use std::path::Path;
use std::process;

use crate::commands::generated_artifact::write_file;

use harn_vm::llm::plan::PLAN_DOCUMENT_SCHEMA_ARTIFACT;
use harn_vm::llm::receipts::TOOL_CALL_RECEIPT_SCHEMA_ARTIFACT;
use harn_vm::tool_registry::{
    tool_catalog_json_schema, tool_catalog_typescript, TOOL_CATALOG_SCHEMA_ARTIFACT,
    TOOL_CATALOG_TYPESCRIPT_ARTIFACT,
};

use activity::ActivityVocabulary;
use connector_setup::ConnectorSetupVocabulary;
use constants::*;
use external_action::ExternalActionVocabulary;
use go::*;
use manifest::*;
use python::*;
use rust::*;
use support::*;
use swift::*;
use typescript::*;

pub(crate) use manifest::{manifest_json, manifest_json_from};

#[derive(Debug)]
struct Artifact {
    relative_path: String,
    contents: String,
}

impl Artifact {
    fn new(relative_path: impl Into<String>, contents: impl Into<String>) -> Self {
        Self {
            relative_path: relative_path.into(),
            contents: ensure_trailing_newline(contents.into()),
        }
    }
}

pub(crate) fn run(output_dir: &str, check_only: bool) {
    let source = ProtocolArtifactSource::discover().unwrap_or_else(|error| {
        cli_diagnostic!("error: failed to locate protocol sources: {error}");
        process::exit(1);
    });
    let artifacts = generate_artifacts(&source).unwrap_or_else(|error| {
        cli_diagnostic!("error: failed to generate protocol artifacts: {error}");
        process::exit(1);
    });
    let output_root = Path::new(output_dir);

    if check_only {
        let mut stale = Vec::new();
        for artifact in &artifacts {
            let path = output_root.join(&artifact.relative_path);
            match fs::read_to_string(&path) {
                Ok(existing)
                    if normalize_line_endings(&existing)
                        == normalize_line_endings(&artifact.contents) => {}
                Ok(_) | Err(_) => stale.push(path),
            }
        }
        if !stale.is_empty() {
            cli_diagnostic!("error: protocol artifacts are stale or missing:");
            for path in stale {
                eprintln!("  {}", path.display());
            }
            eprintln!("hint: run `make gen-protocol-artifacts` to regenerate.");
            process::exit(1);
        }
        return;
    }

    for artifact in artifacts {
        let path = output_root.join(&artifact.relative_path);
        if let Err(error) = write_file(&path, &artifact.contents) {
            cli_diagnostic!("error: {error}");
            process::exit(1);
        }
        println!("wrote {}", path.display());
    }
}

fn generate_artifacts(source: &ProtocolArtifactSource) -> Result<Vec<Artifact>, String> {
    use records::Target;
    let agents = agents_api::AgentsApiProjection::load(source)?;
    let view_schema = run_session_view::schema();
    let external_actions = ExternalActionVocabulary::load(source)?;
    let connector_setup = ConnectorSetupVocabulary::load(source)?;
    let activity = ActivityVocabulary::load(source)?;
    let session_updates = session_update_payloads::SessionUpdatePayloads::load(source)?;
    let agent_events = agent_event_payloads::AgentEventPayloads::load()?;
    let go_artifact = format_go_source(agent_events.append(
        agents.append(generate_go_artifact(&session_updates)?, Target::Go),
        Target::Go,
    ))?;
    let mut artifacts = vec![
        Artifact::new(
            run_session_view::SWIFT_ARTIFACT,
            run_session_view::generate_swift(&view_schema)?,
        ),
        Artifact::new(
            harn_vm::orchestration::RUN_SESSION_VIEW_SCHEMA_ARTIFACT,
            serde_json::to_string_pretty(&view_schema).map_err(|error| error.to_string())?,
        ),
        Artifact::new("README.md", generate_readme()),
        Artifact::new(
            "manifest.json",
            generate_manifest_with_vocabularies(
                source,
                &external_actions,
                &connector_setup,
                &activity,
            )?,
        ),
        Artifact::new(
            "harn-protocol.ts",
            agent_events.append(
                agents.append(
                    generate_typescript(
                        &external_actions,
                        &connector_setup,
                        &activity,
                        &session_updates,
                    ),
                    Target::Typescript,
                ),
                Target::Typescript,
            ),
        ),
        Artifact::new(
            "HarnProtocol.swift",
            agent_events.append(
                agents.append(
                    generate_swift(
                        &external_actions,
                        &connector_setup,
                        &activity,
                        &session_updates,
                    ),
                    Target::Swift,
                ),
                Target::Swift,
            ),
        ),
        Artifact::new(
            "harn-protocol.rs",
            format_rust_source(
                agent_events.append(
                    agents.append(
                        generate_rust(
                            &external_actions,
                            &connector_setup,
                            &activity,
                            &session_updates,
                        ),
                        Target::Rust,
                    ),
                    Target::Rust,
                ),
                source.repo_root(),
            )?,
        ),
        Artifact::new(
            "python/harn_protocol.py",
            agent_events.append(
                agents.append(
                    generate_python_with_payloads(&session_updates),
                    Target::Python,
                ),
                Target::Python,
            ),
        ),
        Artifact::new(
            agent_event_payloads::SCHEMA_ARTIFACT,
            agent_events.schema_json()?,
        ),
        Artifact::new("python/__init__.py", PYTHON_INIT_STUB.to_string()),
        Artifact::new("go/harnprotocol/harnprotocol.go", go_artifact),
        Artifact::new("go/harnprotocol/go.mod", generate_go_mod()),
        Artifact::new("fixtures/round_trip.json", generate_round_trip_fixture()?),
        Artifact::new(
            TOOL_CALL_RECEIPT_SCHEMA_ARTIFACT,
            generate_tool_call_receipt_schema()?,
        ),
        Artifact::new(
            PLAN_DOCUMENT_SCHEMA_ARTIFACT,
            serde_json::to_string_pretty(&harn_vm::llm::plan::plan_document_json_schema())
                .map_err(|error| format!("failed to encode plan document schema: {error}"))?,
        ),
        Artifact::new(
            TOOL_CATALOG_SCHEMA_ARTIFACT,
            serde_json::to_string_pretty(&tool_catalog_json_schema())
                .map_err(|error| format!("failed to encode tool catalog schema: {error}"))?,
        ),
        Artifact::new(TOOL_CATALOG_TYPESCRIPT_ARTIFACT, tool_catalog_typescript()),
        Artifact::new(
            harn_vm::session_recap::SESSION_RECAP_SCHEMA_ARTIFACT,
            serde_json::to_string_pretty(&harn_vm::session_recap::session_recap_json_schema())
                .map_err(|error| format!("failed to encode session recap schema: {error}"))?,
        ),
        Artifact::new(
            harn_vm::prepared_run::PREPARED_SESSION_SCHEMA_ARTIFACT,
            harn_vm::prepared_run::PREPARED_SESSION_V1_SCHEMA_JSON,
        ),
        Artifact::new(
            harn_vm::agent_events::session_health::SESSION_HEALTH_SCHEMA_ARTIFACT,
            serde_json::to_string_pretty(
                &harn_vm::agent_events::session_health::session_health_schema(),
            )
            .map_err(|error| format!("failed to encode session health schema: {error}"))?,
        ),
    ];

    artifacts.push(Artifact::new(
        "schemas/canonical-history-boundaries.schema.json",
        serde_json::to_string_pretty(&canonical_history::schema())
            .map_err(|error| error.to_string())?,
    ));
    for (name, schema) in inference_admission::schemas() {
        artifacts.push(Artifact::new(
            format!(
                "schemas/{}.schema.json",
                records::snake_ident(name).replace('_', "-")
            ),
            serde_json::to_string_pretty(&schema).map_err(|error| error.to_string())?,
        ));
    }
    for schema in SCHEMA_COPIES {
        artifacts.push(Artifact::new(
            schema.artifact,
            source.read_text(schema.source)?,
        ));
    }
    artifacts.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(artifacts)
}
