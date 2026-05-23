use std::time::Duration;

use super::errors::OrchestratorError;
use super::harness::{self, DrainConfig, OrchestratorConfig, OrchestratorHarness, PumpConfig};
use super::tls::TlsFiles;
use crate::cli::{OrchestratorLogFormat, OrchestratorServeArgs};

pub(crate) async fn run(args: OrchestratorServeArgs) -> Result<(), OrchestratorError> {
    // Install signal streams BEFORE any startup log a supervisor (test harness,
    // systemd, launchd) might be watching for. Tokio's signal streams install
    // the OS-level handler on their first call per SignalKind; any SIGTERM
    // delivered before that call uses the default disposition (terminate),
    // which caused orchestrator_serve_starts_and_shuts_down_cleanly to flake
    // under parallel test load when the harness raced past the "HTTP listener
    // ready" log.
    #[cfg(unix)]
    let signal_streams = install_signal_streams()?;

    let tls = TlsFiles::from_args(args.cert.clone(), args.key.clone())?;
    let config_path = harness::absolutize_from_cwd(&args.local.config)?;
    let (manifest, _manifest_dir) = harness::load_manifest(&config_path)?;
    let state_dir = harness::absolutize_from_cwd(&args.local.state_dir)?;

    let config = OrchestratorConfig {
        manifest_path: config_path,
        state_dir,
        bind: args.bind,
        role: args.role,
        watch_manifest: args.watch,
        mcp: args.mcp,
        mcp_path: args.mcp_path,
        mcp_sse_path: args.mcp_sse_path,
        mcp_messages_path: args.mcp_messages_path,
        tls,
        shutdown_timeout: Duration::from_secs(args.shutdown_timeout.max(1)),
        drain: DrainConfig {
            max_items: args
                .drain_max_items
                .unwrap_or(manifest.orchestrator.drain.max_items),
            deadline: Duration::from_secs(
                args.drain_deadline
                    .unwrap_or(manifest.orchestrator.drain.deadline_seconds),
            ),
        },
        pump: PumpConfig {
            max_outstanding: args
                .pump_max_outstanding
                .unwrap_or(manifest.orchestrator.pumps.max_outstanding),
        },
        log_format: Some(log_format(args.log_format)),
        clock: harn_vm::clock::RealClock::arc(),
        public_metrics: args.public_metrics,
    };

    let harness = OrchestratorHarness::start(config)
        .await
        .map_err(|error| OrchestratorError::Serve(error.0))?;

    wait_for_shutdown(
        harness,
        #[cfg(unix)]
        signal_streams,
    )
    .await
}

fn log_format(format: OrchestratorLogFormat) -> harn_vm::observability::otel::LogFormat {
    match format {
        OrchestratorLogFormat::Text => harn_vm::observability::otel::LogFormat::Text,
        OrchestratorLogFormat::Pretty => harn_vm::observability::otel::LogFormat::Pretty,
        OrchestratorLogFormat::Json => harn_vm::observability::otel::LogFormat::Json,
    }
}

#[cfg(unix)]
struct SignalStreams {
    sigterm: tokio::signal::unix::Signal,
    sigint: tokio::signal::unix::Signal,
    sighup: tokio::signal::unix::Signal,
}

#[cfg(unix)]
fn install_signal_streams() -> Result<SignalStreams, OrchestratorError> {
    use tokio::signal::unix::{signal, SignalKind};
    Ok(SignalStreams {
        sigterm: signal(SignalKind::terminate())
            .map_err(|error| format!("failed to register SIGTERM handler: {error}"))?,
        sigint: signal(SignalKind::interrupt())
            .map_err(|error| format!("failed to register SIGINT handler: {error}"))?,
        sighup: signal(SignalKind::hangup())
            .map_err(|error| format!("failed to register SIGHUP handler: {error}"))?,
    })
}

async fn wait_for_shutdown(
    harness: OrchestratorHarness,
    #[cfg(unix)] mut signals: SignalStreams,
) -> Result<(), OrchestratorError> {
    #[cfg(unix)]
    {
        let SignalStreams {
            sigterm,
            sigint,
            sighup,
        } = &mut signals;
        let reload = harness.admin_reload();
        loop {
            tokio::select! {
                _ = sigterm.recv() => break,
                _ = sigint.recv() => break,
                _ = sighup.recv() => { let _ = reload.trigger("sighup"); }
            }
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.map_err(|error| {
            OrchestratorError::Serve(format!("failed to wait for Ctrl-C: {error}"))
        })?;
    }

    eprintln!("[harn] signal received, starting graceful shutdown...");
    harness
        .shutdown(Duration::from_secs(30))
        .await
        .map_err(|error| OrchestratorError::Serve(error.0))?;
    Ok(())
}
