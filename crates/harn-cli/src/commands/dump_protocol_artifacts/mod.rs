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

mod constants;
mod manifest;
mod support;
mod values;

mod go;
mod python;
mod rust;
mod swift;
mod typescript;

#[cfg(test)]
mod tests;

use std::fs;
use std::path::Path;
use std::process;

use harn_vm::llm::receipts::TOOL_CALL_RECEIPT_SCHEMA_ARTIFACT;

use constants::*;
use go::*;
use manifest::*;
use python::*;
use rust::*;
use support::*;
use swift::*;
use typescript::*;

pub(crate) use manifest::manifest_json;

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
    let artifacts = generate_artifacts().unwrap_or_else(|error| {
        eprintln!("error: failed to generate protocol artifacts: {error}");
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
                Ok(_) => stale.push(path),
                Err(_) => stale.push(path),
            }
        }
        if !stale.is_empty() {
            eprintln!("error: protocol artifacts are stale or missing:");
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
        if let Some(parent) = path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                eprintln!("error: cannot create {}: {error}", parent.display());
                process::exit(1);
            }
        }
        if let Err(error) = fs::write(&path, artifact.contents) {
            eprintln!("error: cannot write {}: {error}", path.display());
            process::exit(1);
        }
        println!("wrote {}", path.display());
    }
}

fn generate_artifacts() -> Result<Vec<Artifact>, String> {
    let go_artifact = generate_go_artifact()?;
    let mut artifacts = vec![
        Artifact::new("README.md", generate_readme()),
        Artifact::new("manifest.json", generate_manifest()?),
        Artifact::new("harn-protocol.ts", generate_typescript()),
        Artifact::new("HarnProtocol.swift", generate_swift()),
        Artifact::new("harn-protocol.rs", generate_rust()),
        Artifact::new("python/harn_protocol.py", generate_python()),
        Artifact::new("python/__init__.py", PYTHON_INIT_STUB.to_string()),
        Artifact::new("go/harnprotocol/harnprotocol.go", go_artifact),
        Artifact::new("go/harnprotocol/go.mod", generate_go_mod()),
        Artifact::new("fixtures/round_trip.json", generate_round_trip_fixture()?),
        Artifact::new(
            TOOL_CALL_RECEIPT_SCHEMA_ARTIFACT,
            generate_tool_call_receipt_schema()?,
        ),
    ];

    for schema in SCHEMA_COPIES {
        artifacts.push(Artifact::new(
            schema.artifact,
            read_repo_text(schema.source)?,
        ));
    }
    artifacts.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(artifacts)
}
