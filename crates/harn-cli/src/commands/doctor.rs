use std::collections::HashSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use harn_vm::llm_config;
use harn_vm::runtime_paths;
use harn_vm::secrets::{
    configured_default_chain, EnvSecretProvider, KeyringSecretProvider, SecretId,
    DEFAULT_SECRET_PROVIDER_CHAIN, SECRET_PROVIDER_CHAIN_ENV,
};
use serde::Serialize;

use crate::dispatch;
use crate::env_guard::ScopedEnvVar;
use crate::json_envelope::{to_string_pretty, JsonEnvelope, JsonOutput};
use crate::package;

mod next_step;
mod repo_checks;

use next_step::next_step_suggestion;
use repo_checks::{check_protocol_artifacts, find_harn_repo_root};

/// Env var the embedded `cli/doctor` script reads to pick up the raw
/// `DoctorReport` payload (renderable shape — no envelope wrapper).
/// Kept separate from [`DOCTOR_REPORT_ENVELOPE_ENV`] so the script can
/// inspect typed fields (status, label, summary counts) without
/// re-walking through `data` lookups.
const DOCTOR_REPORT_ENV: &str = "HARN_DOCTOR_REPORT_JSON";

/// Env var carrying the pre-serialized `JsonEnvelope<DoctorReport>`
/// for the `--json` path. Harn's `json_stringify_pretty` would
/// alphabetise the envelope keys (`data`, `error`, `ok`,
/// `schemaVersion`, `warnings`) which would break downstream
/// consumers that expect the legacy serde declaration order. Hand
/// the script the canonical bytes so it can echo them verbatim.
const DOCTOR_REPORT_ENVELOPE_ENV: &str = "HARN_DOCTOR_REPORT_ENVELOPE_JSON";

/// Serialises the dispatch path so concurrent in-process callers
/// don't race on the global env vars the shim sets. Same pattern as
/// the other partial-port commands (W5/W7/W9/W10).
static DISPATCH_DOCTOR_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DoctorStatus {
    #[default]
    Ok,
    Warn,
    Fail,
    Skip,
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
    /// `"scripting"`, or `"editor"`. Consumed by downstream hosts / cloud
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
/// changes in a way that downstream consumers (host preflight, Harn
/// Cloud onboarding) need to react to.
pub(crate) const DOCTOR_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Default)]
pub(crate) struct DoctorOptions {
    pub json: bool,
    /// When true, fan out an HTTP probe per configured provider and record
    /// reachability + p50 latency on the report. When false, only credential
    /// presence is checked so the command stays offline-clean.
    pub check_providers: bool,
    /// When true, run `cargo check --target <triple>` per Rustup-installed
    /// target plus the canonical Linux/macOS/Windows/WASM triples. Off by
    /// default because each probe spawns Cargo and dominates wall-clock.
    pub check_targets: bool,
}

pub(crate) async fn run_doctor_with_options(opts: DoctorOptions) {
    let exit_code = run_dispatch(opts).await;
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

/// `.harn` dispatch path (W12 — see harn#2312). The Rust shim still
/// runs every probe (toolchain, providers, MCP, manifest health,
/// hardware, capabilities) because each one reaches into a different
/// VM/host facility that script-land can't drive yet. The shim
/// assembles the structured [`DoctorReport`], serialises it to JSON,
/// and dispatches to `cli/doctor.harn` for formatting. The script
/// owns the human-readable section layout and the JSON envelope
/// pass-through.
async fn run_dispatch(opts: DoctorOptions) -> i32 {
    let report = build_report(&opts).await;
    let report_json = match serde_json::to_string(&report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to serialise doctor report: {error}");
            return 1;
        }
    };
    // Pre-render the envelope here so the script can echo the bytes
    // verbatim — Harn's `json_stringify_pretty` would re-order keys.
    let envelope_json = to_string_pretty(&report.clone().into_envelope());

    let _guard = DISPATCH_DOCTOR_LOCK.lock().await;
    let _report_guard = ScopedEnvVar::set(DOCTOR_REPORT_ENV, &report_json);
    let _envelope_guard = ScopedEnvVar::set(DOCTOR_REPORT_ENVELOPE_ENV, &envelope_json);
    let outcome = dispatch::run_embedded_script("doctor", Vec::new(), opts.json).await;
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if !outcome.stdout.is_empty() {
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
    outcome.exit_code
}

async fn build_report(opts: &DoctorOptions) -> DoctorReport {
    let version_check = check_harn_version();
    let mut checks: Vec<DoctorCheck> = Vec::new();
    checks.push(version_check);
    let toolchain = check_toolchain();
    checks.extend(toolchain.iter().cloned());
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
    let (hardware_check, hardware) = check_hardware();
    checks.push(hardware_check);

    let providers = collect_providers(opts.check_providers).await;
    checks.extend(provider_doctor_checks(&providers));

    let targets = collect_targets(opts.check_targets).await;
    checks.extend(target_doctor_checks(&targets));

    for check in &mut checks {
        check.ensure_id();
    }

    let next_step = next_step_suggestion(&checks);
    let summary = build_summary(&checks);
    let host = build_host_info(&toolchain);
    let capabilities = stdlib_capability_matrix();

    DoctorReport {
        host,
        providers_config_path: llm_config::loaded_config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        model_defaults: serialize_model_defaults(),
        targets,
        providers,
        capabilities,
        checks: checks.iter().map(DoctorCheckJson::from).collect(),
        summary,
        hardware,
        next_step,
    }
}

/// The complete machine-readable report. Wrapped in [`JsonEnvelope`] for
/// `--json`; the embedded `.harn` renderer consumes the same structure for the
/// human view.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DoctorReport {
    pub host: HostInfo,
    /// Path to the loaded providers config (empty string when defaults are
    /// in effect — kept stringly-typed to match downstream consumers).
    pub providers_config_path: String,
    /// TOML-derived `[model_defaults]` map flattened to JSON.
    pub model_defaults: serde_json::Value,
    pub targets: Vec<TargetInfo>,
    pub providers: Vec<ProviderInfo>,
    pub capabilities: Vec<CapabilityInfo>,
    pub checks: Vec<DoctorCheckJson>,
    pub hardware: HardwareSnapshot,
    pub summary: DoctorSummary,
    pub next_step: String,
}

impl JsonOutput for DoctorReport {
    const SCHEMA_VERSION: u32 = DOCTOR_SCHEMA_VERSION;
    type Data = DoctorReport;
    fn into_envelope(self) -> JsonEnvelope<DoctorReport> {
        JsonEnvelope::ok(Self::SCHEMA_VERSION, self)
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct HostInfo {
    /// Platform identifier as reported by `std::env::consts::OS`
    /// (`"macos"`, `"linux"`, `"windows"`, `"freebsd"`, …).
    pub os: String,
    /// CPU architecture as reported by `std::env::consts::ARCH`
    /// (`"aarch64"`, `"x86_64"`, …).
    pub arch: String,
    pub harn_version: String,
    /// `rustc --version` first line, when the toolchain is on PATH.
    pub rust_toolchain: Option<String>,
    /// `cargo --version` first line, when Cargo is on PATH.
    pub cargo_version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TargetInfo {
    pub triple: String,
    /// `rustup target list --installed` contains this triple.
    pub installed: bool,
    /// `Some(true)` when `cargo check --target` succeeded, `Some(false)`
    /// when it failed. `None` when [`DoctorOptions::check_targets`] was
    /// off, i.e. no probe was attempted.
    pub buildable: Option<bool>,
    /// Human-readable notes attached during enumeration — typically the
    /// cargo error message when `buildable == Some(false)`, or the
    /// reason a target was skipped.
    pub reasons: Vec<String>,
    pub checked: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderInfo {
    pub name: String,
    /// True when credentials are resolvable (env var set or auth not
    /// required for this provider).
    pub configured: bool,
    /// `Some(true)` when the healthcheck returned a success status,
    /// `Some(false)` on failure, `None` when no probe was attempted.
    pub reachable: Option<bool>,
    pub latency_ms: Option<u64>,
    pub errors: Vec<String>,
    pub probed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CapabilityInfo {
    /// Canonical capability name as used by the orchestration policy
    /// (`workspace.read_text`, `process.exec`, `network`, …).
    pub name: String,
    /// Sandbox profiles in which this capability can be exercised on
    /// the current host. Static today; gains runtime probes once the
    /// per-platform sandbox engines expose a self-test.
    pub available_in_sandbox_profile: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct DoctorSummary {
    pub ok: usize,
    /// Count of `warn` checks. Aliased as `warning` to match the spec
    /// language ("warning count") used by the issue.
    pub warning: usize,
    /// Count of `fail` checks. Aliased as `blocking` per the spec to
    /// communicate that these checks gate one or more workflows.
    pub blocking: usize,
    pub skip: usize,
    /// Distinct, sorted list of workflow tags that any failing check
    /// declared in its `blocks` set (`build`, `test`, `portal`, …).
    pub blocked_flows: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DoctorCheckJson {
    pub id: String,
    pub label: String,
    pub status: &'static str,
    pub detail: String,
    pub fix_command: Option<String>,
    pub docs_url: Option<String>,
    pub blocks: Vec<&'static str>,
}

impl From<&DoctorCheck> for DoctorCheckJson {
    fn from(check: &DoctorCheck) -> Self {
        Self {
            id: check.id.clone(),
            label: check.label.clone(),
            status: match check.status {
                DoctorStatus::Ok => "ok",
                DoctorStatus::Warn => "warn",
                DoctorStatus::Fail => "fail",
                DoctorStatus::Skip => "skip",
            },
            detail: check.detail.clone(),
            fix_command: check.fix_command.clone(),
            docs_url: check.docs_url.clone(),
            blocks: check.blocks.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct HardwareSnapshot {
    pub ram_gb: Option<u64>,
    pub gpu: String,
    pub free_disk_gb: Option<u64>,
}

fn build_summary(checks: &[DoctorCheck]) -> DoctorSummary {
    let mut summary = DoctorSummary::default();
    let mut blocks: std::collections::BTreeSet<&'static str> = std::collections::BTreeSet::new();
    for check in checks {
        match check.status {
            DoctorStatus::Ok => summary.ok += 1,
            DoctorStatus::Warn => summary.warning += 1,
            DoctorStatus::Fail => summary.blocking += 1,
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

fn build_host_info(toolchain_checks: &[DoctorCheck]) -> HostInfo {
    let rust_toolchain = toolchain_checks
        .iter()
        .find(|c| c.id == "rustc" && c.status == DoctorStatus::Ok)
        .map(|c| c.detail.clone());
    let cargo_version = toolchain_checks
        .iter()
        .find(|c| c.id == "cargo" && c.status == DoctorStatus::Ok)
        .map(|c| c.detail.clone());
    HostInfo {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        harn_version: env!("CARGO_PKG_VERSION").to_string(),
        rust_toolchain,
        cargo_version,
    }
}

/// Canonical Rust target triples Harn ships for. Probed/listed by
/// `harn doctor` so contributors can see at a glance whether Linux,
/// macOS, Windows, and WASM builds will work locally without spawning
/// a CI run.
const CANONICAL_TARGETS: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "wasm32-unknown-unknown",
    "wasm32-wasip1",
];

async fn collect_targets(check_targets: bool) -> Vec<TargetInfo> {
    let installed = installed_rustup_targets();
    let mut triples: std::collections::BTreeSet<String> =
        CANONICAL_TARGETS.iter().map(|t| (*t).to_string()).collect();
    triples.extend(installed.iter().cloned());

    let mut targets = Vec::with_capacity(triples.len());
    for triple in triples {
        let is_installed = installed.contains(&triple);
        let mut reasons = Vec::new();
        let (buildable, checked) = if !check_targets {
            (None, false)
        } else if !is_installed {
            reasons.push(format!(
                "target not installed; run `rustup target add {triple}` to probe"
            ));
            (Some(false), true)
        } else {
            match cargo_check_target(&triple).await {
                Ok(()) => (Some(true), true),
                Err(detail) => {
                    reasons.push(detail);
                    (Some(false), true)
                }
            }
        };
        targets.push(TargetInfo {
            triple,
            installed: is_installed,
            buildable,
            reasons,
            checked,
        });
    }
    targets
}

fn installed_rustup_targets() -> Vec<String> {
    let output = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .ok();
    match output {
        Some(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

async fn cargo_check_target(triple: &str) -> Result<(), String> {
    let output = tokio::process::Command::new("cargo")
        .args(["check", "--quiet", "--target", triple])
        .output()
        .await
        .map_err(|err| format!("failed to spawn cargo check: {err}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let summary = stderr
            .lines()
            .rev()
            .find(|line| line.contains("error"))
            .map(|line| line.trim().to_string())
            .unwrap_or_else(|| format!("cargo check --target {triple} failed"));
        Err(summary)
    }
}

fn target_doctor_checks(targets: &[TargetInfo]) -> Vec<DoctorCheck> {
    targets
        .iter()
        .filter(|t| t.checked)
        .map(|t| {
            let status = match t.buildable {
                Some(true) => DoctorStatus::Ok,
                Some(false) => DoctorStatus::Warn,
                None => DoctorStatus::Skip,
            };
            let detail = match t.buildable {
                Some(true) => "cargo check --target succeeded".to_string(),
                Some(false) => t.reasons.first().cloned().unwrap_or_default(),
                None => "not probed".to_string(),
            };
            DoctorCheck {
                id: format!("target:{}", t.triple),
                status,
                label: format!("target:{}", t.triple),
                detail,
                fix_command: if !t.installed {
                    Some(format!("rustup target add {}", t.triple))
                } else {
                    None
                },
                docs_url: Some("https://doc.rust-lang.org/rustc/platform-support.html".to_string()),
                blocks: Vec::new(),
            }
        })
        .collect()
}

async fn collect_providers(check_providers: bool) -> Vec<ProviderInfo> {
    let mut names = llm_config::provider_names();
    names.sort();

    let probes: Vec<_> = names
        .iter()
        .map(|name| collect_provider(name.clone(), check_providers))
        .collect();
    futures::future::join_all(probes).await
}

async fn collect_provider(name: String, check_providers: bool) -> ProviderInfo {
    let configured = harn_vm::llm::provider_auth_status(&name).available;

    // Probing an unconfigured provider would short-circuit inside the
    // healthcheck with a "missing credentials" error and report a 0ms
    // latency — meaningless data that clutters the report. Skip the
    // network round-trip until the provider has credentials.
    if !check_providers || !configured {
        return ProviderInfo {
            name,
            configured,
            reachable: None,
            latency_ms: None,
            errors: Vec::new(),
            probed: false,
        };
    }

    let start = std::time::Instant::now();
    let result = harn_vm::llm::run_provider_healthcheck(&name).await;
    let latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

    let mut errors = Vec::new();
    if !result.valid {
        errors.push(result.message.clone());
    }

    ProviderInfo {
        name,
        configured,
        reachable: Some(result.valid),
        latency_ms: Some(latency_ms),
        errors,
        probed: true,
    }
}

fn provider_doctor_checks(providers: &[ProviderInfo]) -> Vec<DoctorCheck> {
    providers
        .iter()
        .filter(|p| p.probed)
        .map(|p| {
            let status = match p.reachable {
                Some(true) => DoctorStatus::Ok,
                Some(false) => {
                    if p.configured {
                        DoctorStatus::Fail
                    } else {
                        DoctorStatus::Warn
                    }
                }
                None => DoctorStatus::Skip,
            };
            let detail = match (p.reachable, p.latency_ms) {
                (Some(true), Some(ms)) => format!("reachable in {ms}ms"),
                (Some(true), None) => "reachable".to_string(),
                (Some(false), _) => p
                    .errors
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "healthcheck failed".to_string()),
                (None, _) => "not probed".to_string(),
            };
            DoctorCheck {
                id: format!("provider:{}", p.name),
                status,
                label: format!("provider:{}", p.name),
                detail,
                ..Default::default()
            }
        })
        .collect()
}

/// Static enumeration of the stdlib capabilities the runtime gates today.
/// Each entry lists the sandbox profiles that currently permit it on this
/// host. The mapping is conservative: capabilities that don't need any
/// platform-specific OS confinement (`workspace.*`, `network`, `llm.call`,
/// in-process calls) are available across every profile; capabilities
/// that need subprocess execution mark the `OsHardened` profile based on
/// whether the host advertises a working sandbox engine.
fn stdlib_capability_matrix() -> Vec<CapabilityInfo> {
    let in_process = vec![
        "unrestricted".to_string(),
        "worktree".to_string(),
        "os_hardened".to_string(),
        "wasi".to_string(),
    ];
    let mut subprocess = vec!["unrestricted".to_string(), "worktree".to_string()];
    if os_sandbox_available() {
        subprocess.push("os_hardened".to_string());
    }
    subprocess.push("wasi".to_string()); // WASI replay covers subprocess capture

    let mut entries = Vec::new();
    for name in [
        "workspace.read_text",
        "workspace.write_text",
        "workspace.list",
        "workspace.exists",
        "workspace.apply_edit",
        "workspace.delete",
    ] {
        entries.push(CapabilityInfo {
            name: name.to_string(),
            available_in_sandbox_profile: in_process.clone(),
        });
    }
    for name in [
        "network",
        "llm.call",
        "connector.call",
        "agent_state.access",
    ] {
        entries.push(CapabilityInfo {
            name: name.to_string(),
            available_in_sandbox_profile: in_process.clone(),
        });
    }
    for name in ["process.exec", "vision.ocr"] {
        entries.push(CapabilityInfo {
            name: name.to_string(),
            available_in_sandbox_profile: subprocess.clone(),
        });
    }
    entries
}

#[cfg(target_os = "macos")]
fn os_sandbox_available() -> bool {
    which::which("sandbox-exec").is_ok()
}

#[cfg(target_os = "linux")]
fn os_sandbox_available() -> bool {
    // Landlock requires kernel ≥5.13. /sys/kernel/security/lsm enumerates
    // active LSMs — fall back to checking that /proc is accessible because
    // a working seccomp + Landlock stack always exposes one.
    std::path::Path::new("/sys/kernel/security/lsm").exists()
}

#[cfg(target_os = "windows")]
fn os_sandbox_available() -> bool {
    // AppContainer is always present on supported Windows builds. Stub for
    // now; refine once the Windows sandbox engine lands a self-test.
    true
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn os_sandbox_available() -> bool {
    false
}

fn serialize_model_defaults() -> serde_json::Value {
    let cfg = llm_config::load_config();
    serde_json::to_value(&cfg.model_defaults).unwrap_or(serde_json::Value::Null)
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
    let mut any_credential_path = false;
    for name in &providers {
        let Some(def) = llm_config::provider_config(name) else {
            continue;
        };
        let auth = harn_vm::llm::provider_auth_status(name);
        let envs = llm_config::auth_env_names(&def.auth_env);
        let (status, detail, fix_command) = match auth.credential_status {
            harn_vm::llm::ProviderCredentialStatus::Ok => {
                any_credential_path = true;
                (
                    DoctorStatus::Ok,
                    "credential resolved by dispatch".to_string(),
                    None,
                )
            }
            harn_vm::llm::ProviderCredentialStatus::Deferred => {
                any_credential_path = true;
                (
                    DoctorStatus::Ok,
                    "credential resolution deferred to platform provider".to_string(),
                    None,
                )
            }
            harn_vm::llm::ProviderCredentialStatus::NotRequired => {
                (DoctorStatus::Skip, "no key required".to_string(), None)
            }
            harn_vm::llm::ProviderCredentialStatus::Missing => {
                let detail = if envs.is_empty() {
                    "credential unavailable".to_string()
                } else {
                    format!("missing: {}", envs.join(", "))
                };
                let fix = envs.first().map(|env| format!("export {env}=…"));
                (DoctorStatus::Warn, detail, fix)
            }
        };
        checks.push(DoctorCheck {
            id: format!("creds:{name}"),
            status,
            label: format!("creds:{name}"),
            detail,
            fix_command,
            docs_url: Some("https://harnlang.com/docs/llm/providers.html".to_string()),
            blocks: Vec::new(),
        });
    }

    // Add an aggregate row that fails only when no provider has creds AND
    // ollama appears unreachable. Reachability is best-effort: we only flag
    // FAIL when the synchronous `ollama --version` probe errors. Otherwise
    // demote to WARN so users without local models still get a softer signal.
    let ollama_present = which::which("ollama").is_ok();
    let aggregate_status = if any_credential_path {
        DoctorStatus::Ok
    } else if ollama_present {
        DoctorStatus::Warn
    } else {
        DoctorStatus::Fail
    };
    let aggregate_detail = if any_credential_path {
        "at least one provider credential path is available".to_string()
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
        gpu,
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
    let cwd = std::env::current_dir().unwrap_or_default();
    check_manifest_from(&cwd).await
}

async fn check_manifest_from(anchor: &Path) -> Vec<DoctorCheck> {
    let Some(path) = find_nearest_manifest(anchor) else {
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
        build_host_info, build_summary, check_event_log, check_hardware, check_manifest_from,
        check_ollama, check_platform_capabilities, find_nearest_manifest, format_trigger_metrics,
        read_manifest, stdlib_capability_matrix, target_doctor_checks, DoctorCheck, DoctorReport,
        DoctorStatus, HardwareSnapshot, TargetInfo, DOCTOR_SCHEMA_VERSION,
    };
    use crate::json_envelope::JsonOutput;
    use harn_vm::llm_config::{AuthEnv, HealthcheckDef, ProviderDef};

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

        let checks = check_manifest_from(dir.path()).await;

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
    fn report_envelope_carries_capability_matrix_and_stable_ids() {
        let checks = vec![
            check("harn_version", DoctorStatus::Ok),
            check("creds:openai", DoctorStatus::Warn),
        ];
        let report = DoctorReport {
            host: build_host_info(&[]),
            providers_config_path: String::new(),
            model_defaults: serde_json::Value::Null,
            targets: vec![TargetInfo {
                triple: "x86_64-unknown-linux-gnu".to_string(),
                installed: true,
                buildable: Some(true),
                reasons: Vec::new(),
                checked: true,
            }],
            providers: vec![super::ProviderInfo {
                name: "anthropic".to_string(),
                configured: true,
                reachable: Some(true),
                latency_ms: Some(120),
                errors: Vec::new(),
                probed: true,
            }],
            capabilities: stdlib_capability_matrix(),
            checks: checks.iter().map(super::DoctorCheckJson::from).collect(),
            hardware: HardwareSnapshot {
                ram_gb: Some(16),
                gpu: "mps".to_string(),
                free_disk_gb: Some(100),
            },
            summary: build_summary(&checks),
            next_step: "test next step".to_string(),
        };
        let envelope = report.into_envelope();
        let value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(value["schemaVersion"], DOCTOR_SCHEMA_VERSION);
        assert_eq!(value["ok"], true);
        let data = &value["data"];
        assert!(data["host"]["os"].is_string());
        assert!(data["host"]["arch"].is_string());
        assert_eq!(data["targets"][0]["triple"], "x86_64-unknown-linux-gnu");
        assert_eq!(data["targets"][0]["buildable"], true);
        assert_eq!(data["providers"][0]["name"], "anthropic");
        assert_eq!(data["providers"][0]["reachable"], true);
        assert_eq!(data["providers"][0]["latency_ms"], 120);
        assert!(!data["capabilities"].as_array().unwrap().is_empty());
        let checks_arr = data["checks"].as_array().expect("checks array");
        assert_eq!(checks_arr[0]["id"], "harn_version");
        assert_eq!(checks_arr[0]["status"], "ok");
        assert_eq!(checks_arr[1]["id"], "creds:openai");
        assert_eq!(checks_arr[1]["status"], "warn");
        assert_eq!(data["hardware"]["ram_gb"], 16);
        assert_eq!(data["hardware"]["gpu"], "mps");
        assert_eq!(data["next_step"], "test next step");
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
    fn summary_aggregates_status_counts_and_blocked_flows() {
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
        let summary = build_summary(&checks);
        assert_eq!(summary.ok, 1);
        assert_eq!(summary.warning, 1);
        assert_eq!(summary.blocking, 2);
        assert_eq!(summary.skip, 1);
        // Sorted, deduplicated, alphabetical.
        assert_eq!(summary.blocked_flows, vec!["build", "portal", "test"]);
    }

    #[test]
    fn capability_matrix_lists_known_capabilities() {
        let entries = stdlib_capability_matrix();
        let names: std::collections::BTreeSet<&str> =
            entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains("workspace.read_text"), "names: {names:?}");
        assert!(names.contains("network"), "names: {names:?}");
        assert!(names.contains("process.exec"), "names: {names:?}");
        for entry in &entries {
            assert!(
                !entry.available_in_sandbox_profile.is_empty(),
                "{} should list at least one sandbox profile",
                entry.name
            );
            for profile in &entry.available_in_sandbox_profile {
                assert!(
                    ["unrestricted", "worktree", "os_hardened", "wasi"].contains(&profile.as_str()),
                    "unknown sandbox profile '{profile}'"
                );
            }
        }
    }

    #[test]
    fn host_info_reports_os_arch_and_harn_version() {
        let info = build_host_info(&[]);
        assert_eq!(info.os, std::env::consts::OS);
        assert_eq!(info.arch, std::env::consts::ARCH);
        assert_eq!(info.harn_version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn target_checks_skipped_when_not_probed() {
        let targets = vec![
            TargetInfo {
                triple: "x86_64-apple-darwin".to_string(),
                installed: true,
                buildable: None,
                reasons: Vec::new(),
                checked: false,
            },
            TargetInfo {
                triple: "wasm32-unknown-unknown".to_string(),
                installed: false,
                buildable: Some(false),
                reasons: vec!["target not installed".to_string()],
                checked: true,
            },
        ];
        let checks = target_doctor_checks(&targets);
        // Only the probed target emits a check entry.
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].id, "target:wasm32-unknown-unknown");
        assert_eq!(checks[0].status, DoctorStatus::Warn);
        assert!(checks[0]
            .fix_command
            .as_deref()
            .map(|s| s.contains("rustup target add"))
            .unwrap_or(false));
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
