use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use harn_vm::llm_config::{self, AuthEnv};
use harn_vm::runtime_paths;
use harn_vm::secrets::{
    configured_default_chain, EnvSecretProvider, KeyringSecretProvider, SecretId,
    DEFAULT_SECRET_PROVIDER_CHAIN, SECRET_PROVIDER_CHAIN_ENV,
};

use crate::package;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DoctorStatus {
    #[default]
    Ok,
    Warn,
    Fail,
    Skip,
}

impl DoctorStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DoctorCheck {
    pub(crate) id: String,
    pub(crate) status: DoctorStatus,
    pub(crate) label: String,
    pub(crate) detail: String,
    /// Shell command a contributor can run to fix this check.
    pub(crate) fix_command: Option<String>,
    /// Link to docs that explain the underlying tool or env var.
    pub(crate) docs_url: Option<String>,
    /// Workflows this check gates when it is failing — stable strings such as
    /// `"build"`, `"test"`, `"release"`, `"publish"`, `"portal"`,
    /// `"scripting"`, or `"editor"`. Consumed by Burin Code / Harn Cloud
    /// preflight automation.
    pub(crate) blocks: Vec<&'static str>,
}

impl DoctorCheck {
    fn ensure_id(&mut self) {
        if self.id.is_empty() {
            self.id = self.label.clone();
        }
    }
}

/// Stable schema version for `harn doctor --json`. Bump when the JSON shape
/// changes in a way that downstream consumers (Burin Code preflight, Harn
/// Cloud onboarding) need to react to.
pub(crate) const DOCTOR_JSON_SCHEMA_VERSION: &str = "1";

#[derive(Debug, Clone, Default)]
pub(crate) struct DoctorOptions {
    pub network: bool,
    pub json: bool,
}

#[allow(dead_code)]
pub(crate) async fn run_doctor(network: bool) {
    run_doctor_with_options(DoctorOptions {
        network,
        json: false,
    })
    .await
}

pub(crate) async fn run_doctor_with_options(opts: DoctorOptions) {
    let version_check = check_harn_version();
    let mut checks: Vec<DoctorCheck> = Vec::new();
    checks.push(version_check.clone());
    checks.extend(check_toolchain());
    checks.extend(check_dev_tools());
    checks.extend(check_protocol_artifacts());
    checks.extend(check_portal());
    checks.extend(check_platform_capabilities());
    checks.extend(check_provider_selection());
    checks.extend(check_secret_providers());
    checks.extend(check_provider_credentials());
    checks.extend(check_manifest().await);
    checks.extend(check_event_log());
    checks.extend(check_metadata_cache());
    checks.extend(check_skills());
    checks.push(check_ollama().await);
    let hardware = check_hardware();
    checks.push(hardware.0.clone());
    checks.extend(check_provider_health(opts.network).await);

    for check in &mut checks {
        check.ensure_id();
    }

    let next_step = next_step_suggestion(&checks);
    let failed = checks.iter().any(|c| c.status == DoctorStatus::Fail);

    if opts.json {
        let json = build_doctor_json(&checks, &hardware.1, &next_step);
        match serde_json::to_string_pretty(&json) {
            Ok(text) => println!("{text}"),
            Err(error) => eprintln!("failed to serialize doctor JSON: {error}"),
        }
        if failed {
            std::process::exit(1);
        }
        return;
    }

    println!("Harn doctor");
    println!();
    for check in &checks {
        println!(
            "{:>4}  {:<24} {}",
            check.status.label(),
            check.label,
            check.detail
        );
        if check.status != DoctorStatus::Ok && check.status != DoctorStatus::Skip {
            if let Some(fix) = &check.fix_command {
                println!("       fix: {fix}");
            }
            if let Some(docs) = &check.docs_url {
                println!("       docs: {docs}");
            }
            if !check.blocks.is_empty() {
                println!("       blocks: {}", check.blocks.join(", "));
            }
        }
    }
    println!();
    println!("--- Summary ---");
    let summary = doctor_summary(&checks);
    println!(
        "OK={ok} WARN={warn} FAIL={fail} SKIP={skip}",
        ok = summary.ok,
        warn = summary.warn,
        fail = summary.fail,
        skip = summary.skip,
    );
    if !summary.blocked_flows.is_empty() {
        println!("blocked: {}", summary.blocked_flows.join(", "));
    }
    println!();
    println!("--- Next step ---");
    println!("{next_step}");

    if failed {
        std::process::exit(1);
    }
}

#[derive(Debug, Default)]
struct DoctorSummary {
    ok: usize,
    warn: usize,
    fail: usize,
    skip: usize,
    blocked_flows: Vec<&'static str>,
}

fn doctor_summary(checks: &[DoctorCheck]) -> DoctorSummary {
    let mut summary = DoctorSummary::default();
    let mut blocks: std::collections::BTreeSet<&'static str> = std::collections::BTreeSet::new();
    for check in checks {
        match check.status {
            DoctorStatus::Ok => summary.ok += 1,
            DoctorStatus::Warn => summary.warn += 1,
            DoctorStatus::Fail => summary.fail += 1,
            DoctorStatus::Skip => summary.skip += 1,
        }
        if check.status == DoctorStatus::Fail {
            for flow in &check.blocks {
                blocks.insert(flow);
            }
        }
    }
    summary.blocked_flows = blocks.into_iter().collect();
    summary
}

#[derive(Debug, Clone, Default)]
pub(crate) struct HardwareSnapshot {
    pub ram_gb: Option<u64>,
    pub gpu: String,
    pub free_disk_gb: Option<u64>,
}

fn build_doctor_json(
    checks: &[DoctorCheck],
    hardware: &HardwareSnapshot,
    next_step: &str,
) -> serde_json::Value {
    let providers_path = llm_config::loaded_config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let model_defaults = serialize_model_defaults();
    let checks_json: Vec<serde_json::Value> = checks
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id,
                "label": c.label,
                "status": match c.status {
                    DoctorStatus::Ok => "ok",
                    DoctorStatus::Warn => "warn",
                    DoctorStatus::Fail => "fail",
                    DoctorStatus::Skip => "skip",
                },
                "detail": c.detail,
                "fix_command": c.fix_command,
                "docs_url": c.docs_url,
                "blocks": c.blocks,
            })
        })
        .collect();
    let summary = doctor_summary(checks);
    serde_json::json!({
        "schema_version": DOCTOR_JSON_SCHEMA_VERSION,
        "harn_version": env!("CARGO_PKG_VERSION"),
        "providers_config_path": providers_path,
        "model_defaults": model_defaults,
        "checks": checks_json,
        "summary": {
            "ok": summary.ok,
            "warn": summary.warn,
            "fail": summary.fail,
            "skip": summary.skip,
            "blocked_flows": summary.blocked_flows,
        },
        "hardware": {
            "ram_gb": hardware.ram_gb,
            "gpu": hardware.gpu,
            "free_disk_gb": hardware.free_disk_gb,
        },
        "next_step": next_step,
    })
}

fn serialize_model_defaults() -> serde_json::Value {
    let cfg = llm_config::load_config();
    let mut out = serde_json::Map::new();
    for (pattern, defaults) in &cfg.model_defaults {
        let mut inner = serde_json::Map::new();
        for (k, v) in defaults {
            // Best-effort TOML -> JSON conversion via toml's serde to JSON value.
            let json_val: serde_json::Value =
                serde_json::to_value(v).unwrap_or(serde_json::Value::Null);
            inner.insert(k.clone(), json_val);
        }
        out.insert(pattern.clone(), serde_json::Value::Object(inner));
    }
    serde_json::Value::Object(out)
}

/// Re-export of the canonical "no LLM provider credentials" guidance line so
/// `harn try` and other CLI surfaces can print the same message users see when
/// their script raises the equivalent VM error.
pub(crate) fn no_credentials_hint() -> String {
    harn_vm::llm::no_credentials_message()
}

fn check_harn_version() -> DoctorCheck {
    let version = env!("CARGO_PKG_VERSION");
    let providers_path = llm_config::loaded_config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<built-in defaults>".to_string());
    DoctorCheck {
        id: "harn_version".to_string(),
        status: DoctorStatus::Ok,
        label: "harn version".to_string(),
        detail: format!("v{version} (providers: {providers_path})"),
        ..Default::default()
    }
}

fn check_provider_credentials() -> Vec<DoctorCheck> {
    let mut providers = llm_config::provider_names();
    providers.sort();

    let mut checks = Vec::new();
    let mut any_creds = false;
    for name in &providers {
        let Some(def) = llm_config::provider_config(name) else {
            continue;
        };
        if def.auth_style == "none" || name == "ollama" {
            checks.push(DoctorCheck {
                id: format!("creds:{name}"),
                status: DoctorStatus::Skip,
                label: format!("creds:{name}"),
                detail: "no key required".to_string(),
                ..Default::default()
            });
            continue;
        }
        let envs = llm_config::auth_env_names(&def.auth_env);
        if envs.is_empty() {
            checks.push(DoctorCheck {
                id: format!("creds:{name}"),
                status: DoctorStatus::Skip,
                label: format!("creds:{name}"),
                detail: "no env vars declared".to_string(),
                ..Default::default()
            });
            continue;
        }
        let mut found: Vec<String> = Vec::new();
        for env in &envs {
            if std::env::var(env).map(|v| !v.is_empty()).unwrap_or(false) {
                found.push(env.clone());
            }
        }
        if found.is_empty() {
            checks.push(DoctorCheck {
                id: format!("creds:{name}"),
                status: DoctorStatus::Warn,
                label: format!("creds:{name}"),
                detail: format!("missing: {}", envs.join(", ")),
                fix_command: Some(format!("export {}=…", envs[0])),
                docs_url: Some("https://harnlang.com/docs/llm/providers.html".to_string()),
                blocks: Vec::new(),
            });
        } else {
            any_creds = true;
            checks.push(DoctorCheck {
                id: format!("creds:{name}"),
                status: DoctorStatus::Ok,
                label: format!("creds:{name}"),
                detail: format!("present: {}", found.join(", ")),
                ..Default::default()
            });
        }
    }

    // Add an aggregate row that fails only when no provider has creds AND
    // ollama appears unreachable. Reachability is best-effort: we only flag
    // FAIL when the synchronous `ollama --version` probe errors. Otherwise
    // demote to WARN so users without local models still get a softer signal.
    let ollama_present = which::which("ollama").is_ok();
    let aggregate_status = if any_creds {
        DoctorStatus::Ok
    } else if ollama_present {
        DoctorStatus::Warn
    } else {
        DoctorStatus::Fail
    };
    let aggregate_detail = if any_creds {
        "at least one provider has credentials".to_string()
    } else if ollama_present {
        "no cloud credentials; falling back to local Ollama".to_string()
    } else {
        "no provider credentials and no local Ollama".to_string()
    };
    let aggregate_blocks: Vec<&'static str> = if aggregate_status == DoctorStatus::Fail {
        vec!["scripting"]
    } else {
        Vec::new()
    };
    let aggregate_fix = if aggregate_status == DoctorStatus::Fail {
        Some("harn models recommend && harn quickstart --non-interactive".to_string())
    } else {
        None
    };
    checks.push(DoctorCheck {
        id: "creds:any".to_string(),
        status: aggregate_status,
        label: "credentials".to_string(),
        detail: aggregate_detail,
        fix_command: aggregate_fix,
        docs_url: Some("https://harnlang.com/docs/llm/providers.html".to_string()),
        blocks: aggregate_blocks,
    });

    checks
}

async fn check_ollama() -> DoctorCheck {
    if which::which("ollama").is_err() {
        return DoctorCheck {
            id: "ollama".to_string(),
            status: DoctorStatus::Skip,
            label: "ollama".to_string(),
            detail: "ollama not installed; see https://ollama.com".to_string(),
            ..Default::default()
        };
    }
    let output = tokio::process::Command::new("ollama")
        .arg("list")
        .output()
        .await;
    match output {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            let mut models: Vec<String> = text
                .lines()
                .skip(1) // header
                .filter_map(|line| line.split_whitespace().next().map(|s| s.to_string()))
                .collect();
            if models.is_empty() {
                DoctorCheck {
                    id: "ollama".to_string(),
                    status: DoctorStatus::Warn,
                    label: "ollama".to_string(),
                    detail: "ollama running, no models pulled; try `harn models recommend`"
                        .to_string(),
                    ..Default::default()
                }
            } else {
                let total = models.len();
                models.truncate(5);
                DoctorCheck {
                    id: "ollama".to_string(),
                    status: DoctorStatus::Ok,
                    label: "ollama".to_string(),
                    detail: format!("{total} models: {}", models.join(", ")),
                    ..Default::default()
                }
            }
        }
        Ok(out) => DoctorCheck {
            id: "ollama".to_string(),
            status: DoctorStatus::Warn,
            label: "ollama".to_string(),
            detail: format!("`ollama list` exited {}", out.status),
            ..Default::default()
        },
        Err(error) => DoctorCheck {
            id: "ollama".to_string(),
            status: DoctorStatus::Skip,
            label: "ollama".to_string(),
            detail: format!("ollama not callable: {error}"),
            ..Default::default()
        },
    }
}

fn check_hardware() -> (DoctorCheck, HardwareSnapshot) {
    let ram_gb = detect_ram_gb();
    let gpu = detect_gpu();
    let free_disk_gb = detect_free_disk_gb();

    let mut detail_parts = Vec::new();
    if let Some(ram) = ram_gb {
        detail_parts.push(format!("RAM {ram}GB"));
    } else {
        detail_parts.push("RAM unknown".to_string());
    }
    detail_parts.push(format!("GPU {gpu}"));
    let status = match free_disk_gb {
        Some(gb) if gb < 1 => DoctorStatus::Fail,
        Some(gb) if gb < 5 => DoctorStatus::Warn,
        _ => DoctorStatus::Ok,
    };
    if let Some(gb) = free_disk_gb {
        detail_parts.push(format!("free disk {gb}GB"));
    } else {
        detail_parts.push("free disk unknown".to_string());
    }
    let snapshot = HardwareSnapshot {
        ram_gb,
        gpu: gpu.clone(),
        free_disk_gb,
    };
    (
        DoctorCheck {
            id: "hardware".to_string(),
            status,
            label: "hardware".to_string(),
            detail: detail_parts.join(", "),
            ..Default::default()
        },
        snapshot,
    )
}

fn detect_ram_gb() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let bytes: u64 = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .ok()?;
        Some(bytes / (1024 * 1024 * 1024))
    }
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                let kb: u64 = rest
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse().ok())?;
                return Some(kb / (1024 * 1024));
            }
        }
        None
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

fn detect_gpu() -> String {
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("sysctl")
            .args(["-n", "hw.optional.arm64"])
            .output();
        if let Ok(out) = output {
            if out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "1" {
                return "Apple Silicon (MPS available)".to_string();
            }
        }
        "CPU-only".to_string()
    }
    #[cfg(target_os = "linux")]
    {
        if std::path::Path::new("/dev/nvidia0").exists() {
            return "NVIDIA GPU detected".to_string();
        }
        "CPU-only".to_string()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        "unknown".to_string()
    }
}

fn detect_free_disk_gb() -> Option<u64> {
    let cwd = std::env::current_dir().unwrap_or_default();
    // metadata_dir lives under the same volume as the rest of `~/.harn` /
    // workspace state on every platform we ship today, so it's a fine proxy
    // for "where do we write run state".
    let metadata = runtime_paths::metadata_dir(&cwd);
    let probe = if metadata.exists() { metadata } else { cwd };
    let output = std::process::Command::new("df")
        .args(["-Pk"])
        .arg(&probe)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().nth(1)?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    let avail_kb: u64 = cols.get(3)?.parse().ok()?;
    Some(avail_kb / (1024 * 1024))
}

fn next_step_suggestion(checks: &[DoctorCheck]) -> String {
    let creds_ok = checks
        .iter()
        .any(|c| c.id == "creds:any" && c.status == DoctorStatus::Ok);
    let ollama = checks.iter().find(|c| c.id == "ollama");
    let manifest_present = checks
        .iter()
        .any(|c| c.label == "manifest" && c.status == DoctorStatus::Ok);
    let any_fail = checks.iter().any(|c| c.status == DoctorStatus::Fail);

    let no_ollama_models = matches!(
        ollama.map(|c| c.status),
        Some(DoctorStatus::Skip) | Some(DoctorStatus::Warn) | None
    );

    if !creds_ok && no_ollama_models {
        return "Run `harn models recommend` to pick a starter model for your machine.".to_string();
    }
    if let Some(c) = ollama {
        if c.status == DoctorStatus::Warn {
            return "Run `harn models recommend` to pick a starter model for your machine."
                .to_string();
        }
    }
    if creds_ok && !manifest_present {
        return "Run `harn new my-agent --template agent` to scaffold a project.".to_string();
    }
    if !any_fail {
        return "You're ready. Try `harn try \"summarize this README\"` or `harn run examples/hello.harn`."
            .to_string();
    }
    "Address the failing checks above, then re-run `harn doctor`.".to_string()
}

/// Definition for a CLI tool we want `harn doctor` to inspect.
struct ToolCheck {
    id: &'static str,
    /// Binary name on PATH.
    binary: &'static str,
    /// `--version` arguments. Defaults to `["--version"]` when empty.
    version_args: &'static [&'static str],
    /// Severity when the tool is missing. Use `Fail` only for genuinely
    /// load-bearing toolchain components; everything else should be `Warn`.
    missing_status: DoctorStatus,
    /// Shell command to install the tool — surfaced as `fix_command` in JSON.
    install_hint: &'static str,
    /// Documentation URL for the tool.
    docs_url: &'static str,
    /// Workflows that fail when this tool is missing or broken.
    blocks: &'static [&'static str],
}

impl ToolCheck {
    fn run(&self) -> DoctorCheck {
        let args: &[&str] = if self.version_args.is_empty() {
            &["--version"]
        } else {
            self.version_args
        };
        let label = self.id.to_string();
        let result = Command::new(self.binary).args(args).output();
        let mut check = match result {
            Ok(output) if output.status.success() => DoctorCheck {
                id: self.id.to_string(),
                status: DoctorStatus::Ok,
                label,
                detail: String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("version detected")
                    .to_string(),
                ..Default::default()
            },
            Ok(output) => DoctorCheck {
                id: self.id.to_string(),
                status: self.missing_status,
                label,
                detail: format!(
                    "`{} {}` exited with {}",
                    self.binary,
                    args.join(" "),
                    output.status
                ),
                ..Default::default()
            },
            Err(error) => DoctorCheck {
                id: self.id.to_string(),
                status: self.missing_status,
                label,
                detail: format!("{} not found in PATH: {error}", self.binary),
                ..Default::default()
            },
        };
        check.fix_command = Some(self.install_hint.to_string());
        check.docs_url = Some(self.docs_url.to_string());
        check.blocks = self.blocks.to_vec();
        check
    }
}

/// Checks the load-bearing toolchain — Rust + Cargo. These block every code
/// workflow if missing, so they FAIL hard.
fn check_toolchain() -> Vec<DoctorCheck> {
    const TOOLS: &[ToolCheck] = &[
        ToolCheck {
            id: "rustc",
            binary: "rustc",
            version_args: &["--version"],
            missing_status: DoctorStatus::Fail,
            install_hint:
                "https://rustup.rs (curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh)",
            docs_url: "https://www.rust-lang.org/tools/install",
            blocks: &["build", "test", "release", "publish"],
        },
        ToolCheck {
            id: "cargo",
            binary: "cargo",
            version_args: &["--version"],
            missing_status: DoctorStatus::Fail,
            install_hint: "https://rustup.rs",
            docs_url: "https://doc.rust-lang.org/cargo/",
            blocks: &["build", "test", "release", "publish"],
        },
    ];
    TOOLS.iter().map(ToolCheck::run).collect()
}

/// Checks optional but recommended developer tools. These never FAIL — every
/// missing tool degrades a workflow but has a documented fallback (cargo test
/// for nextest, no-op for sccache, skipped lint for actionlint, etc).
fn check_dev_tools() -> Vec<DoctorCheck> {
    const TOOLS: &[ToolCheck] = &[
        ToolCheck {
            id: "cargo-nextest",
            binary: "cargo-nextest",
            version_args: &["nextest", "--version"],
            missing_status: DoctorStatus::Warn,
            install_hint: "cargo install cargo-nextest --locked",
            docs_url: "https://nexte.st",
            blocks: &[],
        },
        ToolCheck {
            id: "sccache",
            binary: "sccache",
            version_args: &["--version"],
            missing_status: DoctorStatus::Warn,
            install_hint: "cargo install sccache --locked",
            docs_url: "https://github.com/mozilla/sccache",
            blocks: &[],
        },
        ToolCheck {
            id: "actionlint",
            binary: "actionlint",
            version_args: &["-version"],
            missing_status: DoctorStatus::Warn,
            install_hint: "brew install actionlint  # or: go install github.com/rhysd/actionlint/cmd/actionlint@latest",
            docs_url: "https://github.com/rhysd/actionlint",
            blocks: &[],
        },
    ];
    TOOLS.iter().map(ToolCheck::run).collect()
}

/// Checks the portal frontend toolchain — node, npm, and the installed
/// `node_modules`. Skipped entirely when not running inside the repo.
fn check_portal() -> Vec<DoctorCheck> {
    let Some(repo) = find_harn_repo_root(&std::env::current_dir().unwrap_or_default()) else {
        return vec![DoctorCheck {
            id: "portal".to_string(),
            status: DoctorStatus::Skip,
            label: "portal".to_string(),
            detail: "not running inside the harn repo; skipping portal checks".to_string(),
            ..Default::default()
        }];
    };

    let mut checks = Vec::new();

    const NODE_TOOLS: &[ToolCheck] = &[
        ToolCheck {
            id: "node",
            binary: "node",
            version_args: &["--version"],
            missing_status: DoctorStatus::Warn,
            install_hint: "https://nodejs.org (or `brew install node`, `nvm install --lts`)",
            docs_url: "https://nodejs.org",
            blocks: &["portal", "editor"],
        },
        ToolCheck {
            id: "npm",
            binary: "npm",
            version_args: &["--version"],
            missing_status: DoctorStatus::Warn,
            install_hint: "ships with Node.js — install Node first",
            docs_url: "https://docs.npmjs.com",
            blocks: &["portal", "editor"],
        },
    ];
    for tool in NODE_TOOLS {
        checks.push(tool.run());
    }

    let portal_dir = repo.join("crates/harn-cli/portal");
    let pkg_json = portal_dir.join("package.json");
    let node_modules = portal_dir.join("node_modules");
    if !pkg_json.is_file() {
        // The portal directory disappeared — surface as Warn so we don't
        // crash on a malformed checkout.
        checks.push(DoctorCheck {
            id: "portal:deps".to_string(),
            status: DoctorStatus::Warn,
            label: "portal:deps".to_string(),
            detail: format!("missing {}", pkg_json.display()),
            ..Default::default()
        });
        return checks;
    }
    let detail = if node_modules.is_dir() {
        DoctorCheck {
            id: "portal:deps".to_string(),
            status: DoctorStatus::Ok,
            label: "portal:deps".to_string(),
            detail: format!("installed at {}", node_modules.display()),
            ..Default::default()
        }
    } else {
        DoctorCheck {
            id: "portal:deps".to_string(),
            status: DoctorStatus::Warn,
            label: "portal:deps".to_string(),
            detail: format!("node_modules missing under {}", portal_dir.display()),
            fix_command: Some(format!("(cd {} && npm install)", portal_dir.display())),
            docs_url: Some("https://harnlang.com/docs/portal.html".to_string()),
            blocks: vec!["portal"],
        }
    };
    checks.push(detail);
    checks
}

/// Checks the checked-in protocol artifacts against what the current binary
/// would generate. Only runs inside the harn repo.
fn check_protocol_artifacts() -> Vec<DoctorCheck> {
    let Some(repo) = find_harn_repo_root(&std::env::current_dir().unwrap_or_default()) else {
        return vec![DoctorCheck {
            id: "protocol-artifacts".to_string(),
            status: DoctorStatus::Skip,
            label: "protocol-artifacts".to_string(),
            detail: "not running inside the harn repo; skipping artifact drift check".to_string(),
            ..Default::default()
        }];
    };

    let ts_path = repo.join("spec/protocol-artifacts/harn-protocol.ts");
    let Ok(text) = fs::read_to_string(&ts_path) else {
        return vec![DoctorCheck {
            id: "protocol-artifacts".to_string(),
            status: DoctorStatus::Warn,
            label: "protocol-artifacts".to_string(),
            detail: format!("unable to read {}", ts_path.display()),
            fix_command: Some("make gen-protocol-artifacts".to_string()),
            docs_url: Some("https://harnlang.com/docs/protocol-artifacts.html".to_string()),
            blocks: vec!["release"],
        }];
    };

    // The TS artifact records the harn version it was generated against:
    //   export const HARN_PROTOCOL_ARTIFACT_VERSION = "0.8.4"
    let pinned_version = text
        .lines()
        .find_map(|line| {
            line.split_once("HARN_PROTOCOL_ARTIFACT_VERSION = \"")
                .map(|(_, rest)| rest)
                .and_then(|rest| rest.split_once('"').map(|(v, _)| v.to_string()))
        })
        .unwrap_or_default();
    let current = env!("CARGO_PKG_VERSION");
    if pinned_version.is_empty() {
        return vec![DoctorCheck {
            id: "protocol-artifacts".to_string(),
            status: DoctorStatus::Warn,
            label: "protocol-artifacts".to_string(),
            detail: format!(
                "could not parse HARN_PROTOCOL_ARTIFACT_VERSION from {}",
                ts_path.display()
            ),
            fix_command: Some("make gen-protocol-artifacts".to_string()),
            docs_url: Some("https://harnlang.com/docs/protocol-artifacts.html".to_string()),
            blocks: vec!["release"],
        }];
    }
    if pinned_version == current {
        vec![DoctorCheck {
            id: "protocol-artifacts".to_string(),
            status: DoctorStatus::Ok,
            label: "protocol-artifacts".to_string(),
            detail: format!("pinned at v{pinned_version}"),
            ..Default::default()
        }]
    } else {
        vec![DoctorCheck {
            id: "protocol-artifacts".to_string(),
            status: DoctorStatus::Fail,
            label: "protocol-artifacts".to_string(),
            detail: format!("stale: pinned v{pinned_version}, current v{current}"),
            fix_command: Some("make gen-protocol-artifacts".to_string()),
            docs_url: Some("https://harnlang.com/docs/protocol-artifacts.html".to_string()),
            blocks: vec!["release"],
        }]
    }
}

/// Best-effort platform capability probes. None of these block builds; they
/// help the contributor understand what features will work on this machine.
fn check_platform_capabilities() -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    // File watching — `harn run --watch`, `harn test --watch`, and the
    // playground depend on the `notify` crate's recommended backend.
    let watcher = notify::recommended_watcher(|_res: notify::Result<notify::Event>| {});
    let watcher_check = match watcher {
        Ok(_) => DoctorCheck {
            id: "platform:file-watcher".to_string(),
            status: DoctorStatus::Ok,
            label: "file-watcher".to_string(),
            detail: format!("notify backend `{}`", notify_backend_name()),
            ..Default::default()
        },
        Err(error) => DoctorCheck {
            id: "platform:file-watcher".to_string(),
            status: DoctorStatus::Warn,
            label: "file-watcher".to_string(),
            detail: format!(
                "notify backend unavailable: {error}; --watch and the playground will fall back to polling"
            ),
            fix_command: None,
            docs_url: Some("https://docs.rs/notify".to_string()),
            blocks: vec![],
        },
    };
    checks.push(watcher_check);

    // Browser opener — `harn portal`, `harn mcp login`, and OAuth flows
    // shell out via the `webbrowser` crate. We don't open a browser here;
    // we just check whether a known opener is on PATH.
    let opener = browser_opener();
    let opener_check = if let Some(name) = opener {
        DoctorCheck {
            id: "platform:browser-opener".to_string(),
            status: DoctorStatus::Ok,
            label: "browser-opener".to_string(),
            detail: format!("`{name}` available"),
            ..Default::default()
        }
    } else {
        DoctorCheck {
            id: "platform:browser-opener".to_string(),
            status: DoctorStatus::Warn,
            label: "browser-opener".to_string(),
            detail: "no system opener (open/xdg-open/start) on PATH; OAuth flows print URLs"
                .to_string(),
            docs_url: None,
            fix_command: Some(
                "install xdg-utils (Linux) or use `--no-open` flags to print URLs".to_string(),
            ),
            blocks: vec![],
        }
    };
    checks.push(opener_check);

    checks
}

#[cfg(target_os = "macos")]
fn notify_backend_name() -> &'static str {
    "fsevents"
}
#[cfg(target_os = "linux")]
fn notify_backend_name() -> &'static str {
    "inotify"
}
#[cfg(target_os = "windows")]
fn notify_backend_name() -> &'static str {
    "ReadDirectoryChangesW"
}
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn notify_backend_name() -> &'static str {
    "polling"
}

#[cfg(target_os = "macos")]
fn browser_opener() -> Option<&'static str> {
    if which::which("open").is_ok() {
        Some("open")
    } else {
        None
    }
}
#[cfg(target_os = "linux")]
fn browser_opener() -> Option<&'static str> {
    if which::which("xdg-open").is_ok() {
        Some("xdg-open")
    } else {
        None
    }
}
#[cfg(target_os = "windows")]
fn browser_opener() -> Option<&'static str> {
    // `start` is a cmd.exe builtin, not a binary on PATH. Treat the platform
    // as always supporting it.
    Some("start")
}
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn browser_opener() -> Option<&'static str> {
    None
}

/// Walk up from `start` looking for a marker that uniquely identifies the
/// harn repo (the checked-in protocol artifacts directory). Returns the repo
/// root when found.
fn find_harn_repo_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join("spec/protocol-artifacts/manifest.json").is_file()
            && dir.join("crates/harn-cli/Cargo.toml").is_file()
        {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn check_provider_selection() -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    if let Ok(path) = std::env::var("HARN_PROVIDERS_CONFIG") {
        let config_path = PathBuf::from(&path);
        let status = if config_path.is_file() {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Fail
        };
        checks.push(DoctorCheck {
            id: String::new(),
            status,
            label: "providers config".to_string(),
            detail: format!("HARN_PROVIDERS_CONFIG={path}"),
            ..Default::default()
        });
    }

    if let Ok(provider) = std::env::var("HARN_LLM_PROVIDER") {
        let status = if llm_config::provider_config(&provider).is_some() {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Fail
        };
        checks.push(DoctorCheck {
            id: String::new(),
            status,
            label: "selected provider".to_string(),
            detail: format!("HARN_LLM_PROVIDER={provider}"),
            ..Default::default()
        });
    }

    checks
}

fn check_secret_providers() -> Vec<DoctorCheck> {
    let namespace = default_secret_namespace();
    let configured = std::env::var(SECRET_PROVIDER_CHAIN_ENV)
        .unwrap_or_else(|_| DEFAULT_SECRET_PROVIDER_CHAIN.to_string());
    let mut checks = Vec::new();

    match configured_default_chain(namespace.clone()) {
        Ok(chain) => checks.push(DoctorCheck {
            id: String::new(),
            status: if chain.providers().is_empty() {
                DoctorStatus::Fail
            } else {
                DoctorStatus::Ok
            },
            label: "secret providers".to_string(),
            detail: format!(
                "{} (namespace {})",
                configured.replace(',', " -> "),
                namespace
            ),
            ..Default::default()
        }),
        Err(error) => {
            checks.push(DoctorCheck {
                id: String::new(),
                status: DoctorStatus::Fail,
                label: "secret providers".to_string(),
                detail: error.to_string(),
                ..Default::default()
            });
            return checks;
        }
    }

    for provider in configured
        .split(',')
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
    {
        match provider {
            "env" => {
                let env_provider = EnvSecretProvider::new(namespace.clone());
                let sample = env_provider.env_var_name(&SecretId::new("sample", "token"));
                checks.push(DoctorCheck {
                    id: String::new(),
                    status: DoctorStatus::Ok,
                    label: "secret:env".to_string(),
                    detail: format!("reads process env via {sample}"),
                    ..Default::default()
                });
            }
            "keyring" => {
                let keyring_provider = KeyringSecretProvider::new(namespace.clone());
                match keyring_provider.healthcheck() {
                    Ok(detail) => checks.push(DoctorCheck {
                        id: String::new(),
                        status: DoctorStatus::Ok,
                        label: "secret:keyring".to_string(),
                        detail,
                        ..Default::default()
                    }),
                    Err(error) => checks.push(DoctorCheck {
                        id: String::new(),
                        status: DoctorStatus::Fail,
                        label: "secret:keyring".to_string(),
                        detail: error.to_string(),
                        ..Default::default()
                    }),
                }
            }
            other => checks.push(DoctorCheck {
                id: String::new(),
                status: DoctorStatus::Fail,
                label: format!("secret:{other}"),
                detail: format!("unsupported provider '{other}'"),
                ..Default::default()
            }),
        }
    }

    checks
}

async fn check_manifest() -> Vec<DoctorCheck> {
    let Some(path) = find_nearest_manifest(&std::env::current_dir().unwrap_or_default()) else {
        return vec![DoctorCheck {
            id: String::new(),
            status: DoctorStatus::Warn,
            label: "manifest".to_string(),
            detail: "no harn.toml found in the current directory or its parents".to_string(),
            ..Default::default()
        }];
    };

    let manifest_result = read_manifest(&path);
    let manifest = match manifest_result {
        Ok(manifest) => manifest,
        Err(error) => {
            return vec![DoctorCheck {
                id: String::new(),
                status: DoctorStatus::Fail,
                label: "manifest".to_string(),
                detail: format!("{}: {error}", path.display()),
                ..Default::default()
            }];
        }
    };

    let package_name = manifest
        .package
        .as_ref()
        .and_then(|pkg| pkg.name.clone())
        .unwrap_or_else(|| "unnamed package".to_string());

    let mut checks = vec![DoctorCheck {
        id: String::new(),
        status: DoctorStatus::Ok,
        label: "manifest".to_string(),
        detail: format!("{} ({package_name})", path.display()),
        ..Default::default()
    }];

    let mut seen_names = HashSet::new();
    for server in &manifest.mcp {
        let name = server.name.clone();
        if !seen_names.insert(name.clone()) {
            checks.push(DoctorCheck {
                id: String::new(),
                status: DoctorStatus::Fail,
                label: format!("mcp:{name}"),
                detail: "duplicate MCP server name".to_string(),
                ..Default::default()
            });
            continue;
        }
        if server.url.trim().is_empty() && server.command.trim().is_empty() {
            checks.push(DoctorCheck {
                id: String::new(),
                status: DoctorStatus::Warn,
                label: format!("mcp:{name}"),
                detail: "entry has neither url nor command".to_string(),
                ..Default::default()
            });
        } else {
            checks.push(DoctorCheck {
                id: String::new(),
                status: DoctorStatus::Ok,
                label: format!("mcp:{name}"),
                detail: if !server.url.trim().is_empty() {
                    format!("remote {}", server.url)
                } else {
                    format!("stdio {}", server.command)
                },
                ..Default::default()
            });
        }
    }

    let extensions = package::load_runtime_extensions(&path);
    if !extensions.triggers.is_empty() {
        let mut vm = harn_vm::Vm::new();
        harn_vm::register_vm_stdlib(&mut vm);
        crate::install_default_hostlib(&mut vm);
        harn_vm::clear_trigger_registry();
        match package::install_manifest_triggers(&mut vm, &extensions).await {
            Ok(()) => {
                for trigger in harn_vm::snapshot_trigger_bindings() {
                    checks.push(DoctorCheck {
                        id: String::new(),
                        status: DoctorStatus::Ok,
                        label: format!("trigger:{}", trigger.id),
                        detail: format!(
                            "{} via {} handler={} state={} version={} metrics={}",
                            trigger.kind,
                            trigger.provider,
                            trigger.handler_kind,
                            trigger.state.as_str(),
                            trigger.version,
                            format_trigger_metrics(&trigger.metrics),
                        ),
                        ..Default::default()
                    });
                }
                let dispatcher = harn_vm::snapshot_dispatcher_stats();
                checks.push(DoctorCheck {
                    id: String::new(),
                    status: DoctorStatus::Ok,
                    label: "dispatcher".to_string(),
                    detail: format!(
                        "in_flight={} retry_queue_depth={} dlq_depth={}",
                        dispatcher.in_flight, dispatcher.retry_queue_depth, dispatcher.dlq_depth,
                    ),
                    ..Default::default()
                });
                harn_vm::clear_trigger_registry();
            }
            Err(error) => checks.push(DoctorCheck {
                id: String::new(),
                status: DoctorStatus::Fail,
                label: "triggers".to_string(),
                detail: error.to_string(),
                ..Default::default()
            }),
        }
    }

    checks
}

fn format_trigger_metrics(metrics: &harn_vm::TriggerMetricsSnapshot) -> String {
    format!(
        "received={} dispatched={} failed={} dlq={} in_flight={}",
        metrics.received, metrics.dispatched, metrics.failed, metrics.dlq, metrics.in_flight
    )
}

fn check_skills() -> Vec<DoctorCheck> {
    use crate::skill_loader;

    let loaded = skill_loader::load_skills(&skill_loader::SkillLoaderInputs {
        cli_dirs: Vec::new(),
        source_path: None,
    });

    let mut checks = Vec::new();
    let winners = &loaded.report.winners;
    if winners.is_empty() {
        checks.push(DoctorCheck {
            id: String::new(),
            status: DoctorStatus::Skip,
            label: "skills".to_string(),
            detail: "no SKILL.md files discovered (use --skill-dir, $HARN_SKILLS_PATH, .harn/skills, or harn.toml [skills])".to_string(),
            ..Default::default()
        });
    } else {
        let mut by_layer: std::collections::BTreeMap<&str, usize> =
            std::collections::BTreeMap::new();
        for w in winners {
            *by_layer.entry(w.layer.label()).or_default() += 1;
        }
        let breakdown: Vec<String> = by_layer.iter().map(|(k, v)| format!("{v} {k}")).collect();
        checks.push(DoctorCheck {
            id: String::new(),
            status: DoctorStatus::Ok,
            label: "skills".to_string(),
            detail: format!("{} loaded ({})", winners.len(), breakdown.join(", ")),
            ..Default::default()
        });
    }

    for shadow in &loaded.report.shadowed {
        checks.push(DoctorCheck {
            id: String::new(),
            status: DoctorStatus::Warn,
            label: format!("skill:{}", shadow.id),
            detail: format!(
                "shadowed by {} layer; {} version at {} is hidden",
                shadow.winner.label(),
                shadow.loser.label(),
                shadow.loser_origin,
            ),
            ..Default::default()
        });
    }

    for (id, fields) in &loaded.report.unknown_fields {
        checks.push(DoctorCheck {
            id: String::new(),
            status: DoctorStatus::Warn,
            label: format!("skill:{id}"),
            detail: format!(
                "unknown frontmatter field(s) forwarded as metadata: {}",
                fields.join(", ")
            ),
            ..Default::default()
        });
    }

    for layer in &loaded.report.disabled_layers {
        checks.push(DoctorCheck {
            id: String::new(),
            status: DoctorStatus::Skip,
            label: format!("skills-layer:{}", layer.label()),
            detail: "layer disabled by harn.toml [skills.disable]".to_string(),
            ..Default::default()
        });
    }

    checks
}

fn check_metadata_cache() -> Vec<DoctorCheck> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let metadata_dir = runtime_paths::metadata_dir(&cwd);
    let read_dir = match fs::read_dir(&metadata_dir) {
        Ok(read_dir) => read_dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return vec![DoctorCheck {
                id: String::new(),
                status: DoctorStatus::Skip,
                label: "metadata".to_string(),
                detail: format!("no metadata cache under {}", metadata_dir.display()),
                ..Default::default()
            }];
        }
        Err(error) => {
            return vec![DoctorCheck {
                id: String::new(),
                status: DoctorStatus::Warn,
                label: "metadata".to_string(),
                detail: format!("failed to read {}: {error}", metadata_dir.display()),
                ..Default::default()
            }];
        }
    };

    let mut namespace_summaries = Vec::new();
    let mut saw_legacy_root = false;
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_file() && entry.file_name() == "root.json" {
            saw_legacy_root = true;
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        let shard_path = path.join("entries.json");
        let Ok(text) = fs::read_to_string(&shard_path) else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(namespace) = parsed.get("namespace").and_then(|value| value.as_str()) else {
            continue;
        };
        let count = parsed
            .get("entries")
            .and_then(|value| value.as_object())
            .map(|entries| entries.len())
            .unwrap_or(0);
        namespace_summaries.push(format!("{namespace} ({count} dirs)"));
    }

    namespace_summaries.sort();
    let detail = if namespace_summaries.is_empty() {
        if saw_legacy_root {
            format!(
                "legacy metadata shard present at {}",
                metadata_dir.join("root.json").display()
            )
        } else {
            format!(
                "metadata directory present at {} but no namespace shards found",
                metadata_dir.display()
            )
        }
    } else {
        namespace_summaries.join(", ")
    };

    vec![DoctorCheck {
        id: String::new(),
        status: DoctorStatus::Ok,
        label: "metadata".to_string(),
        detail,
        ..Default::default()
    }]
}

fn check_event_log() -> Vec<DoctorCheck> {
    let cwd = std::env::current_dir().unwrap_or_default();
    match harn_vm::event_log::describe_for_base_dir(&cwd) {
        Ok(description) => {
            let detail = match description.location {
                Some(path) => format!(
                    "{} ({}, {} B)",
                    description.backend,
                    path.display(),
                    description.size_bytes.unwrap_or(0)
                ),
                None => format!("{} (in-memory)", description.backend),
            };
            vec![DoctorCheck {
                id: String::new(),
                status: DoctorStatus::Ok,
                label: "event log".to_string(),
                detail,
                ..Default::default()
            }]
        }
        Err(error) => vec![DoctorCheck {
            id: String::new(),
            status: DoctorStatus::Fail,
            label: "event log".to_string(),
            detail: error.to_string(),
            ..Default::default()
        }],
    }
}

async fn check_provider_health(network: bool) -> Vec<DoctorCheck> {
    let mut providers = llm_config::provider_names();
    providers.sort();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("reqwest client");

    let mut checks = Vec::new();
    for provider_name in providers {
        if !network {
            checks.push(DoctorCheck {
                id: String::new(),
                status: DoctorStatus::Skip,
                label: format!("provider:{provider_name}"),
                detail: "network checks disabled".to_string(),
                ..Default::default()
            });
            continue;
        }

        // Local OpenAI-compatible providers expose loaded models through
        // `/v1/models`. Probe the selected model directly so a missing model
        // surfaces with the distinct `model_missing` category rather than a
        // generic 200 OK from the unified healthcheck.
        if let Some(def) = llm_config::provider_config(&provider_name) {
            if harn_vm::llm::supports_model_readiness_probe(&def) {
                if let Some(model) = harn_vm::llm::selected_model_for_provider(&provider_name) {
                    let api_key = match &def.auth_env {
                        AuthEnv::None => String::new(),
                        AuthEnv::Single(name) => std::env::var(name).unwrap_or_default(),
                        AuthEnv::Multiple(names) => names
                            .iter()
                            .find_map(|name| std::env::var(name).ok())
                            .unwrap_or_default(),
                    };
                    checks.push(run_model_readiness(&provider_name, &model, &api_key).await);
                    continue;
                }
            }
        }

        let result = harn_vm::llm::run_provider_healthcheck_with_options(
            &provider_name,
            harn_vm::llm::ProviderHealthcheckOptions {
                api_key: None,
                client: Some(client.clone()),
            },
        )
        .await;
        checks.push(healthcheck_result_to_doctor_check(result));
    }
    checks
}

async fn run_model_readiness(provider_name: &str, model: &str, api_key: &str) -> DoctorCheck {
    let readiness =
        harn_vm::llm::probe_openai_compatible_model(provider_name, model, api_key).await;
    let status = if readiness.valid {
        DoctorStatus::Ok
    } else {
        match readiness.category.as_str() {
            "model_missing" | "bad_status" | "invalid_url" => DoctorStatus::Fail,
            _ => DoctorStatus::Warn,
        }
    };
    DoctorCheck {
        id: String::new(),
        status,
        label: format!("provider:{provider_name}"),
        detail: format!("{}: {}", readiness.category, readiness.message),
        ..Default::default()
    }
}

fn healthcheck_result_to_doctor_check(
    result: harn_vm::llm::ProviderHealthcheckResult,
) -> DoctorCheck {
    let reason = result
        .metadata
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let status_code = result
        .metadata
        .get("status")
        .and_then(|value| value.as_u64())
        .unwrap_or_default();
    let status = if result.valid {
        DoctorStatus::Ok
    } else {
        match reason {
            "no_healthcheck" => DoctorStatus::Skip,
            "missing_credentials" => DoctorStatus::Warn,
            "http_status" if status_code == 401 || status_code == 403 => DoctorStatus::Fail,
            "http_status" => DoctorStatus::Warn,
            _ => DoctorStatus::Fail,
        }
    };
    let detail = if result.valid {
        let url = result
            .metadata
            .get("url")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let status = result
            .metadata
            .get("status")
            .and_then(|value| value.as_u64())
            .map(|value| value.to_string())
            .unwrap_or_else(|| "ok".to_string());
        if url.is_empty() {
            status
        } else {
            format!("{status} {url}")
        }
    } else {
        result.message
    };

    DoctorCheck {
        id: String::new(),
        status,
        label: format!("provider:{}", result.provider),
        detail,
        ..Default::default()
    }
}

fn find_nearest_manifest(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let manifest = dir.join("harn.toml");
        if manifest.is_file() {
            return Some(manifest);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn default_secret_namespace() -> String {
    if let Ok(namespace) = std::env::var("HARN_SECRET_NAMESPACE") {
        if !namespace.trim().is_empty() {
            return namespace;
        }
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    let leaf = cwd
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace");
    format!("harn/{leaf}")
}

fn read_manifest(path: &Path) -> Result<package::Manifest, String> {
    let content =
        fs::read_to_string(path).map_err(|error| format!("failed to read manifest: {error}"))?;
    toml::from_str::<package::Manifest>(&content)
        .map_err(|error| format!("failed to parse manifest: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{
        build_doctor_json, check_event_log, check_hardware, check_manifest, check_ollama,
        check_platform_capabilities, check_protocol_artifacts, doctor_summary, find_harn_repo_root,
        find_nearest_manifest, format_trigger_metrics, healthcheck_result_to_doctor_check,
        next_step_suggestion, read_manifest, DoctorCheck, DoctorStatus, HardwareSnapshot,
        DOCTOR_JSON_SCHEMA_VERSION,
    };
    use harn_vm::llm::ProviderHealthcheckResult;
    use harn_vm::llm_config::{AuthEnv, HealthcheckDef, ProviderDef};
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn build_healthcheck_url_uses_base_and_path() {
        let def = ProviderDef {
            base_url: "https://example.com/api".to_string(),
            ..Default::default()
        };
        let healthcheck = HealthcheckDef {
            method: "GET".to_string(),
            path: Some("/health".to_string()),
            url: None,
            body: None,
        };

        assert_eq!(
            harn_vm::llm::build_healthcheck_url(&def, &healthcheck),
            "https://example.com/api/health"
        );
    }

    #[test]
    fn doctor_maps_healthcheck_results_to_existing_statuses() {
        let missing = ProviderHealthcheckResult {
            provider: "openai".to_string(),
            valid: false,
            message: "Missing credentials".to_string(),
            metadata: BTreeMap::from([("reason".to_string(), json!("missing_credentials"))]),
        };
        let auth_rejected = ProviderHealthcheckResult {
            provider: "openai".to_string(),
            valid: false,
            message: "openai returned HTTP 401".to_string(),
            metadata: BTreeMap::from([
                ("reason".to_string(), json!("http_status")),
                ("status".to_string(), json!(401)),
            ]),
        };
        let no_probe = ProviderHealthcheckResult {
            provider: "custom".to_string(),
            valid: false,
            message: "No healthcheck configured".to_string(),
            metadata: BTreeMap::from([("reason".to_string(), json!("no_healthcheck"))]),
        };

        assert_eq!(
            healthcheck_result_to_doctor_check(missing).status,
            DoctorStatus::Warn
        );
        assert_eq!(
            healthcheck_result_to_doctor_check(auth_rejected).status,
            DoctorStatus::Fail
        );
        assert_eq!(
            healthcheck_result_to_doctor_check(no_probe).status,
            DoctorStatus::Skip
        );
    }

    #[test]
    fn find_nearest_manifest_walks_up() {
        let root = tempfile::tempdir().expect("tempdir");
        let nested = root.path().join("a/b/c");
        std::fs::create_dir_all(&nested).expect("create nested dirs");
        std::fs::write(
            root.path().join("harn.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .expect("write manifest");

        let found = find_nearest_manifest(&nested).expect("manifest");
        assert_eq!(found, root.path().join("harn.toml"));
    }

    #[test]
    fn read_manifest_accepts_basic_package() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("harn.toml");
        std::fs::write(&path, "[package]\nname = \"demo\"\n").expect("write manifest");

        let manifest = read_manifest(&path).expect("manifest parses");
        assert_eq!(
            manifest.package.and_then(|pkg| pkg.name),
            Some("demo".to_string())
        );
    }

    #[test]
    fn auth_env_multiple_variant_exists_for_provider_checks() {
        let auth = AuthEnv::Multiple(vec!["FIRST".to_string(), "SECOND".to_string()]);
        let AuthEnv::Multiple(names) = auth else {
            panic!("expected multiple auth envs");
        };
        assert_eq!(names, vec!["FIRST".to_string(), "SECOND".to_string()]);
    }

    #[test]
    fn event_log_check_reports_backend_and_location() {
        let _state_guard = crate::tests::common::harn_state_lock::lock_harn_state();
        let dir = tempfile::tempdir().expect("tempdir");
        let sqlite_path = dir.path().join(".harn/events.sqlite");
        std::env::set_var(harn_vm::event_log::HARN_EVENT_LOG_BACKEND_ENV, "sqlite");
        std::env::set_var(
            harn_vm::event_log::HARN_EVENT_LOG_SQLITE_PATH_ENV,
            &sqlite_path,
        );
        let checks = check_event_log();
        std::env::remove_var(harn_vm::event_log::HARN_EVENT_LOG_BACKEND_ENV);
        std::env::remove_var(harn_vm::event_log::HARN_EVENT_LOG_SQLITE_PATH_ENV);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, super::DoctorStatus::Ok);
        assert!(checks[0].detail.contains("sqlite"));
        assert!(checks[0]
            .detail
            .contains(&sqlite_path.display().to_string()));
    }

    #[test]
    fn format_trigger_metrics_renders_snapshot() {
        let rendered = format_trigger_metrics(&harn_vm::TriggerMetricsSnapshot {
            received: 1,
            dispatched: 2,
            failed: 3,
            dlq: 4,
            in_flight: 5,
            last_received_ms: None,
            cost_total_usd_micros: 0,
            cost_today_usd_micros: 0,
            cost_hour_usd_micros: 0,
            autonomous_decisions_total: 0,
            autonomous_decisions_today: 0,
            autonomous_decisions_hour: 0,
        });
        assert_eq!(
            rendered,
            "received=1 dispatched=2 failed=3 dlq=4 in_flight=5"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn check_manifest_reports_loaded_triggers() {
        let _cwd_guard = crate::tests::common::cwd_lock::lock_cwd_async().await;
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".git")).expect("git dir");
        std::fs::write(
            dir.path().join("harn.toml"),
            r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::on_new_issue"
budget = { daily_cost_usd = 5.0, max_concurrent = 10 }
secrets = { signing_secret = "github/webhook-secret" }
"#,
        )
        .expect("write manifest");
        std::fs::write(
            dir.path().join("lib.harn"),
            r#"
import "std/triggers"

pub fn on_new_issue(event: TriggerEvent) {
  log(event.kind)
}
"#,
        )
        .expect("write lib");

        let previous = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(dir.path()).expect("set cwd");
        let checks = check_manifest().await;
        std::env::set_current_dir(previous).expect("restore cwd");

        let trigger = checks
            .iter()
            .find(|check| check.label == "trigger:github-new-issue")
            .expect("trigger check");
        assert_eq!(trigger.status, DoctorStatus::Ok);
        assert!(trigger.detail.contains("webhook via github"));
        assert!(trigger.detail.contains("handler=local"));
        assert!(trigger.detail.contains("state=active"));
        assert!(trigger.detail.contains("version=1"));
        assert!(trigger.detail.contains("metrics=received=0"));

        let dispatcher = checks
            .iter()
            .find(|check| check.label == "dispatcher")
            .expect("dispatcher check");
        assert_eq!(dispatcher.status, DoctorStatus::Ok);
        assert_eq!(
            dispatcher.detail,
            "in_flight=0 retry_queue_depth=0 dlq_depth=0"
        );
    }

    fn check(id: &str, status: DoctorStatus) -> DoctorCheck {
        DoctorCheck {
            id: id.to_string(),
            status,
            label: id.to_string(),
            detail: String::new(),
            ..Default::default()
        }
    }

    #[test]
    fn next_step_no_creds_no_ollama_recommends_models() {
        let checks = vec![
            check("creds:any", DoctorStatus::Fail),
            check("ollama", DoctorStatus::Skip),
        ];
        let next = next_step_suggestion(&checks);
        assert!(
            next.contains("harn models recommend"),
            "unexpected next step: {next}"
        );
    }

    #[test]
    fn next_step_creds_present_no_manifest_recommends_new() {
        let checks = vec![
            check("creds:any", DoctorStatus::Ok),
            check("ollama", DoctorStatus::Ok),
        ];
        let next = next_step_suggestion(&checks);
        assert!(next.contains("harn new"), "unexpected next step: {next}");
    }

    #[test]
    fn json_emits_stable_ids() {
        let checks = vec![
            check("harn_version", DoctorStatus::Ok),
            check("creds:openai", DoctorStatus::Warn),
        ];
        let hardware = HardwareSnapshot {
            ram_gb: Some(16),
            gpu: "mps".to_string(),
            free_disk_gb: Some(100),
        };
        let value = build_doctor_json(&checks, &hardware, "test next step");
        let checks_arr = value["checks"].as_array().expect("checks array");
        assert_eq!(checks_arr.len(), 2);
        assert_eq!(checks_arr[0]["id"], "harn_version");
        assert_eq!(checks_arr[0]["status"], "ok");
        assert_eq!(checks_arr[1]["id"], "creds:openai");
        assert_eq!(checks_arr[1]["status"], "warn");
        assert_eq!(value["hardware"]["ram_gb"], 16);
        assert_eq!(value["hardware"]["gpu"], "mps");
        assert_eq!(value["next_step"], "test next step");
        assert!(value.get("harn_version").is_some());
    }

    #[test]
    fn hardware_check_does_not_fail_on_unknown_platform() {
        let (check, _snapshot) = check_hardware();
        assert_ne!(
            check.status,
            DoctorStatus::Fail,
            "hardware check returned Fail unexpectedly: {}",
            check.detail
        );
    }

    #[allow(clippy::await_holding_lock)] // sync state lock guards process-global PATH mutation across the await
    #[tokio::test(flavor = "current_thread")]
    async fn ollama_check_skips_when_binary_missing() {
        // Force `which` to fail by clearing PATH for the duration of this
        // assertion. We restore it immediately on return; the global env
        // mutation is bracketed and the test is single-threaded.
        let _state_guard = crate::tests::common::harn_state_lock::lock_harn_state();
        let prev = std::env::var_os("PATH");
        std::env::set_var("PATH", "");
        let result = check_ollama().await;
        if let Some(prev) = prev {
            std::env::set_var("PATH", prev);
        } else {
            std::env::remove_var("PATH");
        }
        assert_eq!(result.status, DoctorStatus::Skip);
        assert!(
            result.detail.contains("not installed") || result.detail.contains("not callable"),
            "unexpected ollama detail: {}",
            result.detail
        );
    }

    #[test]
    fn doctor_summary_aggregates_status_counts_and_blocked_flows() {
        let checks = vec![
            DoctorCheck {
                id: "rustc".to_string(),
                status: DoctorStatus::Fail,
                blocks: vec!["build", "test"],
                ..Default::default()
            },
            DoctorCheck {
                id: "node".to_string(),
                status: DoctorStatus::Fail,
                blocks: vec!["portal"],
                ..Default::default()
            },
            DoctorCheck {
                id: "creds:openai".to_string(),
                status: DoctorStatus::Warn,
                blocks: vec!["scripting"], // not blocking — only Fail counts
                ..Default::default()
            },
            DoctorCheck {
                id: "harn_version".to_string(),
                status: DoctorStatus::Ok,
                ..Default::default()
            },
            DoctorCheck {
                id: "metadata".to_string(),
                status: DoctorStatus::Skip,
                ..Default::default()
            },
        ];
        let summary = doctor_summary(&checks);
        assert_eq!(summary.ok, 1);
        assert_eq!(summary.warn, 1);
        assert_eq!(summary.fail, 2);
        assert_eq!(summary.skip, 1);
        // Sorted, deduplicated, alphabetical.
        assert_eq!(summary.blocked_flows, vec!["build", "portal", "test"]);
    }

    #[test]
    fn doctor_json_includes_schema_version_summary_and_per_check_metadata() {
        let checks = vec![
            DoctorCheck {
                id: "rustc".to_string(),
                status: DoctorStatus::Fail,
                label: "rustc".to_string(),
                detail: "missing".to_string(),
                fix_command: Some("install rust".to_string()),
                docs_url: Some("https://rustup.rs".to_string()),
                blocks: vec!["build", "test"],
            },
            DoctorCheck {
                id: "harn_version".to_string(),
                status: DoctorStatus::Ok,
                label: "harn version".to_string(),
                detail: "v0.0.0".to_string(),
                ..Default::default()
            },
        ];
        let hardware = HardwareSnapshot::default();
        let value = build_doctor_json(&checks, &hardware, "next");

        assert_eq!(value["schema_version"], DOCTOR_JSON_SCHEMA_VERSION);
        assert_eq!(value["summary"]["ok"], 1);
        assert_eq!(value["summary"]["fail"], 1);
        assert_eq!(
            value["summary"]["blocked_flows"],
            serde_json::json!(["build", "test"])
        );

        let first = &value["checks"][0];
        assert_eq!(first["fix_command"], "install rust");
        assert_eq!(first["docs_url"], "https://rustup.rs");
        assert_eq!(first["blocks"], serde_json::json!(["build", "test"]));

        let second = &value["checks"][1];
        // Optional fields surface as JSON null / empty array — the keys
        // themselves are always present so consumers can rely on the shape.
        assert!(second["fix_command"].is_null());
        assert!(second["docs_url"].is_null());
        assert_eq!(second["blocks"], serde_json::json!([]));
    }

    #[test]
    fn find_harn_repo_root_walks_up_from_nested_dir() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(root.path().join("spec/protocol-artifacts")).unwrap();
        std::fs::write(
            root.path().join("spec/protocol-artifacts/manifest.json"),
            "{}",
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join("crates/harn-cli")).unwrap();
        std::fs::write(root.path().join("crates/harn-cli/Cargo.toml"), "").unwrap();

        let nested = root.path().join("crates/harn-cli/src/commands");
        std::fs::create_dir_all(&nested).unwrap();

        let found = find_harn_repo_root(&nested).expect("repo root");
        assert_eq!(found, root.path());

        let unrelated = tempfile::tempdir().expect("tempdir");
        assert!(find_harn_repo_root(unrelated.path()).is_none());
    }

    #[test]
    fn protocol_artifacts_check_skipped_outside_repo() {
        let _cwd_guard = crate::tests::common::cwd_lock::lock_cwd();
        let dir = tempfile::tempdir().expect("tempdir");
        let prev = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(dir.path()).expect("set cwd");
        let checks = check_protocol_artifacts();
        std::env::set_current_dir(prev).expect("restore cwd");
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, DoctorStatus::Skip);
        assert_eq!(checks[0].id, "protocol-artifacts");
    }

    #[test]
    fn platform_capability_check_emits_known_ids() {
        let checks = check_platform_capabilities();
        let ids: std::collections::BTreeSet<&str> = checks.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains("platform:file-watcher"), "ids: {ids:?}");
        assert!(ids.contains("platform:browser-opener"), "ids: {ids:?}");
        // None of the platform checks should ever be FAIL — they're
        // best-effort capability probes with documented fallbacks.
        for check in &checks {
            assert_ne!(check.status, DoctorStatus::Fail, "{}", check.detail);
        }
    }
}
