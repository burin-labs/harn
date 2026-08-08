//! Runnable loadgen scenario for the Postgres hostlib.
//!
//! Reads `HARN_PG_LOADGEN_*` overrides plus the gating connection URL from
//! the environment (see [`harn_postgres_perf`]). Without the URL it prints
//! a skip notice and exits 0, so the nightly E2E job is a clean no-op until
//! a Postgres instance is provisioned.

use std::process::ExitCode;

use harn_postgres_perf::LoadgenConfig;

fn main() -> ExitCode {
    let config = LoadgenConfig::from_env();

    if !config.url_available() {
        eprintln!(
            "harn-postgres-loadgen: {} not set — skipping (no Postgres to drive)",
            config.url_env
        );
        return ExitCode::SUCCESS;
    }

    let report = match config.run() {
        Ok(report) => report,
        Err(error) => {
            eprintln!("harn-postgres-loadgen: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("{report}");

    match report.meets(&config) {
        Ok(()) => {
            println!(
                "PASS: sustained ≥{:.0} req/s at p99 ≤ {:.0} ms",
                config.target_rps,
                config.p99_threshold.as_secs_f64() * 1_000.0
            );
            ExitCode::SUCCESS
        }
        Err(reason) => {
            eprintln!("{}: {reason}", if config.enforce { "FAIL" } else { "WARN" });
            if config.enforce {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
    }
}
