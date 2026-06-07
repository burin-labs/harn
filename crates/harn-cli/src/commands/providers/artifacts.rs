use std::fs;
use std::path::{Path, PathBuf};

use harn_vm::llm_config::ProvidersConfig;
use serde_json::json;

use crate::cli::{ProvidersExportArgs, ProvidersValidateArgs};

pub(crate) fn run_validate(args: &ProvidersValidateArgs) -> Result<(), String> {
    let overlay = load_overlay(args.overlay.as_deref())?;
    // Generation is hermetic: validate the artifact built from the compiled-in
    // embedded catalog (plus any explicit `--overlay`), never the developer's
    // home config or environment. Otherwise a personal
    // `~/.config/harn/providers.toml` would leak aliases/providers into the
    // catalog we validate and ship.
    let artifact = harn_vm::provider_catalog::artifact_embedded(overlay.as_ref());
    let logical = harn_vm::provider_catalog::validate_artifact(&artifact);
    let schema = harn_vm::provider_catalog::schema_value();
    let artifact_value = serde_json::to_value(&artifact)
        .map_err(|error| format!("failed to serialize provider catalog: {error}"))?;
    let mut schema_errors = Vec::new();
    validate_against_schema(&schema, &artifact_value, &mut schema_errors)?;
    let mut drift = Vec::new();
    if args.check_artifacts {
        drift = artifact_drift(&args.artifact_dir, overlay.as_ref())?;
    }

    if args.json {
        let payload = json!({
            "valid": logical.errors.is_empty() && schema_errors.is_empty() && drift.is_empty(),
            "errors": logical.errors,
            "warnings": logical.warnings,
            "schema_errors": schema_errors,
            "artifact_drift": drift,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|error| format!("failed to render validation JSON: {error}"))?
        );
    } else {
        for warning in &logical.warnings {
            eprintln!("warning: {warning}");
        }
        for error in &logical.errors {
            eprintln!("error: {error}");
        }
        for error in &schema_errors {
            eprintln!("error: {error}");
        }
        for path in &drift {
            eprintln!("error: generated artifact is stale or missing: {path}");
        }
        if logical.errors.is_empty() && schema_errors.is_empty() && drift.is_empty() {
            println!("provider catalog validation OK");
        }
    }

    if logical.errors.is_empty() && schema_errors.is_empty() && drift.is_empty() {
        Ok(())
    } else {
        Err("provider catalog validation failed".to_string())
    }
}

pub(crate) fn run_export(args: &ProvidersExportArgs) -> Result<(), String> {
    let overlay = load_overlay(args.overlay.as_deref())?;
    // Export from the embedded catalog only (plus any explicit `--overlay`) so
    // the checked-in artifacts are a pure function of the source tree.
    let artifacts = generated_artifacts(overlay.as_ref())?;
    if args.check {
        let drift = artifact_drift_from(&args.output_dir, &artifacts)?;
        if drift.is_empty() {
            println!("provider catalog artifacts are up to date");
            return Ok(());
        }
        for path in drift {
            eprintln!("error: generated artifact is stale or missing: {path}");
        }
        return Err("provider catalog artifacts drifted".to_string());
    }

    fs::create_dir_all(&args.output_dir).map_err(|error| {
        format!(
            "failed to create artifact directory {}: {error}",
            args.output_dir.display()
        )
    })?;
    for artifact in artifacts {
        let path = args.output_dir.join(artifact.relative_path);
        fs::write(&path, artifact.body)
            .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

/// Parse an explicit `--overlay` providers.toml file into a config to merge on
/// top of the embedded catalog. Returning the parsed overlay (instead of
/// installing it via `set_user_overrides`) keeps generation hermetic: only this
/// declared overlay influences the artifacts, never ambient thread-local state.
fn load_overlay(path: Option<&Path>) -> Result<Option<ProvidersConfig>, String> {
    let Some(path) = path else {
        return Ok(None);
    };
    let src = fs::read_to_string(path)
        .map_err(|error| format!("failed to read overlay {}: {error}", path.display()))?;
    let overlay = harn_vm::llm_config::parse_config_toml(&src)
        .map_err(|error| format!("failed to parse overlay {}: {error}", path.display()))?;
    Ok(Some(overlay))
}

fn validate_against_schema(
    schema: &serde_json::Value,
    artifact: &serde_json::Value,
    errors: &mut Vec<String>,
) -> Result<(), String> {
    jsonschema::draft202012::meta::validate(schema)
        .map_err(|error| format!("provider catalog schema is invalid: {error}"))?;
    let validator = jsonschema::draft202012::new(schema)
        .map_err(|error| format!("failed to compile provider catalog schema: {error}"))?;
    if let Err(error) = validator.validate(artifact) {
        errors.push(format!(
            "generated catalog failed schema validation: {error}"
        ));
    }
    Ok(())
}

struct GeneratedArtifact {
    relative_path: &'static str,
    body: String,
}

fn generated_artifacts(
    overlay: Option<&ProvidersConfig>,
) -> Result<Vec<GeneratedArtifact>, String> {
    Ok(vec![
        GeneratedArtifact {
            relative_path: "provider-catalog.json",
            body: harn_vm::provider_catalog::artifact_json_embedded(overlay)
                .map_err(|error| format!("failed to generate catalog JSON: {error}"))?,
        },
        GeneratedArtifact {
            relative_path: "provider-catalog.schema.json",
            body: harn_vm::provider_catalog::schema_json()
                .map_err(|error| format!("failed to generate catalog schema: {error}"))?,
        },
        GeneratedArtifact {
            relative_path: "harn-provider-catalog.ts",
            body: harn_vm::provider_catalog::typescript_binding_embedded(overlay)
                .map_err(|error| format!("failed to generate TypeScript binding: {error}"))?,
        },
        GeneratedArtifact {
            relative_path: "HarnProviderCatalog.swift",
            body: harn_vm::provider_catalog::swift_binding_embedded(overlay)
                .map_err(|error| format!("failed to generate Swift binding: {error}"))?,
        },
    ])
}

fn artifact_drift(dir: &Path, overlay: Option<&ProvidersConfig>) -> Result<Vec<String>, String> {
    artifact_drift_from(dir, &generated_artifacts(overlay)?)
}

fn artifact_drift_from(dir: &Path, artifacts: &[GeneratedArtifact]) -> Result<Vec<String>, String> {
    let mut drift = Vec::new();
    for artifact in artifacts {
        let path: PathBuf = dir.join(artifact.relative_path);
        match fs::read_to_string(&path) {
            Ok(existing) if existing == artifact.body => {}
            Ok(_) | Err(_) => drift.push(path.display().to_string()),
        }
    }
    Ok(drift)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_artifacts_include_downstream_bindings() {
        let artifacts = generated_artifacts(None).expect("artifacts generate");
        let names: Vec<_> = artifacts
            .iter()
            .map(|artifact| artifact.relative_path)
            .collect();
        assert!(names.contains(&"provider-catalog.json"));
        assert!(names.contains(&"provider-catalog.schema.json"));
        assert!(names.contains(&"harn-provider-catalog.ts"));
        assert!(names.contains(&"HarnProviderCatalog.swift"));
    }

    #[test]
    fn generated_catalog_validates_against_schema() {
        let schema = harn_vm::provider_catalog::schema_value();
        let artifact =
            serde_json::to_value(harn_vm::provider_catalog::artifact_embedded(None)).unwrap();
        let mut errors = Vec::new();
        validate_against_schema(&schema, &artifact, &mut errors).expect("schema compiles");
        assert!(errors.is_empty(), "schema errors: {errors:?}");
    }
}
