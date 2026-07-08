//! `tools/toolchain_facts` — config-declared toolchain identity probes.
//!
//! Verification profiles need facts such as "which Zig cache directory did
//! this run use?" and "which Go/Cargo/Swift version did the check execute
//! under?" without baking language-specific heuristics into Burin. This
//! builtin accepts data rows that describe how to probe a toolchain and
//! returns normalized facts for each row.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use harn_vm::VmValue;
use regex::Regex;

use crate::error::HostlibError;
use crate::process::EnvMode;
use crate::tools::args::to_agent_path;
use crate::tools::payload::{
    optional_env_mode, optional_string, optional_string_list, optional_string_map,
    optional_timeout, optional_u64, parse_argv_program, require_dict_arg, require_string,
};
use crate::tools::proc::{self, CaptureConfig, SpawnOutcome, SpawnRequest};
use crate::tools::response::ResponseBuilder;

pub(crate) const NAME: &str = "hostlib_tools_toolchain_facts";

const MAX_PROBES: usize = 32;
const MAX_STATE_PATHS: usize = 128;

pub(crate) fn handle(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let request = require_dict_arg(NAME, args)?;
    let probes = parse_probes(&request)?;
    let toolchains = probes.into_iter().map(run_probe).collect();
    Ok(ResponseBuilder::new()
        .list("toolchains", toolchains)
        .build())
}

#[derive(Debug)]
struct Probe {
    name: String,
    argv: Vec<String>,
    program: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    env: BTreeMap<String, String>,
    env_mode: EnvMode,
    timeout: Option<std::time::Duration>,
    version: VersionParser,
    cache_env: Vec<String>,
    state_paths: Vec<String>,
}

#[derive(Debug)]
enum VersionParser {
    FirstLine,
    Regex { pattern: Regex, group: usize },
}

fn parse_probes(request: &harn_vm::value::DictMap) -> Result<Vec<Probe>, HostlibError> {
    let raw = request
        .get("probes")
        .ok_or(HostlibError::MissingParameter {
            builtin: NAME,
            param: "probes",
        })?;
    let VmValue::List(items) = raw else {
        return Err(HostlibError::InvalidParameter {
            builtin: NAME,
            param: "probes",
            message: format!("expected list, got {}", raw.type_name()),
        });
    };
    if items.is_empty() {
        return Err(HostlibError::InvalidParameter {
            builtin: NAME,
            param: "probes",
            message: "must contain at least one probe row".to_string(),
        });
    }
    if items.len() > MAX_PROBES {
        return Err(HostlibError::InvalidParameter {
            builtin: NAME,
            param: "probes",
            message: format!("must contain at most {MAX_PROBES} probe rows"),
        });
    }

    items
        .iter()
        .enumerate()
        .map(|(idx, item)| parse_probe(idx, item))
        .collect()
}

fn parse_probe(idx: usize, item: &VmValue) -> Result<Probe, HostlibError> {
    let VmValue::Dict(row) = item else {
        return Err(HostlibError::InvalidParameter {
            builtin: NAME,
            param: "probes",
            message: format!("probe row {idx} must be a dict, got {}", item.type_name()),
        });
    };
    let name = require_string(NAME, row, "name")?;
    if name.trim().is_empty() {
        return Err(HostlibError::InvalidParameter {
            builtin: NAME,
            param: "name",
            message: "must not be empty".to_string(),
        });
    }
    let argv = optional_string_list(NAME, row, "argv")?.ok_or(HostlibError::MissingParameter {
        builtin: NAME,
        param: "argv",
    })?;
    let (program, args) = parse_argv_program(NAME, argv.clone())?;
    let cwd_raw = optional_string(NAME, row, "cwd")?;
    let cwd = proc::parse_cwd(NAME, cwd_raw.as_deref())?;
    let env = optional_string_map(NAME, row, "env")?.unwrap_or_default();
    let env_mode = optional_env_mode(NAME, row, !env.is_empty())?;
    let timeout = optional_timeout(NAME, row, "timeout_ms")?;
    let version = parse_version_parser(row)?;
    let cache_env = optional_string_list(NAME, row, "cache_env")?.unwrap_or_default();
    let state_paths = optional_string_list(NAME, row, "state_paths")?.unwrap_or_default();
    if state_paths.len() > MAX_STATE_PATHS {
        return Err(HostlibError::InvalidParameter {
            builtin: NAME,
            param: "state_paths",
            message: format!("must contain at most {MAX_STATE_PATHS} paths"),
        });
    }

    Ok(Probe {
        name,
        argv,
        program,
        args,
        cwd,
        env,
        env_mode,
        timeout,
        version,
        cache_env,
        state_paths,
    })
}

fn parse_version_parser(row: &harn_vm::value::DictMap) -> Result<VersionParser, HostlibError> {
    let Some(raw) = row.get("version") else {
        return Ok(VersionParser::FirstLine);
    };
    let VmValue::Dict(version) = raw else {
        return Err(HostlibError::InvalidParameter {
            builtin: NAME,
            param: "version",
            message: format!("expected dict, got {}", raw.type_name()),
        });
    };
    match optional_string(NAME, version, "parser")?
        .as_deref()
        .unwrap_or("first_line")
    {
        "first_line" => Ok(VersionParser::FirstLine),
        "regex" => {
            let pattern = require_string(NAME, version, "pattern")?;
            let regex = Regex::new(&pattern).map_err(|error| HostlibError::InvalidParameter {
                builtin: NAME,
                param: "version",
                message: format!("invalid regex pattern: {error}"),
            })?;
            let group = optional_u64(NAME, version, "group")?.unwrap_or(1);
            let group = usize::try_from(group).map_err(|_| HostlibError::InvalidParameter {
                builtin: NAME,
                param: "version",
                message: format!("regex group index is too large: {group}"),
            })?;
            Ok(VersionParser::Regex {
                pattern: regex,
                group,
            })
        }
        other => Err(HostlibError::InvalidParameter {
            builtin: NAME,
            param: "version",
            message: format!("unsupported parser {other:?}; expected first_line or regex"),
        }),
    }
}

fn run_probe(probe: Probe) -> VmValue {
    let request = SpawnRequest {
        builtin: NAME,
        program: probe.program.clone(),
        args: probe.args.clone(),
        cwd: probe.cwd.clone(),
        env: probe.env.clone(),
        env_remove: Vec::new(),
        env_mode: probe.env_mode,
        stdin: None,
        timeout: probe.timeout,
        capture: CaptureConfig::default(),
    };

    match proc::run(request) {
        Ok(outcome) => response_from_outcome(&probe, outcome),
        Err(error) => response_from_error(&probe, error),
    }
}

fn response_from_outcome(probe: &Probe, outcome: SpawnOutcome) -> VmValue {
    let raw_version = combined_output(&outcome.stdout, &outcome.stderr);
    let (status, version) = if outcome.timed_out {
        ("timed_out", None)
    } else if outcome.exit_code != 0 {
        ("failed", None)
    } else {
        match parse_version(&probe.version, &raw_version) {
            Some(version) => ("ok", Some(version)),
            None => ("unparsed", None),
        }
    };
    let available = matches!(status, "ok" | "unparsed");

    base_response(probe, status, available)
        .opt_str("version", version)
        .str("raw_version", raw_version.trim())
        .int("duration_ms", outcome.duration.as_millis() as i64)
        .int("exit_code", outcome.exit_code as i64)
        .bool("timed_out", outcome.timed_out)
        .nil("error")
        .build()
}

fn response_from_error(probe: &Probe, error: HostlibError) -> VmValue {
    let message = error.to_string();
    let status = match error {
        HostlibError::CatastrophicFloor { .. } => "blocked",
        HostlibError::SandboxViolation { .. } => "blocked",
        _ => "spawn_failed",
    };
    base_response(probe, status, false)
        .nil("version")
        .str("raw_version", "")
        .int("duration_ms", 0)
        .nil("exit_code")
        .bool("timed_out", false)
        .str("error", message)
        .build()
}

fn base_response(probe: &Probe, status: &str, available: bool) -> ResponseBuilder {
    ResponseBuilder::new()
        .str("name", &probe.name)
        .bool("available", available)
        .str("status", status)
        .str("command", probe.argv.join(" "))
        .dict("cache_env", cache_env_values(probe))
        .list("state_paths", state_path_values(probe))
}

fn parse_version(parser: &VersionParser, output: &str) -> Option<String> {
    match parser {
        VersionParser::FirstLine => output
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(ToOwned::to_owned),
        VersionParser::Regex { pattern, group } => pattern
            .captures(output)
            .and_then(|captures| captures.get(*group))
            .map(|match_| match_.as_str().trim().to_string())
            .filter(|version| !version.is_empty()),
    }
}

fn combined_output(stdout: &str, stderr: &str) -> String {
    match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (false, false) => format!("{stdout}\n{stderr}"),
    }
}

fn cache_env_values(probe: &Probe) -> harn_vm::value::DictMap {
    let mut out = harn_vm::value::DictMap::new();
    for key in &probe.cache_env {
        let (source, value) = if let Some(value) = probe.env.get(key) {
            ("probe_env", Some(value.clone()))
        } else if probe.env_mode != EnvMode::Replace {
            match std::env::var(key) {
                Ok(value) => ("process_env", Some(value)),
                Err(_) => ("unset", None),
            }
        } else {
            ("unset", None)
        };
        let mut entry = harn_vm::value::DictMap::new();
        entry.insert(
            harn_vm::value::intern_key("value"),
            value.map(VmValue::string).unwrap_or(VmValue::Nil),
        );
        entry.insert(
            harn_vm::value::intern_key("source"),
            VmValue::string(source),
        );
        out.insert(harn_vm::value::intern_key(key), VmValue::dict(entry));
    }
    out
}

fn state_path_values(probe: &Probe) -> Vec<VmValue> {
    let cwd = probe
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    probe
        .state_paths
        .iter()
        .map(|raw| state_path_value(&cwd, raw))
        .collect()
}

fn state_path_value(cwd: &Path, raw: &str) -> VmValue {
    let raw_path = Path::new(raw);
    let resolved = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        cwd.join(raw_path)
    };
    let (exists, kind) = match std::fs::metadata(&resolved) {
        Ok(metadata) if metadata.is_dir() => (true, "directory"),
        Ok(metadata) if metadata.is_file() => (true, "file"),
        Ok(_) => (true, "other"),
        Err(_) => (false, "missing"),
    };
    let mut map = harn_vm::value::DictMap::new();
    map.insert(harn_vm::value::intern_key("path"), VmValue::string(raw));
    map.insert(
        harn_vm::value::intern_key("resolved_path"),
        VmValue::string(to_agent_path(&resolved)),
    );
    map.insert(harn_vm::value::intern_key("exists"), VmValue::Bool(exists));
    map.insert(harn_vm::value::intern_key("kind"), VmValue::string(kind));
    VmValue::dict(map)
}
