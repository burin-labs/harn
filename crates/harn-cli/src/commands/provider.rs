//! `harn provider probe` — one-shot machine-readable provider snapshot.
//!
//! Combines provider readiness with local runtime-state details in a
//! structured envelope eval pipelines decode alongside per-call telemetry.
//!
//! ## .harn dispatch
//!
//! Aggregation stays in Rust (sandboxed scripts can't run the
//! readiness HTTP probe + `/api/ps` calls), the rendering layer lives
//! in `crates/harn-stdlib/src/stdlib/cli/providers/{probe,tool_probe}.harn`.

use std::process;

use harn_vm::llm::readiness::{probe_provider_readiness, ProviderReadiness};
use harn_vm::llm_config;
use serde::Serialize;

use crate::cli::{
    ProviderCacheProbeArgs, ProviderProbeArgs, ProviderToolProbeArgs, ProviderToolProbeAuditArgs,
    ProviderToolScorecardArgs,
};
use crate::commands::local::runtime::{fetch_ollama_ps, LoadedModel, LOCAL_PROVIDERS};
use crate::commands::provider_report::{dispatch_provider_report, ProviderReportDispatch};

/// Env var carrying the JSON `ProviderProbe` envelope handed across to
/// the embedded `cli/providers/probe` script.
const PROBE_PAYLOAD_ENV: &str = "HARN_PROVIDER_PROBE_PAYLOAD_JSON";

/// Pretty-printed companion to [`PROBE_PAYLOAD_ENV`]. The script
/// forwards these bytes directly in `--json` mode so Harn's
/// `json_parse`/`json_stringify` round-trip can't normalise float
/// fields like pricing entries that serde keeps typed.
const PROBE_PAYLOAD_PRETTY_ENV: &str = "HARN_PROVIDER_PROBE_PAYLOAD_PRETTY";

/// Env var carrying the JSON `ToolConformanceReport` envelope handed
/// across to the embedded `cli/providers/tool_probe` script.
const TOOL_PROBE_PAYLOAD_ENV: &str = "HARN_PROVIDER_TOOL_PROBE_PAYLOAD_JSON";

/// Pretty-printed companion to [`TOOL_PROBE_PAYLOAD_ENV`] — same
/// rationale as [`PROBE_PAYLOAD_PRETTY_ENV`].
const TOOL_PROBE_PAYLOAD_PRETTY_ENV: &str = "HARN_PROVIDER_TOOL_PROBE_PAYLOAD_PRETTY";

/// Env var carrying the JSON `ToolScorecardReport` envelope handed across to
/// the embedded `cli/providers/tool_scorecard` render script.
const TOOL_SCORECARD_PAYLOAD_ENV: &str = "HARN_PROVIDER_TOOL_SCORECARD_PAYLOAD_JSON";

/// Pretty-printed companion to [`TOOL_SCORECARD_PAYLOAD_ENV`].
const TOOL_SCORECARD_PAYLOAD_PRETTY_ENV: &str = "HARN_PROVIDER_TOOL_SCORECARD_PAYLOAD_PRETTY";

/// Env var carrying the JSON `CacheConformanceReport` envelope handed across
/// to the embedded `cli/providers/cache_probe` render script.
const CACHE_PROBE_PAYLOAD_ENV: &str = "HARN_PROVIDER_CACHE_PROBE_PAYLOAD_JSON";

/// Pretty-printed companion to [`CACHE_PROBE_PAYLOAD_ENV`] — same rationale as
/// [`PROBE_PAYLOAD_PRETTY_ENV`].
const CACHE_PROBE_PAYLOAD_PRETTY_ENV: &str = "HARN_PROVIDER_CACHE_PROBE_PAYLOAD_PRETTY";

#[derive(Debug, Serialize)]
struct ProviderProbe {
    provider: String,
    base_url: Option<String>,
    readiness: ProviderReadiness,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_profile: Option<harn_vm::llm::local_profiles::LocalRuntimeProfileReport>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    loaded_models: Vec<LoadedModel>,
}

const PROBE_REPORT_DISPATCH: ProviderReportDispatch = ProviderReportDispatch {
    script_name: "providers/probe",
    payload_name: "provider probe",
    payload_env: PROBE_PAYLOAD_ENV,
    pretty_env: PROBE_PAYLOAD_PRETTY_ENV,
};

const TOOL_PROBE_REPORT_DISPATCH: ProviderReportDispatch = ProviderReportDispatch {
    script_name: "providers/tool_probe",
    payload_name: "tool-probe",
    payload_env: TOOL_PROBE_PAYLOAD_ENV,
    pretty_env: TOOL_PROBE_PAYLOAD_PRETTY_ENV,
};

const TOOL_SCORECARD_REPORT_DISPATCH: ProviderReportDispatch = ProviderReportDispatch {
    script_name: "providers/tool_scorecard",
    payload_name: "tool-scorecard",
    payload_env: TOOL_SCORECARD_PAYLOAD_ENV,
    pretty_env: TOOL_SCORECARD_PAYLOAD_PRETTY_ENV,
};

const CACHE_PROBE_REPORT_DISPATCH: ProviderReportDispatch = ProviderReportDispatch {
    script_name: "providers/cache_probe",
    payload_name: "cache-probe",
    payload_env: CACHE_PROBE_PAYLOAD_ENV,
    pretty_env: CACHE_PROBE_PAYLOAD_PRETTY_ENV,
};

pub(crate) async fn run_provider_probe(args: ProviderProbeArgs) {
    let exit_code = dispatch_provider_probe(args).await;
    if exit_code != 0 {
        process::exit(exit_code);
    }
}

async fn dispatch_provider_probe(args: ProviderProbeArgs) -> i32 {
    let probe = aggregate_provider_probe(&args).await;
    let exit_code = i32::from(!probe.readiness.ok);
    let render_exit = dispatch_provider_report(PROBE_REPORT_DISPATCH, args.json, &probe).await;
    // The script's own exit code reflects readiness (1 on failure).
    // If the script itself errored at the 70 level (internal), surface
    // that to the user instead.
    if render_exit != 0 {
        render_exit
    } else {
        exit_code
    }
}

async fn aggregate_provider_probe(args: &ProviderProbeArgs) -> ProviderProbe {
    let readiness = probe_provider_readiness(
        &args.provider,
        args.model.as_deref(),
        args.base_url.as_deref(),
    )
    .await;

    let base_url = readiness.base_url.clone().or_else(|| {
        llm_config::provider_config(&args.provider).map(|def| llm_config::resolve_base_url(&def))
    });

    let loaded_models = if args.provider == "ollama" {
        let base = base_url
            .clone()
            .unwrap_or_else(|| "http://localhost:11434".to_string());
        match fetch_ollama_ps(&base).await {
            Ok(entries) => entries,
            Err(error) => {
                // `/api/ps` is best-effort: a daemon that doesn't expose it
                // shouldn't block the readiness signal. Warn so eval logs
                // surface the gap without failing the probe.
                eprintln!("warning: /api/ps unavailable: {error}");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    ProviderProbe {
        provider: args.provider.clone(),
        base_url,
        readiness,
        runtime_profile: if LOCAL_PROVIDERS.contains(&args.provider.as_str()) {
            args.model.as_deref().map(|model| {
                harn_vm::llm::local_profiles::local_runtime_profile_report(
                    model,
                    Some(&args.provider),
                )
            })
        } else {
            None
        },
        loaded_models,
    }
}

pub(crate) async fn run_provider_tool_probe(args: ProviderToolProbeArgs) {
    let exit_code = dispatch_provider_tool_probe(args).await;
    if exit_code != 0 {
        process::exit(exit_code);
    }
}

pub(crate) fn run_provider_tool_probe_audit(args: ProviderToolProbeAuditArgs) {
    let report = harn_vm::llm::tool_conformance::tool_conformance_request_catalog_audit(
        args.probe_cases
            .into_iter()
            .map(|probe_case| probe_case.tool_probe_case())
            .collect(),
        args.mode.tool_probe_modes(),
    );
    if args.json {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("internal error: failed to render tool-probe audit report: {error}");
                process::exit(1);
            }
        }
    } else {
        println!(
            "provider tool-probe audit: {}/{} request validations passed across {} catalog routes",
            report.validation_pass_count, report.request_count, report.route_count
        );
        if report.validation_fail_count > 0 {
            for failure in report.failures.iter().take(10) {
                println!(
                    "- {}:{} {} {}: {}",
                    failure.provider,
                    failure.model,
                    failure.probe_case,
                    failure.mode,
                    failure.issues.join("; ")
                );
            }
        }
    }
    if report.validation_fail_count > 0 {
        process::exit(1);
    }
}

async fn dispatch_provider_tool_probe(args: ProviderToolProbeArgs) -> i32 {
    if args.dry_run_request {
        return dispatch_provider_tool_probe_request_dry_run(&args);
    }
    let report = match aggregate_tool_conformance_report(&args).await {
        Ok(report) => report,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    let fallback_disabled = report.tool_calling.fallback_mode
        == harn_vm::llm::tool_conformance::ToolProbeFallbackMode::Disabled;
    let render_exit =
        dispatch_provider_report(TOOL_PROBE_REPORT_DISPATCH, args.json, &report).await;
    if render_exit != 0 {
        return render_exit;
    }
    i32::from(fallback_disabled)
}

async fn aggregate_tool_conformance_report(
    args: &ProviderToolProbeArgs,
) -> Result<harn_vm::llm::tool_conformance::ToolConformanceReport, String> {
    if let Some(path) = args.response_fixture.as_ref() {
        if args.repeat != 1 {
            return Err(
                "error: --repeat is only supported for live provider tool probes; \
                 --response-fixture classifies one saved response"
                    .to_string(),
            );
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|error| format!("error: failed to read {}: {error}", path.display()))?;
        Ok(
            harn_vm::llm::tool_conformance::classify_tool_conformance_fixture_for_case(
                args.provider.clone(),
                args.model.clone(),
                args.mode
                    .tool_probe_modes()
                    .into_iter()
                    .next()
                    .unwrap_or(harn_vm::llm::tool_conformance::ToolProbeMode::NonStreaming),
                args.probe_case.tool_probe_case(),
                args.marker.clone(),
                &raw,
            ),
        )
    } else {
        Ok(harn_vm::llm::tool_conformance::run_tool_conformance_probe(
            args.tool_conformance_probe_options(),
        )
        .await)
    }
}

fn dispatch_provider_tool_probe_request_dry_run(args: &ProviderToolProbeArgs) -> i32 {
    match harn_vm::llm::tool_conformance::tool_conformance_request_report_json(
        args.provider.clone(),
        args.model.clone(),
        args.base_url.clone(),
        args.mode.tool_probe_modes(),
        args.probe_case.tool_probe_case(),
        args.marker.clone(),
    ) {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

pub(crate) async fn run_provider_tool_scorecard(args: ProviderToolScorecardArgs) {
    let exit_code = dispatch_provider_tool_scorecard(args).await;
    if exit_code != 0 {
        process::exit(exit_code);
    }
}

async fn dispatch_provider_tool_scorecard(args: ProviderToolScorecardArgs) -> i32 {
    let render = TOOL_SCORECARD_REPORT_DISPATCH;
    if args.plan_from_catalog {
        let plan = match harn_vm::llm::tool_scorecard::tool_scorecard_plan_from_catalog(
            &args.routes,
            args.include_batch_manifest,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                eprintln!("{error}");
                return 1;
            }
        };
        return render.render(args.json, args.markdown, &plan).await;
    }

    let report = match aggregate_tool_scorecard(&args) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    render.render(args.json, args.markdown, &report).await
}

fn aggregate_tool_scorecard(
    args: &ProviderToolScorecardArgs,
) -> Result<harn_vm::llm::tool_scorecard::ToolScorecardReport, String> {
    let mut reports = Vec::new();
    for path in &args.tool_probe_reports {
        let raw = std::fs::read_to_string(path)
            .map_err(|error| format!("error: failed to read {}: {error}", path.display()))?;
        let report =
            serde_json::from_str::<harn_vm::llm::tool_conformance::ToolConformanceReport>(&raw)
                .map_err(|error| {
                    format!(
                        "error: failed to parse tool-probe report {}: {error}",
                        path.display()
                    )
                })?;
        if report.schema_version != harn_vm::llm::tool_conformance::TOOL_CONFORMANCE_SCHEMA_VERSION
        {
            return Err(format!(
                "error: unsupported tool-probe report schema_version {} in {}; expected {}",
                report.schema_version,
                path.display(),
                harn_vm::llm::tool_conformance::TOOL_CONFORMANCE_SCHEMA_VERSION
            ));
        }
        reports.push(report);
    }
    Ok(harn_vm::llm::tool_scorecard::scorecard_from_tool_reports(
        reports,
    ))
}

pub(crate) async fn run_provider_cache_probe(args: ProviderCacheProbeArgs) {
    let exit_code = dispatch_provider_cache_probe(args).await;
    if exit_code != 0 {
        process::exit(exit_code);
    }
}

async fn dispatch_provider_cache_probe(args: ProviderCacheProbeArgs) -> i32 {
    let Some(fixture_path) = args.usage_fixture.as_ref() else {
        eprintln!(
            "error: live prompt-cache probing is not yet wired; pass --usage-fixture <path> \
             with a saved repeat-run usage fixture"
        );
        return 2;
    };
    let raw = match std::fs::read_to_string(fixture_path) {
        Ok(raw) => raw,
        Err(error) => {
            eprintln!("error: failed to read {}: {error}", fixture_path.display());
            return 1;
        }
    };
    let report = match harn_vm::llm::cache_conformance::classify_cache_conformance_fixture(
        args.provider.clone(),
        args.model.clone(),
        &raw,
    ) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    let dogfood_failure = report.dogfood_failure;
    let render_exit =
        dispatch_provider_report(CACHE_PROBE_REPORT_DISPATCH, args.json, &report).await;
    if render_exit != 0 {
        return render_exit;
    }
    // A supported route that never caches, or contradictory provider fields, is
    // a real conformance failure; a non-cache provider is not.
    i32::from(dogfood_failure)
}
