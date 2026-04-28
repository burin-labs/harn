//! `tools/run_build_command` — invoke the project build, then parse
//! diagnostics back out of the runner's machine-readable output.
//!
//! Schema: `schemas/tools/run_build_command.{request,response}.json`.
//!
//! Behavior:
//! - If `argv` is supplied, run it verbatim. We *still* try to parse
//!   diagnostics from the output afterwards (cargo / tsc / eslint / go),
//!   so explicit-argv callers benefit from the structured surface too.
//! - Otherwise detect the workspace ecosystem and pick a default. For
//!   `cargo`, swap in `--message-format=json-diagnostic-rendered-ansi`
//!   so we can emit per-error diagnostics.
//! - `release: true` and `target` are forwarded as flags where supported
//!   (cargo, swift, dotnet).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::diagnostics::{parse_diagnostics, DiagnosticSource};
use crate::tools::lang::{detect, Ecosystem};
use crate::tools::payload::{
    optional_bool, optional_string, optional_string_list, optional_timeout, parse_argv_program,
    require_dict_arg,
};
use crate::tools::proc::{self, CaptureConfig, EnvMode, SpawnRequest};
use crate::tools::response::ResponseBuilder;

pub(crate) const NAME: &str = "hostlib_tools_run_build_command";

pub(crate) fn handle(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let map = require_dict_arg(NAME, args)?;
    let cwd_raw = optional_string(NAME, &map, "cwd")?;
    let cwd_path = proc::parse_cwd(NAME, cwd_raw.as_deref())?;
    let cwd_for_detect = cwd_path
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let target = optional_string(NAME, &map, "target")?;
    let release = optional_bool(NAME, &map, "release")?.unwrap_or(false);
    let timeout = optional_timeout(NAME, &map, "timeout_ms")?;
    let long_running = optional_bool(NAME, &map, "long_running")?.unwrap_or(false);

    let (argv, source) = if let Some(argv) = optional_string_list(NAME, &map, "argv")? {
        let source = infer_diagnostic_source(&argv);
        (argv, source)
    } else {
        let ecosystem = detect(&cwd_for_detect).ok_or(HostlibError::InvalidParameter {
            builtin: NAME,
            param: "argv",
            message: "no recognized project manifest in cwd; pass argv explicitly".to_string(),
        })?;
        build_default_argv(ecosystem, target.as_deref(), release)
    };

    let (program, args_tail) = parse_argv_program(NAME, argv)?;

    if long_running {
        let session_id = harn_vm::current_agent_session_id().unwrap_or_default();
        let info = super::long_running::spawn_long_running_with_options(
            NAME,
            program,
            args_tail,
            cwd_path,
            BTreeMap::new(),
            EnvMode::InheritClean,
            CaptureConfig::default(),
            session_id,
        )?;
        return Ok(info.into_handle_response());
    }

    let outcome = proc::run(SpawnRequest {
        builtin: NAME,
        program,
        args: args_tail,
        cwd: cwd_path,
        env: BTreeMap::new(),
        env_mode: EnvMode::InheritClean,
        stdin: None,
        timeout,
        capture: CaptureConfig::default(),
    })?;

    let diagnostics = parse_diagnostics(source, &outcome.stdout, &outcome.stderr);
    let diagnostic_values: Vec<VmValue> = diagnostics
        .into_iter()
        .map(|d| {
            let mut entry: BTreeMap<String, VmValue> = BTreeMap::new();
            entry.insert(
                "severity".to_string(),
                VmValue::String(Rc::from(d.severity.as_str())),
            );
            entry.insert("message".to_string(), VmValue::String(Rc::from(d.message)));
            entry.insert(
                "path".to_string(),
                d.path
                    .map(|p| VmValue::String(Rc::from(p)))
                    .unwrap_or(VmValue::Nil),
            );
            entry.insert(
                "line".to_string(),
                d.line.map(VmValue::Int).unwrap_or(VmValue::Nil),
            );
            entry.insert(
                "column".to_string(),
                d.column.map(VmValue::Int).unwrap_or(VmValue::Nil),
            );
            VmValue::Dict(Rc::new(entry))
        })
        .collect();

    Ok(ResponseBuilder::new()
        .int("exit_code", outcome.exit_code as i64)
        .str("stdout", outcome.stdout)
        .str("stderr", outcome.stderr)
        .int("duration_ms", outcome.duration.as_millis() as i64)
        .list("diagnostics", diagnostic_values)
        .build())
}

fn build_default_argv(
    eco: Ecosystem,
    target: Option<&str>,
    release: bool,
) -> (Vec<String>, DiagnosticSource) {
    match eco {
        Ecosystem::Cargo => {
            let mut argv = vec![
                "cargo".into(),
                "build".into(),
                "--message-format=json-diagnostic-rendered-ansi".into(),
            ];
            if release {
                argv.push("--release".into());
            }
            if let Some(t) = target {
                argv.push("--target".into());
                argv.push(t.into());
            }
            (argv, DiagnosticSource::CargoJson)
        }
        Ecosystem::Go => {
            let mut argv = vec!["go".into(), "build".into()];
            if let Some(t) = target {
                argv.push(t.into());
            } else {
                argv.push("./...".into());
            }
            (argv, DiagnosticSource::GoBuild)
        }
        Ecosystem::Swift => {
            let mut argv = vec!["swift".into(), "build".into()];
            if release {
                argv.push("-c".into());
                argv.push("release".into());
            }
            if let Some(t) = target {
                argv.push("--target".into());
                argv.push(t.into());
            }
            (argv, DiagnosticSource::Generic)
        }
        Ecosystem::Npm => (
            vec!["npm".into(), "run".into(), "build".into()],
            DiagnosticSource::Generic,
        ),
        Ecosystem::Pnpm => (
            vec!["pnpm".into(), "run".into(), "build".into()],
            DiagnosticSource::Generic,
        ),
        Ecosystem::Yarn => (
            vec!["yarn".into(), "build".into()],
            DiagnosticSource::Generic,
        ),
        Ecosystem::Gradle => (
            vec!["./gradlew".into(), "build".into()],
            DiagnosticSource::Generic,
        ),
        Ecosystem::Maven => (
            vec!["mvn".into(), "package".into()],
            DiagnosticSource::Generic,
        ),
        Ecosystem::Dotnet => {
            let mut argv = vec!["dotnet".into(), "build".into()];
            if release {
                argv.push("-c".into());
                argv.push("Release".into());
            }
            if let Some(t) = target {
                argv.push(t.into());
            }
            (argv, DiagnosticSource::Generic)
        }
        // Build is a no-op concept for these — fall back to whatever the
        // ecosystem treats as "make sure deps resolve".
        Ecosystem::Pip => (
            vec!["python".into(), "-m".into(), "build".into()],
            DiagnosticSource::Generic,
        ),
        Ecosystem::Uv => (vec!["uv".into(), "build".into()], DiagnosticSource::Generic),
        Ecosystem::Poetry => (
            vec!["poetry".into(), "build".into()],
            DiagnosticSource::Generic,
        ),
        Ecosystem::Bundler => (
            vec!["bundle".into(), "install".into()],
            DiagnosticSource::Generic,
        ),
        Ecosystem::Composer => (
            vec!["composer".into(), "install".into()],
            DiagnosticSource::Generic,
        ),
    }
}

fn infer_diagnostic_source(argv: &[String]) -> DiagnosticSource {
    let joined = argv.join(" ");
    if joined.contains("cargo") && joined.contains("--message-format=json") {
        DiagnosticSource::CargoJson
    } else if argv.first().map(|s| s.as_str()) == Some("go") {
        DiagnosticSource::GoBuild
    } else {
        DiagnosticSource::Generic
    }
}
