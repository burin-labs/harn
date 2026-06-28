//! `harn local launch` — bring a local model up through Harn's catalog.
//!
//! Ollama is daemon-managed, so launch means "load/warm this model through
//! the API." llama.cpp and MLX are managed processes, so launch spawns the
//! configured server command, records its PID, waits for `/v1/models`, and
//! lets `harn local stop` own cleanup.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use harn_vm::llm::readiness::probe_provider_readiness;
use harn_vm::llm_config::{self, LocalMemoryDef, LocalRuntimeDef};
use serde::Serialize;
use time::{format_description, OffsetDateTime};

use crate::cli::LocalLaunchArgs;
use crate::commands::hardware::{collect_hardware_snapshot, HardwareSnapshot};

use super::profile::defaults_for;
use super::runtime::{local_provider_ids, port_from_base_url, resolve_provider_def};
use super::state::{
    ensure_state_dir, write_pid_record, write_selection, LocalSelection, PidRecord,
};
use super::switch::{evict_siblings, warm_ollama};

const BYTES_PER_GIB: f64 = 1024.0 * 1024.0 * 1024.0;

#[derive(Debug, Serialize)]
struct LaunchResult {
    provider: String,
    model: String,
    alias: Option<String>,
    base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    command: String,
    args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    log_path: Option<String>,
    readiness: serde_json::Value,
    rechecked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    memory_plan: Option<LaunchMemoryPlan>,
}

#[derive(Debug, Clone, Serialize)]
struct LaunchMemoryPlan {
    status: String,
    requested_context: u64,
    estimated_resident_gib: f64,
    safety_margin_gib: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    available_gib: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_gib: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_recommended_context: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suggested_context: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_type_k: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_type_v: Option<String>,
    cache_multiplier: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_verified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

struct ManagedLaunchPlan {
    provider: String,
    runtime: LocalRuntimeDef,
    base_url: String,
    host: String,
    port: u16,
    ctx: u64,
    memory_plan: Option<LaunchMemoryPlan>,
}

pub(crate) async fn run(args: LocalLaunchArgs, base_dir: &Path) -> Result<(), String> {
    let resolved = llm_config::resolve_model_info(&args.model);
    let provider = args
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| resolved.provider.clone());
    if !local_provider_ids(None).contains(&provider) {
        return Err(format!(
            "'{provider}' is not a local provider Harn manages (expected one of: {})",
            local_provider_ids(None).join(", ")
        ));
    }

    if !args.no_evict {
        let _ = evict_siblings(&provider, &resolved.id, base_dir).await;
    }

    let def = resolve_provider_def(&provider)?;
    let runtime = def.local_runtime.clone().ok_or_else(|| {
        format!("provider '{provider}' has no [providers.{provider}.local_runtime] catalog row")
    })?;
    let catalog_url = llm_config::resolve_base_url(&def);
    let port = args
        .port
        .or_else(|| port_from_base_url(&catalog_url))
        .or(runtime.default_port)
        .unwrap_or(8000);
    let host = args
        .host
        .clone()
        .or_else(|| host_from_base_url(&catalog_url))
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let base_url = format!("http://{host}:{port}");
    let hardware = collect_hardware_snapshot();
    let defaults = defaults_for(&hardware);
    let ctx = args.ctx.unwrap_or(defaults.ctx);
    let keep_alive = args
        .keep_alive
        .clone()
        .unwrap_or_else(|| defaults.keep_alive.to_string());
    let memory_plan = llm_config::model_catalog_entry(&resolved.id)
        .and_then(|model| model.local_memory)
        .map(|memory| launch_memory_plan(&resolved.id, &memory, ctx, &args, &hardware))
        .transpose()?;

    if runtime.kind.as_deref() == Some("daemon_api") || provider == "ollama" {
        return launch_daemon(
            args,
            resolved,
            provider,
            base_url,
            ctx,
            keep_alive,
            memory_plan,
            base_dir,
        )
        .await;
    }

    let plan = ManagedLaunchPlan {
        provider,
        runtime,
        base_url,
        host,
        port,
        ctx,
        memory_plan,
    };
    launch_managed_process(args, resolved, plan, base_dir).await
}

async fn launch_daemon(
    args: LocalLaunchArgs,
    resolved: llm_config::ResolvedModel,
    provider: String,
    base_url: String,
    ctx: u64,
    keep_alive: String,
    memory_plan: Option<LaunchMemoryPlan>,
    base_dir: &Path,
) -> Result<(), String> {
    if provider != "ollama" {
        return Err(format!(
            "provider '{provider}' is marked daemon_api, but only Ollama warmup is implemented"
        ));
    }
    let (readiness, rechecked) =
        warm_ollama(&resolved.id, &base_url, ctx, &keep_alive, args.no_pull).await?;
    let selection = LocalSelection::now(
        provider.clone(),
        resolved.id.clone(),
        resolved.alias.clone(),
        base_url.clone(),
        Some(ctx),
        Some(keep_alive),
    );
    write_selection(base_dir, &selection)?;
    let result = LaunchResult {
        provider,
        model: resolved.id,
        alias: resolved.alias,
        base_url,
        pid: None,
        command: "ollama-api".to_string(),
        args: Vec::new(),
        log_path: None,
        readiness,
        rechecked,
        memory_plan,
    };
    render_result(&result, args.json)
}

async fn launch_managed_process(
    args: LocalLaunchArgs,
    resolved: llm_config::ResolvedModel,
    plan: ManagedLaunchPlan,
    base_dir: &Path,
) -> Result<(), String> {
    if plan.runtime.kind.as_deref() != Some("managed_process") {
        return Err(format!(
            "provider '{}' local runtime kind '{}' is not launchable",
            plan.provider,
            plan.runtime.kind.as_deref().unwrap_or("(unset)")
        ));
    }
    let model_source = resolve_model_source(&args, &plan.runtime, &plan.provider, &resolved.id)?;
    let command = args
        .server_command
        .clone()
        .or_else(|| plan.runtime.command.clone())
        .ok_or_else(|| {
            format!(
                "provider '{}' local_runtime is missing a command field",
                plan.provider
            )
        })?;
    let launch_args = build_managed_args(
        &args,
        &plan.runtime,
        &model_source,
        &resolved.id,
        &plan.host,
        plan.port,
        plan.ctx,
    );
    let log_path = args
        .log
        .clone()
        .unwrap_or(default_log_path(base_dir, &plan.provider)?);
    let mut child = spawn_managed_process(&command, &launch_args, &log_path)?;
    let pid = child.id();

    let record = PidRecord {
        provider: plan.provider.clone(),
        pid,
        model: resolved.id.clone(),
        base_url: plan.base_url.clone(),
        command: command.clone(),
        args: launch_args.clone(),
        started_at: now_rfc3339(),
    };
    write_pid_record(base_dir, &record)?;

    let (readiness, rechecked) = wait_for_readiness(
        &mut child,
        &plan.provider,
        &resolved.id,
        &plan.base_url,
        args.timeout_secs,
    )
    .await?;

    let selection = LocalSelection::now(
        plan.provider.clone(),
        resolved.id.clone(),
        resolved.alias.clone(),
        plan.base_url.clone(),
        Some(plan.ctx),
        None,
    );
    write_selection(base_dir, &selection)?;

    let result = LaunchResult {
        provider: plan.provider,
        model: resolved.id,
        alias: resolved.alias,
        base_url: plan.base_url,
        pid: Some(pid),
        command,
        args: launch_args,
        log_path: Some(log_path.display().to_string()),
        readiness,
        rechecked,
        memory_plan: plan.memory_plan,
    };
    render_result(&result, args.json)
}

fn launch_memory_plan(
    model_id: &str,
    memory: &LocalMemoryDef,
    ctx: u64,
    args: &LocalLaunchArgs,
    hardware: &HardwareSnapshot,
) -> Result<LaunchMemoryPlan, String> {
    let base_gib = memory
        .base_resident_gib
        .or(memory.measured_resident_gib)
        .unwrap_or(0.0);
    let kv_per_1k = memory.kv_cache_gib_per_1k_ctx.unwrap_or(0.0);
    let default_cache = memory
        .default_cache_type
        .as_deref()
        .or(memory.measured_cache_type.as_deref());
    let cache_type_k = args
        .cache_type_k
        .as_deref()
        .or(default_cache)
        .map(str::to_string);
    let cache_type_v = args
        .cache_type_v
        .as_deref()
        .or(args.cache_type_k.as_deref())
        .or(default_cache)
        .map(str::to_string);
    let (cache_multiplier, mut warnings) =
        averaged_cache_multiplier(memory, cache_type_k.as_deref(), cache_type_v.as_deref());
    let estimate = base_gib + (ctx as f64 / 1000.0) * kv_per_1k * cache_multiplier;
    let safety_margin = memory.safety_margin_gib.unwrap_or(4.0);
    let available_gib = hardware.ram.available_bytes.map(bytes_to_gib_f64);
    let total_gib = hardware.ram.total_bytes.map(bytes_to_gib_f64);
    let suggested_context =
        suggested_context(memory, available_gib, base_gib, kv_per_1k, cache_multiplier);
    if let Some(max_ctx) = memory.max_recommended_context {
        if ctx > max_ctx {
            warnings.push(format!(
                "requested context {ctx} exceeds catalog max_recommended_context {max_ctx}"
            ));
        }
    }
    let risk = available_gib.is_some_and(|available| estimate + safety_margin > available);
    let status = if risk && args.allow_memory_risk {
        "overridden"
    } else if risk {
        "risk"
    } else if available_gib.is_some() {
        "ok"
    } else {
        "unknown_available_ram"
    };
    let plan = LaunchMemoryPlan {
        status: status.to_string(),
        requested_context: ctx,
        estimated_resident_gib: round_gib(estimate),
        safety_margin_gib: round_gib(safety_margin),
        available_gib: available_gib.map(round_gib),
        total_gib: total_gib.map(round_gib),
        max_recommended_context: memory.max_recommended_context,
        suggested_context,
        cache_type_k,
        cache_type_v,
        cache_multiplier: round_gib(cache_multiplier),
        last_verified: memory.last_verified.clone(),
        notes: memory.notes.clone(),
        warnings,
    };
    if risk && !args.allow_memory_risk {
        let available = available_gib.unwrap_or_default();
        let suggestion = suggested_context
            .map(|ctx| format!(" Try `--ctx {ctx}`."))
            .unwrap_or_default();
        return Err(format!(
            "launching {model_id} at ctx {ctx} estimates {:.1} GiB resident plus {:.1} GiB safety margin, but only {:.1} GiB is currently available.{suggestion} Unload other local models or pass `--allow-memory-risk` to override.",
            plan.estimated_resident_gib, plan.safety_margin_gib, available
        ));
    }
    Ok(plan)
}

fn averaged_cache_multiplier(
    memory: &LocalMemoryDef,
    cache_type_k: Option<&str>,
    cache_type_v: Option<&str>,
) -> (f64, Vec<String>) {
    let mut warnings = Vec::new();
    let k = cache_multiplier(memory, cache_type_k, &mut warnings);
    let v = cache_multiplier(memory, cache_type_v.or(cache_type_k), &mut warnings);
    (k.midpoint(v), warnings)
}

fn cache_multiplier(
    memory: &LocalMemoryDef,
    cache_type: Option<&str>,
    warnings: &mut Vec<String>,
) -> f64 {
    let Some(cache_type) = cache_type else {
        return 1.0;
    };
    if let Some(value) = memory.cache_type_multipliers.get(cache_type) {
        return *value;
    }
    if !memory.cache_type_multipliers.is_empty() {
        warnings.push(format!(
            "cache type {cache_type} has no catalog multiplier; using 1.0"
        ));
    }
    1.0
}

fn suggested_context(
    memory: &LocalMemoryDef,
    available_gib: Option<f64>,
    base_gib: f64,
    kv_per_1k: f64,
    cache_multiplier: f64,
) -> Option<u64> {
    let available_gib = available_gib?;
    if kv_per_1k <= 0.0 || cache_multiplier <= 0.0 {
        return memory.max_recommended_context;
    }
    let margin = memory.safety_margin_gib.unwrap_or(4.0);
    let budget = available_gib - margin - base_gib;
    if budget <= 0.0 {
        return Some(1024);
    }
    let mut ctx = ((budget / (kv_per_1k * cache_multiplier)) * 1000.0).floor() as u64;
    if let Some(max_ctx) = memory.max_recommended_context {
        ctx = ctx.min(max_ctx);
    }
    Some(ctx.max(1024))
}

fn bytes_to_gib_f64(bytes: u64) -> f64 {
    bytes as f64 / BYTES_PER_GIB
}

fn round_gib(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn resolve_model_source(
    args: &LocalLaunchArgs,
    runtime: &LocalRuntimeDef,
    provider: &str,
    model_id: &str,
) -> Result<String, String> {
    if let Some(source) = args
        .model_source
        .as_deref()
        .map(str::trim)
        .filter(|source| !source.is_empty())
    {
        return Ok(expand_home(source));
    }
    if let Some(env_name) = runtime.model_source_env.as_deref() {
        if let Ok(value) = std::env::var(env_name) {
            let value = value.trim();
            if !value.is_empty() {
                return Ok(expand_home(value));
            }
        }
    }
    if let Some(source) = runtime.model_source.as_deref() {
        let source = source.trim();
        if !source.is_empty() {
            return Ok(expand_home(source));
        }
    }
    if provider == "mlx" {
        return Ok(model_id.to_string());
    }
    Err(format!(
        "provider '{provider}' needs a model source. Pass `--model-source <path-or-repo>` \
         or set {} in your environment/provider overlay.",
        runtime
            .model_source_env
            .as_deref()
            .unwrap_or("a provider local_runtime.model_source")
    ))
}

fn build_managed_args(
    args: &LocalLaunchArgs,
    runtime: &LocalRuntimeDef,
    model_source: &str,
    model_id: &str,
    host: &str,
    port: u16,
    ctx: u64,
) -> Vec<String> {
    let mut out = Vec::new();
    push_arg(&mut out, runtime.model_arg.as_deref(), model_source);
    push_arg(&mut out, runtime.served_model_arg.as_deref(), model_id);
    push_arg(&mut out, runtime.host_arg.as_deref(), host);
    push_arg(&mut out, runtime.port_arg.as_deref(), port.to_string());
    push_arg(&mut out, runtime.ctx_arg.as_deref(), ctx.to_string());
    push_arg(
        &mut out,
        runtime.parallel_arg.as_deref(),
        args.parallel.to_string(),
    );

    // Apply the explicit dedicated flags first so they win over `default_args`.
    // We track which flag keys the user set explicitly, then fold in only the
    // `default_args` entries that the user did not already override — otherwise
    // a flag like `--jinja` or `--flash-attn` (present in both `default_args`
    // and as a dedicated CLI flag) lands in the launch argv twice.
    let mut explicit = Vec::new();
    push_arg(
        &mut explicit,
        runtime.gpu_layers_arg.as_deref(),
        args.gpu_layers.clone(),
    );
    if let Some(value) = args.cache_type_k.as_deref() {
        push_arg(&mut explicit, runtime.cache_type_k_arg.as_deref(), value);
    }
    if let Some(value) = args.cache_type_v.as_deref() {
        push_arg(&mut explicit, runtime.cache_type_v_arg.as_deref(), value);
    }
    if let Some(value) = args.cache_ram {
        push_arg(
            &mut explicit,
            runtime.cache_ram_arg.as_deref(),
            value.to_string(),
        );
    }
    if args.jinja {
        explicit.push("--jinja".to_string());
    }
    if let Some(value) = args.reasoning.as_deref() {
        explicit.extend(["--reasoning".to_string(), value.to_string()]);
    }
    if let Some(value) = args.reasoning_format.as_deref() {
        explicit.extend(["--reasoning-format".to_string(), value.to_string()]);
    }
    if let Some(value) = args.flash_attn.as_deref() {
        explicit.extend(["--flash-attn".to_string(), value.to_string()]);
    }
    if args.metrics {
        explicit.push("--metrics".to_string());
    }

    let explicit_keys: std::collections::HashSet<&str> = explicit
        .iter()
        .filter(|a| a.starts_with("--"))
        .map(String::as_str)
        .collect();
    out.extend(filter_default_args(&runtime.default_args, &explicit_keys));
    out.extend(explicit);
    out.extend(args.server_args.iter().cloned());
    out
}

/// Drop any `--flag` (and its following value, if any) from `default_args`
/// whose key already appears in `explicit_keys`, so a flag the caller set
/// explicitly is not also pulled in from the runtime defaults.
fn filter_default_args(
    default_args: &[String],
    explicit_keys: &std::collections::HashSet<&str>,
) -> Vec<String> {
    let mut kept = Vec::with_capacity(default_args.len());
    let mut iter = default_args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg.starts_with("--") && explicit_keys.contains(default_arg_key(arg)) {
            // Skip this flag, and its value too when the next token is not
            // itself a flag (covers valued flags like `--flash-attn on`).
            // `--flag=value` carries its value inline, so only that one token
            // is skipped.
            if !arg.contains('=') {
                if let Some(next) = iter.peek() {
                    if !next.starts_with("--") {
                        iter.next();
                    }
                }
            }
            continue;
        }
        kept.push(arg.clone());
    }
    kept
}

fn default_arg_key(arg: &str) -> &str {
    arg.split_once('=').map(|(key, _)| key).unwrap_or(arg)
}

fn push_arg(out: &mut Vec<String>, key: Option<&str>, value: impl ToString) {
    let Some(key) = key else {
        return;
    };
    if key.trim().is_empty() {
        return;
    }
    out.push(key.to_string());
    out.push(value.to_string());
}

fn spawn_managed_process(
    command: &str,
    args: &[String],
    log_path: &Path,
) -> Result<std::process::Child, String> {
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|error| format!("failed to open {}: {error}", log_path.display()))?;
    let log_err = log
        .try_clone()
        .map_err(|error| format!("failed to clone log handle {}: {error}", log_path.display()))?;
    Command::new(command)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .spawn()
        .map_err(|error| format!("failed to spawn {command}: {error}"))
}

async fn wait_for_readiness(
    child: &mut std::process::Child,
    provider: &str,
    model: &str,
    base_url: &str,
    timeout_secs: u64,
) -> Result<(serde_json::Value, bool), String> {
    let mut last = None;
    let attempts = timeout_secs.max(1) * 2;
    for _ in 0..attempts {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("failed to inspect launched process: {error}"))?
        {
            return Err(format!(
                "launched {provider} server exited before readiness ({status})"
            ));
        }
        let probe = probe_provider_readiness(provider, Some(model), Some(base_url)).await;
        if probe.ok {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let second = probe_provider_readiness(provider, Some(model), Some(base_url)).await;
            return Ok((
                serde_json::to_value(&second).map_err(|error| error.to_string())?,
                true,
            ));
        }
        last = Some(probe);
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let detail = last
        .map(|probe| probe.message)
        .unwrap_or_else(|| "no readiness probe completed".to_string());
    Err(format!(
        "{provider} did not become ready at {base_url} within {timeout_secs}s: {detail}"
    ))
}

fn render_result(result: &LaunchResult, json: bool) -> Result<(), String> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(result)
                .map_err(|error| format!("failed to render launch JSON: {error}"))?
        );
        return Ok(());
    }
    if let Some(pid) = result.pid {
        println!(
            "Launched {} via {} at {} (pid {})",
            result.model, result.provider, result.base_url, pid
        );
    } else {
        println!(
            "Loaded {} via {} at {}",
            result.model, result.provider, result.base_url
        );
    }
    if let Some(path) = result.log_path.as_deref() {
        println!("  log: {path}");
    }
    if result.rechecked {
        println!("  readiness re-checked after launch");
    }
    Ok(())
}

fn default_log_path(base_dir: &Path, provider: &str) -> Result<PathBuf, String> {
    let dir = ensure_state_dir(base_dir)?.join("logs");
    let stamp = OffsetDateTime::now_utc()
        .format(
            &format_description::parse("[year][month][day]T[hour][minute][second]Z")
                .map_err(|error| format!("failed to build timestamp format: {error}"))?,
        )
        .unwrap_or_else(|_| "unknown-time".to_string());
    Ok(dir.join(format!("{provider}-{stamp}.log")))
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| String::new())
}

fn host_from_base_url(base_url: &str) -> Option<String> {
    reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
}

fn expand_home(value: &str) -> String {
    let Some(home) = std::env::var_os("HOME") else {
        return value.to_string();
    };
    let home = PathBuf::from(home);
    if value == "~" {
        return home.display().to_string();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home.join(rest).display().to_string();
    }
    if let Some(rest) = value.strip_prefix("$HOME/") {
        return home.join(rest).display().to_string();
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli_args() -> LocalLaunchArgs {
        LocalLaunchArgs {
            model: "local-qwen3.6".to_string(),
            provider: Some("llamacpp".to_string()),
            model_source: Some("/models/qwen.gguf".to_string()),
            server_command: None,
            host: Some("127.0.0.1".to_string()),
            port: Some(8001),
            ctx: Some(8192),
            keep_alive: None,
            no_pull: false,
            parallel: 1,
            gpu_layers: "auto".to_string(),
            cache_type_k: Some("q8_0".to_string()),
            cache_type_v: Some("q8_0".to_string()),
            cache_ram: Some(0),
            reasoning: Some("off".to_string()),
            reasoning_format: Some("deepseek".to_string()),
            flash_attn: Some("on".to_string()),
            jinja: true,
            metrics: true,
            server_args: vec!["--no-warmup".to_string()],
            timeout_secs: 1,
            log: None,
            no_evict: false,
            allow_memory_risk: false,
            json: true,
        }
    }

    fn runtime() -> LocalRuntimeDef {
        LocalRuntimeDef {
            kind: Some("managed_process".to_string()),
            command: Some("llama-server".to_string()),
            model_arg: Some("--model".to_string()),
            served_model_arg: Some("--alias".to_string()),
            host_arg: Some("--host".to_string()),
            port_arg: Some("--port".to_string()),
            ctx_arg: Some("--ctx-size".to_string()),
            parallel_arg: Some("--parallel".to_string()),
            gpu_layers_arg: Some("--n-gpu-layers".to_string()),
            cache_type_k_arg: Some("--cache-type-k".to_string()),
            cache_type_v_arg: Some("--cache-type-v".to_string()),
            cache_ram_arg: Some("--cache-ram".to_string()),
            default_args: vec!["--jinja".to_string()],
            ..LocalRuntimeDef::default()
        }
    }

    #[test]
    fn build_managed_args_uses_runtime_shape_and_passthrough_flags() {
        let built = build_managed_args(
            &cli_args(),
            &runtime(),
            "/models/qwen.gguf",
            "qwen3.6-35b-a3b-ud-q4-k-xl",
            "127.0.0.1",
            8001,
            8192,
        );
        assert!(built
            .windows(2)
            .any(|pair| pair == ["--model", "/models/qwen.gguf"]));
        assert!(built
            .windows(2)
            .any(|pair| pair == ["--alias", "qwen3.6-35b-a3b-ud-q4-k-xl"]));
        assert!(built.windows(2).any(|pair| pair == ["--reasoning", "off"]));
        assert!(built.contains(&"--jinja".to_string()));
        assert!(built.contains(&"--metrics".to_string()));
        assert!(built.contains(&"--no-warmup".to_string()));
        // `--jinja` is in both `default_args` and set as a dedicated flag, but
        // must land in the argv exactly once.
        assert_eq!(
            built.iter().filter(|a| *a == "--jinja").count(),
            1,
            "--jinja must not be duplicated: {built:?}"
        );
    }

    #[test]
    fn build_managed_args_dedups_flags_shared_with_default_args() {
        // Mirror the production llamacpp `default_args`, which carry `--jinja`,
        // `--reasoning off`, `--reasoning-format deepseek`, `--metrics`, and
        // `--flash-attn on`. The caller also sets all of these as dedicated
        // flags; none may appear twice, and the explicit value must win.
        let mut runtime = runtime();
        runtime.default_args = vec![
            "--jinja".to_string(),
            "--reasoning".to_string(),
            "off".to_string(),
            "--reasoning-format".to_string(),
            "deepseek".to_string(),
            "--metrics".to_string(),
            "--flash-attn".to_string(),
            "on".to_string(),
        ];
        let mut args = cli_args();
        // Explicit value differs from the default, to prove the explicit wins.
        args.flash_attn = Some("auto".to_string());

        let built = build_managed_args(
            &args,
            &runtime,
            "/models/qwen.gguf",
            "qwen3.6-35b-a3b-ud-q4-k-xl",
            "127.0.0.1",
            8001,
            8192,
        );

        for flag in [
            "--jinja",
            "--reasoning",
            "--reasoning-format",
            "--metrics",
            "--flash-attn",
        ] {
            assert_eq!(
                built.iter().filter(|a| *a == flag).count(),
                1,
                "{flag} must appear exactly once: {built:?}"
            );
        }
        // The default `--flash-attn on` is dropped; the explicit `auto` remains.
        assert!(built
            .windows(2)
            .any(|pair| pair == ["--flash-attn", "auto"]));
        assert!(!built.windows(2).any(|pair| pair == ["--flash-attn", "on"]));
    }

    #[test]
    fn build_managed_args_dedups_equals_form_default_args() {
        let mut runtime = runtime();
        runtime.default_args = vec![
            "--flash-attn=off".to_string(),
            "--reasoning-format=none".to_string(),
            "--jinja".to_string(),
        ];
        let mut args = cli_args();
        args.flash_attn = Some("auto".to_string());
        args.reasoning_format = Some("deepseek".to_string());

        let built = build_managed_args(
            &args,
            &runtime,
            "/models/qwen.gguf",
            "qwen3.6-35b-a3b-ud-q4-k-xl",
            "127.0.0.1",
            8001,
            8192,
        );

        assert!(!built.iter().any(|arg| arg == "--flash-attn=off"));
        assert!(!built.iter().any(|arg| arg == "--reasoning-format=none"));
        assert!(built
            .windows(2)
            .any(|pair| pair == ["--flash-attn", "auto"]));
        assert!(built
            .windows(2)
            .any(|pair| pair == ["--reasoning-format", "deepseek"]));
    }

    #[test]
    fn mlx_can_default_model_source_to_model_id() {
        let source =
            resolve_model_source(&cli_args(), &LocalRuntimeDef::default(), "mlx", "repo/id")
                .expect("mlx default");
        assert_eq!(source, "/models/qwen.gguf");

        let mut args = cli_args();
        args.model_source = None;
        let source = resolve_model_source(&args, &LocalRuntimeDef::default(), "mlx", "repo/id")
            .expect("mlx model id");
        assert_eq!(source, "repo/id");
    }

    #[test]
    fn memory_plan_blocks_obvious_overcommit() {
        let args = cli_args();
        let hardware = hardware_with_available_gib(24);
        let memory = LocalMemoryDef {
            base_resident_gib: Some(20.0),
            kv_cache_gib_per_1k_ctx: Some(0.25),
            safety_margin_gib: Some(4.0),
            max_recommended_context: Some(65_536),
            ..LocalMemoryDef::default()
        };
        let error = launch_memory_plan("qwen", &memory, 65_536, &args, &hardware)
            .expect_err("memory pressure should block");
        assert!(error.contains("--allow-memory-risk"));
        assert!(error.contains("--ctx"));
    }

    #[test]
    fn memory_plan_can_be_overridden() {
        let mut args = cli_args();
        args.allow_memory_risk = true;
        let hardware = hardware_with_available_gib(24);
        let memory = LocalMemoryDef {
            base_resident_gib: Some(20.0),
            kv_cache_gib_per_1k_ctx: Some(0.25),
            safety_margin_gib: Some(4.0),
            ..LocalMemoryDef::default()
        };
        let plan = launch_memory_plan("qwen", &memory, 65_536, &args, &hardware)
            .expect("override should pass");
        assert_eq!(plan.status, "overridden");
    }

    #[test]
    fn memory_plan_scales_by_cache_type() {
        let args = cli_args();
        let hardware = hardware_with_available_gib(64);
        let memory = LocalMemoryDef {
            base_resident_gib: Some(20.0),
            kv_cache_gib_per_1k_ctx: Some(0.2),
            default_cache_type: Some("q8_0".to_string()),
            cache_type_multipliers: [
                ("q8_0".to_string(), 1.0),
                ("f16".to_string(), 2.0),
                ("q4_0".to_string(), 0.5),
            ]
            .into_iter()
            .collect(),
            ..LocalMemoryDef::default()
        };
        let plan =
            launch_memory_plan("qwen", &memory, 10_000, &args, &hardware).expect("memory plan");
        assert_eq!(plan.cache_multiplier, 1.0);
        assert_eq!(plan.estimated_resident_gib, 22.0);
    }

    fn hardware_with_available_gib(available_gib: u64) -> HardwareSnapshot {
        use crate::commands::hardware::{DiskSnapshot, GpuKind, GpuSnapshot, RamSnapshot};

        HardwareSnapshot {
            ram: RamSnapshot {
                total_bytes: Some(128 * 1024 * 1024 * 1024),
                available_bytes: Some(available_gib * 1024 * 1024 * 1024),
            },
            gpu: GpuSnapshot {
                kind: GpuKind::None,
                total_memory_bytes: None,
                free_memory_bytes: None,
            },
            disk: DiskSnapshot {
                path: PathBuf::from("."),
                free_bytes: None,
            },
        }
    }
}
