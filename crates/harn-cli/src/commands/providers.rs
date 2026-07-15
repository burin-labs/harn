use std::fs;

use tokio::process::Command;

use crate::cli::{ProvidersMatrixArgs, ProvidersRecommendArgs, ProvidersRefreshArgs};

mod artifacts;
mod build;
mod tool_probe_audit;
mod tool_probe_request;

pub(crate) use artifacts::{run_export, run_validate};
pub(crate) use build::run_generate;
pub(crate) use tool_probe_audit::run as run_audit;
pub(crate) use tool_probe_request::render as render_tool_probe_request;

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
    // integer-valued floats and lose serde fidelity.
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
