use std::ffi::OsString;
use std::fs;

use tokio::process::Command;

use crate::cli::{
    ProviderCatalogCommand, ProvidersMatrixArgs, ProvidersRecommendArgs, ProvidersRefreshArgs,
};

mod artifacts;
mod build;
mod overlay_audit;
mod tool_probe_audit;
mod tool_probe_request;

pub(crate) use artifacts::{run_export, run_validate};
pub(crate) use build::run_generate;
pub(crate) use overlay_audit::run_overlay_audit;
pub(crate) use tool_probe_audit::run as run_audit;
pub(crate) use tool_probe_request::{
    render as render_tool_probe_request, resolve_probe_wire_model,
};

/// Route one `harn provider catalog <sub>` invocation to its command.
///
/// Lives beside the commands rather than in the top-level `Command` match so
/// adding a catalog subcommand touches this file and the arg definitions, not
/// the crate root.
pub(crate) async fn dispatch_catalog(command: ProviderCatalogCommand) {
    let outcome = match &command {
        ProviderCatalogCommand::Refresh(refresh) => run_refresh(refresh).await,
        ProviderCatalogCommand::Validate(validate) => run_validate(validate),
        ProviderCatalogCommand::Generate(generate) => run_generate(generate),
        ProviderCatalogCommand::Export(export) => run_export(export),
        ProviderCatalogCommand::OverlayAudit(audit) => run_overlay_audit(audit),
        ProviderCatalogCommand::Matrix(matrix) => run_matrix(matrix),
        ProviderCatalogCommand::Support(support) => crate::commands::provider_support::run(support),
        ProviderCatalogCommand::Recommend(recommend) => run_recommend(recommend).await,
        // `show` owns its own exit path: it refreshes first, then reports the
        // dispatcher's exit code rather than a Result.
        ProviderCatalogCommand::Show(show) => {
            crate::cli::refresh_provider_catalog_if_requested(show).await;
            let exit_code = crate::dispatch_provider_catalog(show.available_only).await;
            crate::runtime::exit_on_error(exit_code);
            Ok(())
        }
    };
    if let Err(error) = outcome {
        crate::command_error(&error);
    }
}

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
    command.args(refresh_run_args(args));
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to run provider refresh workflow: {error}"))?;
    let status = match refresh_timeout(args) {
        None => child
            .wait()
            .await
            .map_err(|error| format!("failed to run provider refresh workflow: {error}"))?,
        Some(limit) => match tokio::time::timeout(limit, child.wait()).await {
            Ok(status) => status
                .map_err(|error| format!("failed to run provider refresh workflow: {error}"))?,
            Err(_) => {
                // Reap the child before reporting. A refresh that is still
                // holding the network open is exactly what the caller was
                // waiting on, and leaving it behind would make the next run
                // contend with it.
                let _ = child.kill().await;
                return Err(refresh_timeout_message(args, limit));
            }
        },
    };
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

/// Default bound on the refresh workflow.
///
/// Generous enough for a live pass over every provider source on a slow link,
/// and short enough that a gate blocked on an unreachable endpoint fails while
/// somebody is still watching it.
pub(crate) const DEFAULT_REFRESH_TIMEOUT_SECS: u64 = 120;

/// The bound to apply to one refresh, or `None` when the caller asked to wait
/// forever.
pub(crate) fn refresh_timeout(args: &ProvidersRefreshArgs) -> Option<std::time::Duration> {
    if args.timeout_secs == 0 {
        return None;
    }
    Some(std::time::Duration::from_secs(args.timeout_secs))
}

/// What a caller is told when the refresh runs out of time.
///
/// A timed-out refresh and a refresh that found nothing are different
/// outcomes, so this never says the catalog was empty. It names the bound, the
/// script, and which source class the run was reading, because "it hung" with
/// no endpoint class named is what made the original five-minute stall
/// undiagnosable.
pub(crate) fn refresh_timeout_message(
    args: &ProvidersRefreshArgs,
    limit: std::time::Duration,
) -> String {
    let sources = if args.live {
        "live provider and model endpoints"
    } else {
        "bundled offline fixtures"
    };
    format!(
        "provider refresh workflow timed out after {}s waiting on {} via {}. \
         This is a timeout, not an empty catalog: nothing was written and the \
         committed catalog is unchanged. Raise --timeout-secs, pass \
         --timeout-secs 0 to wait indefinitely, or drop --live to refresh from \
         the committed fixtures without a network call.",
        limit.as_secs(),
        sources,
        args.script.display(),
    )
}

fn refresh_run_args(args: &ProvidersRefreshArgs) -> Vec<OsString> {
    let mut command = vec![
        OsString::from("run"),
        OsString::from("--allow-process-network"),
        args.script.as_os_str().to_owned(),
        OsString::from("--"),
    ];
    if args.live {
        command.push(OsString::from("--live"));
    }
    if args.check || args.update {
        command.push(OsString::from("--check"));
    }
    if args.update {
        command.push(OsString::from("--update"));
    }
    command
}

pub(crate) fn run_matrix(args: &ProvidersMatrixArgs) -> Result<(), String> {
    let rows = crate::commands::check::provider_matrix::filtered_rows(args.filter.as_deref());
    let catalog = crate::commands::check::provider_matrix::load_catalog_for_docs(&args.empirical)?;
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

#[cfg(test)]
mod tests {
    use super::{refresh_timeout, refresh_timeout_message, DEFAULT_REFRESH_TIMEOUT_SECS};
    use crate::cli::ProvidersRefreshArgs;
    use std::path::PathBuf;
    use std::time::Duration;

    fn args(live: bool, timeout_secs: u64) -> ProvidersRefreshArgs {
        ProvidersRefreshArgs {
            live,
            check: false,
            update: false,
            script: PathBuf::from("scripts/update_provider_catalog.harn"),
            timeout_secs,
        }
    }

    #[test]
    fn the_refresh_is_bounded_by_default() {
        // The defect was an unbounded wait, so the assertion that binds is
        // that the default produces a bound at all.
        assert_eq!(
            refresh_timeout(&args(true, DEFAULT_REFRESH_TIMEOUT_SECS)),
            Some(Duration::from_secs(DEFAULT_REFRESH_TIMEOUT_SECS)),
        );
    }

    #[test]
    fn zero_is_the_explicit_opt_out_and_not_an_instant_timeout() {
        // Reading zero as a zero-length bound would turn the opt-out into a
        // refresh that always times out immediately.
        assert_eq!(refresh_timeout(&args(true, 0)), None);
    }

    #[test]
    fn a_timeout_names_the_bound_and_the_source_class() {
        let message = refresh_timeout_message(&args(true, 30), Duration::from_secs(30));
        assert!(message.contains("timed out after 30s"), "{message}");
        assert!(
            message.contains("live provider and model endpoints"),
            "a timeout must name what it was waiting on: {message}",
        );
        assert!(
            message.contains("update_provider_catalog.harn"),
            "{message}"
        );
    }

    #[test]
    fn a_timeout_is_never_reported_as_an_empty_catalog() {
        // The third ask on the issue. A caller that greps for "empty" or reads
        // "no models" would otherwise treat an unreachable endpoint as a real
        // and answerable result.
        let message = refresh_timeout_message(&args(true, 30), Duration::from_secs(30));
        assert!(message.contains("not an empty catalog"), "{message}");
        assert!(!message.to_lowercase().contains("no models"), "{message}");
        assert!(
            message.contains("committed catalog is unchanged"),
            "{message}"
        );
    }

    #[test]
    fn an_offline_refresh_says_so_rather_than_blaming_the_network() {
        // Without --live the workflow reads bundled fixtures, so a timeout
        // there is not a reachability problem and must not be reported as one.
        let message = refresh_timeout_message(&args(false, 5), Duration::from_secs(5));
        assert!(message.contains("bundled offline fixtures"), "{message}");
        assert!(
            !message.contains("live provider and model endpoints"),
            "{message}",
        );
    }
}
