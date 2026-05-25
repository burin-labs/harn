use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;
use tokio::process::Command;

use crate::cli::{
    ProvidersExportArgs, ProvidersMatrixArgs, ProvidersRecommendArgs, ProvidersRefreshArgs,
    ProvidersValidateArgs,
};

pub(crate) async fn run_refresh(args: &ProvidersRefreshArgs) -> Result<(), String> {
    if !args.script.exists() {
        return Err(format!(
            "provider refresh script not found: {}",
            args.script.display()
        ));
    }
    let exe = std::env::current_exe()
        .map_err(|error| format!("failed to resolve current executable: {error}"))?;
    let mut command = Command::new(exe);
    command.arg("run").arg(&args.script).arg("--");
    if args.live {
        command.arg("--live");
    }
    if args.check || args.update {
        command.arg("--check");
    }
    if args.update {
        command.arg("--update");
    }
    let status = command
        .status()
        .await
        .map_err(|error| format!("failed to run provider refresh workflow: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "provider refresh workflow exited with {}",
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string())
        ))
    }
}

pub(crate) fn run_validate(args: &ProvidersValidateArgs) -> Result<(), String> {
    apply_overlay(args.overlay.as_deref())?;
    let artifact = harn_vm::provider_catalog::artifact();
    let logical = harn_vm::provider_catalog::validate_artifact(&artifact);
    let schema = harn_vm::provider_catalog::schema_value();
    let artifact_value = serde_json::to_value(&artifact)
        .map_err(|error| format!("failed to serialize provider catalog: {error}"))?;
    let mut schema_errors = Vec::new();
    validate_against_schema(&schema, &artifact_value, &mut schema_errors)?;
    let mut drift = Vec::new();
    if args.check_artifacts {
        drift = artifact_drift(&args.artifact_dir)?;
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
    apply_overlay(args.overlay.as_deref())?;
    let artifacts = generated_artifacts()?;
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

pub(crate) fn run_matrix(args: &ProvidersMatrixArgs) -> Result<(), String> {
    let rows = crate::commands::check::provider_matrix::filtered_rows(args.filter.as_deref());
    let catalog = crate::commands::check::provider_matrix::load_catalog_for_docs()?;
    let generated = crate::commands::check::provider_matrix::generate_markdown(&rows, &catalog);
    if args.check {
        match fs::read_to_string(&args.output) {
            Ok(existing) if existing == generated => {
                if !args.stdout {
                    println!("provider capability matrix is up to date");
                    return Ok(());
                }
            }
            Ok(_) | Err(_) => {
                return Err(format!(
                    "provider capability matrix is stale or missing: {}",
                    args.output.display()
                ));
            }
        }
    }
    if args.stdout {
        print!("{generated}");
        return Ok(());
    }
    if let Some(parent) = args
        .output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create provider matrix directory {}: {error}",
                parent.display()
            )
        })?;
    }
    fs::write(&args.output, generated)
        .map_err(|error| format!("failed to write {}: {error}", args.output.display()))?;
    println!("wrote {}", args.output.display());
    Ok(())
}

pub(crate) async fn run_recommend(args: &ProvidersRecommendArgs) -> Result<(), String> {
    if std::env::var("HARN_CLI_IMPL").as_deref() == Ok("rust") {
        return run_recommend_legacy(args);
    }
    let exit_code = run_recommend_dispatch(args).await?;
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

async fn run_recommend_dispatch(args: &ProvidersRecommendArgs) -> Result<i32, String> {
    let report = load_filtered_recommend_report(args)?;
    let payload_json = serde_json::to_string(&report)
        .map_err(|error| format!("failed to serialise recommend payload: {error}"))?;
    // Pretty companion so the script can forward bytes verbatim in
    // `--json` mode — Harn's JSON round-trip would otherwise normalise
    // integer-valued floats and lose serde fidelity. The legacy impl
    // emits `serde_json::to_string_pretty(&report)`.
    let payload_pretty = serde_json::to_string_pretty(&report)
        .map_err(|error| format!("failed to render recommend payload: {error}"))?;

    static DISPATCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = DISPATCH_LOCK.lock().await;
    let _payload_guard =
        crate::env_guard::ScopedEnvVar::set("HARN_PROVIDERS_RECOMMEND_PAYLOAD_JSON", &payload_json);
    let _pretty_guard = crate::env_guard::ScopedEnvVar::set(
        "HARN_PROVIDERS_RECOMMEND_PAYLOAD_PRETTY",
        &payload_pretty,
    );
    let outcome =
        crate::dispatch::run_embedded_script("providers/recommend", Vec::new(), args.json).await;
    if !outcome.stderr.is_empty() {
        use std::io::Write as _;
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if !outcome.stdout.is_empty() {
        use std::io::Write as _;
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
    Ok(outcome.exit_code)
}

fn load_filtered_recommend_report(
    args: &ProvidersRecommendArgs,
) -> Result<crate::commands::local_readiness::LocalReadinessReport, String> {
    let report = if let Some(summary) = args.summary.as_deref() {
        crate::commands::local_readiness::report_from_summary_path(summary)?
    } else if let Some(input) = args.input.as_deref() {
        crate::commands::local_readiness::load_report_or_summary(input)?
    } else {
        crate::commands::local_readiness::load_default_report()?
    };
    Ok(crate::commands::local_readiness::filter_report_by_provider(
        report,
        args.provider.as_deref(),
    ))
}

/// Legacy direct-render path. Kept verbatim for the parity-snapshot
/// harness (#2299) until C1 (#2314) deletes it.
fn run_recommend_legacy(args: &ProvidersRecommendArgs) -> Result<(), String> {
    let report = load_filtered_recommend_report(args)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("failed to render recommendation JSON: {error}"))?
        );
        return Ok(());
    }
    println!("local provider recommendations");
    println!("source: {}", report.source);
    if report.recommendations.is_empty() {
        println!("(no local model outcomes found)");
        return Ok(());
    }
    for recommendation in &report.recommendations {
        let tool = recommendation
            .recommended_tool_format
            .as_deref()
            .unwrap_or("unproven");
        println!(
            "{}. {} {} ({}, selector {}, tools {})",
            recommendation.rank,
            recommendation.provider,
            recommendation.model,
            recommendation.status,
            recommendation.selector,
            tool
        );
        for caveat in &recommendation.caveats {
            println!("   caveat: {caveat}");
        }
    }
    Ok(())
}

fn apply_overlay(path: Option<&Path>) -> Result<(), String> {
    let Some(path) = path else {
        return Ok(());
    };
    let src = fs::read_to_string(path)
        .map_err(|error| format!("failed to read overlay {}: {error}", path.display()))?;
    let overlay = harn_vm::llm_config::parse_config_toml(&src)
        .map_err(|error| format!("failed to parse overlay {}: {error}", path.display()))?;
    harn_vm::llm_config::set_user_overrides(Some(overlay));
    Ok(())
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

fn generated_artifacts() -> Result<Vec<GeneratedArtifact>, String> {
    Ok(vec![
        GeneratedArtifact {
            relative_path: "provider-catalog.json",
            body: harn_vm::provider_catalog::artifact_json()
                .map_err(|error| format!("failed to generate catalog JSON: {error}"))?,
        },
        GeneratedArtifact {
            relative_path: "provider-catalog.schema.json",
            body: harn_vm::provider_catalog::schema_json()
                .map_err(|error| format!("failed to generate catalog schema: {error}"))?,
        },
        GeneratedArtifact {
            relative_path: "harn-provider-catalog.ts",
            body: harn_vm::provider_catalog::typescript_binding()
                .map_err(|error| format!("failed to generate TypeScript binding: {error}"))?,
        },
        GeneratedArtifact {
            relative_path: "HarnProviderCatalog.swift",
            body: harn_vm::provider_catalog::swift_binding()
                .map_err(|error| format!("failed to generate Swift binding: {error}"))?,
        },
    ])
}

fn artifact_drift(dir: &Path) -> Result<Vec<String>, String> {
    artifact_drift_from(dir, &generated_artifacts()?)
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
        let artifacts = generated_artifacts().expect("artifacts generate");
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
        let artifact = serde_json::to_value(harn_vm::provider_catalog::artifact()).unwrap();
        let mut errors = Vec::new();
        validate_against_schema(&schema, &artifact, &mut errors).expect("schema compiles");
        assert!(errors.is_empty(), "schema errors: {errors:?}");
    }
}
