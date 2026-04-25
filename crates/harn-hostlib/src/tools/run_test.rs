//! `tools/run_test` — run a project-defined test command.
//!
//! Schema: `schemas/tools/run_test.{request,response}.json`.
//!
//! Behavior:
//! - If `argv` is supplied, run it verbatim.
//! - Otherwise detect the workspace ecosystem from `cwd` and pick a sensible
//!   default test command (cargo test, pytest, vitest, go test, swift test,
//!   …). When the runner supports it, append a JUnit-style reporter so we
//!   can produce a `result_handle` for `inspect_test_results`.
//! - When `filter` is supplied, append it through the runner's
//!   pattern-filter flag (`-k`, `--filter`, `-run`, …).
//!
//! Divergence vs. Swift `runTest`:
//! - We never invoke a Makefile or rely on `cachedBuildCommands` —
//!   that's host-side state we don't have. Callers that drive a Makefile
//!   should pass `argv: ["make", "test"]` explicitly.
//! - JUnit XML / cargo libtest text is captured into the
//!   `inspect_test_results` cache rather than parsed inline; that keeps
//!   the response payload small and lets the caller drill in only when
//!   they need to.

use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::inspect_test_results::{store_run, RawArtifacts, TestSummaryData};
use crate::tools::lang::{detect, Ecosystem};
use crate::tools::payload::{
    optional_string, optional_string_list, optional_timeout, parse_argv_program, require_dict_arg,
};
use crate::tools::proc::{self, SpawnRequest};
use crate::tools::response::ResponseBuilder;

pub(crate) const NAME: &str = "hostlib_tools_run_test";

pub(crate) fn handle(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let map = require_dict_arg(NAME, args)?;
    let cwd_raw = optional_string(NAME, &map, "cwd")?;
    let cwd_path = proc::parse_cwd(NAME, cwd_raw.as_deref())?;
    let cwd_for_detect = cwd_path
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let filter = optional_string(NAME, &map, "filter")?;
    let timeout = optional_timeout(NAME, &map, "timeout_ms")?;

    let plan = if let Some(argv) = optional_string_list(NAME, &map, "argv")? {
        TestPlan::Explicit(argv)
    } else {
        let ecosystem = detect(&cwd_for_detect).ok_or(HostlibError::InvalidParameter {
            builtin: NAME,
            param: "argv",
            message: "no recognized project manifest in cwd; pass argv explicitly".to_string(),
        })?;
        TestPlan::Detected(ecosystem)
    };

    let (argv, junit_tmp) = plan.build_argv(filter.as_deref(), &cwd_for_detect)?;
    let (program, args_tail) = parse_argv_program(NAME, argv.clone())?;

    let outcome = proc::run(SpawnRequest {
        builtin: NAME,
        program,
        args: args_tail,
        cwd: cwd_path,
        env: BTreeMap::new(),
        stdin: None,
        timeout,
    })?;

    let artifacts = RawArtifacts {
        stdout: outcome.stdout.clone(),
        stderr: outcome.stderr.clone(),
        exit_code: outcome.exit_code,
        junit_path: junit_tmp.clone(),
        ecosystem: plan.ecosystem_name(),
        argv,
    };
    let summary = artifacts.compute_summary();
    let handle = store_run(artifacts);

    let mut builder = ResponseBuilder::new()
        .int("exit_code", outcome.exit_code as i64)
        .str("stdout", outcome.stdout)
        .str("stderr", outcome.stderr)
        .int("duration_ms", outcome.duration.as_millis() as i64)
        .str("result_handle", handle);

    if let Some(summary) = summary {
        builder = builder.dict("summary", summary_to_dict(summary));
    }
    Ok(builder.build())
}

fn summary_to_dict(summary: TestSummaryData) -> BTreeMap<String, VmValue> {
    let mut map = BTreeMap::new();
    map.insert("passed".to_string(), VmValue::Int(summary.passed as i64));
    map.insert("failed".to_string(), VmValue::Int(summary.failed as i64));
    map.insert("skipped".to_string(), VmValue::Int(summary.skipped as i64));
    map
}

enum TestPlan {
    Explicit(Vec<String>),
    Detected(Ecosystem),
}

impl TestPlan {
    fn ecosystem_name(&self) -> Option<String> {
        match self {
            TestPlan::Explicit(_) => None,
            TestPlan::Detected(eco) => Some(eco.name().to_string()),
        }
    }

    /// Returns `(argv, junit_tmp_file)`. `junit_tmp_file` is `Some` for the
    /// runners we know how to point at a per-run JUnit output path.
    fn build_argv(
        &self,
        filter: Option<&str>,
        cwd: &Path,
    ) -> Result<(Vec<String>, Option<PathBuf>), HostlibError> {
        match self {
            TestPlan::Explicit(argv) => Ok((argv.clone(), None)),
            TestPlan::Detected(eco) => {
                let mut argv = base_test_argv(*eco);
                let mut junit_path = None;
                match eco {
                    Ecosystem::Pip | Ecosystem::Uv | Ecosystem::Poetry => {
                        let path = junit_temp_path(cwd, "pytest");
                        argv.push(format!("--junitxml={}", path.display()));
                        junit_path = Some(path);
                        if let Some(f) = filter {
                            argv.push("-k".into());
                            argv.push(f.into());
                        }
                    }
                    Ecosystem::Pnpm | Ecosystem::Yarn | Ecosystem::Npm => {
                        // Vitest is the most common JS runner that respects
                        // these flags. Plain `npm test` hooks scripts —
                        // we forward filter as a positional and let the
                        // script consume it.
                        let path = junit_temp_path(cwd, "vitest");
                        argv.push("--reporter=junit".into());
                        argv.push(format!("--outputFile={}", path.display()));
                        junit_path = Some(path);
                        if let Some(f) = filter {
                            argv.push("-t".into());
                            argv.push(f.into());
                        }
                    }
                    Ecosystem::Cargo => {
                        if let Some(f) = filter {
                            argv.push(f.into());
                        }
                    }
                    Ecosystem::Go => {
                        if let Some(f) = filter {
                            argv.push("-run".into());
                            argv.push(f.into());
                        }
                    }
                    Ecosystem::Swift => {
                        if let Some(f) = filter {
                            argv.push("--filter".into());
                            argv.push(f.into());
                        }
                    }
                    _ => {
                        if let Some(f) = filter {
                            argv.push(f.into());
                        }
                    }
                }
                Ok((argv, junit_path))
            }
        }
    }
}

fn base_test_argv(eco: Ecosystem) -> Vec<String> {
    match eco {
        Ecosystem::Cargo => vec!["cargo".into(), "test".into()],
        Ecosystem::Npm => vec!["npm".into(), "test".into()],
        Ecosystem::Pnpm => vec!["pnpm".into(), "test".into()],
        Ecosystem::Yarn => vec!["yarn".into(), "test".into()],
        Ecosystem::Pip => vec!["pytest".into()],
        Ecosystem::Uv => vec!["uv".into(), "run".into(), "pytest".into()],
        Ecosystem::Poetry => vec!["poetry".into(), "run".into(), "pytest".into()],
        Ecosystem::Go => vec!["go".into(), "test".into(), "./...".into()],
        Ecosystem::Swift => vec!["swift".into(), "test".into()],
        Ecosystem::Gradle => vec!["./gradlew".into(), "test".into()],
        Ecosystem::Maven => vec!["mvn".into(), "test".into()],
        Ecosystem::Bundler => vec!["bundle".into(), "exec".into(), "rake".into(), "test".into()],
        Ecosystem::Composer => vec!["./vendor/bin/phpunit".into()],
        Ecosystem::Dotnet => vec!["dotnet".into(), "test".into()],
    }
}

fn junit_temp_path(cwd: &Path, prefix: &str) -> PathBuf {
    let id: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id();
    let target_dir = cwd.join(".harn").join("hostlib-tests");
    let _ = std::fs::create_dir_all(&target_dir);
    target_dir.join(format!("{prefix}-{pid}-{id}.xml"))
}
