use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;

use harn_vm::event_log::{AnyEventLog, EventLog};

use crate::cli::{
    PersonaCheckArgs, PersonaControlArgs, PersonaInspectArgs, PersonaListArgs, PersonaSpendArgs,
    PersonaStatusArgs, PersonaTickArgs, PersonaTriggerArgs,
};
use crate::package::{self, PersonaManifestEntry, PersonaValidationError, ResolvedPersonaManifest};

/// In-process variant of `harn persona list --json` used by the binary's
/// dispatcher and by integration tests that want to assert on the
/// structured payload without spawning a subprocess.
pub fn list_payload(manifest: Option<&Path>) -> Result<Vec<serde_json::Value>, String> {
    let catalog = load_catalog_result(manifest)?;
    Ok(catalog
        .personas
        .iter()
        .map(|persona| persona_to_json(persona, &catalog))
        .collect())
}

pub(crate) fn run_list(manifest: Option<&Path>, args: &PersonaListArgs) {
    if args.json {
        let personas = list_payload(manifest).unwrap_or_else(|error| fatal(&error));
        println!(
            "{}",
            serde_json::to_string_pretty(&personas)
                .unwrap_or_else(|error| fatal(&format!("failed to serialize personas: {error}")))
        );
        return;
    }

    let catalog = load_catalog_or_exit(manifest);
    if catalog.personas.is_empty() {
        println!(
            "No personas declared in {}.",
            catalog.manifest_path.display()
        );
        return;
    }

    println!("Personas in {}:", catalog.manifest_path.display());
    let name_width = catalog
        .personas
        .iter()
        .filter_map(|persona| persona.name.as_ref())
        .map(String::len)
        .max()
        .unwrap_or(4);
    for persona in &catalog.personas {
        let name = persona.name.as_deref().unwrap_or("<unnamed>");
        let tier = persona
            .autonomy_tier
            .map(|tier| tier.as_str())
            .unwrap_or("<missing>");
        let receipts = persona
            .receipt_policy
            .map(|policy| policy.as_str())
            .unwrap_or("<missing>");
        let entry = persona.entry_workflow.as_deref().unwrap_or("<missing>");
        println!(
            "  {name:<name_width$}  tier={tier:<17} receipts={receipts:<8} entry={entry}",
            name_width = name_width
        );
    }
}

/// In-process variant of `harn persona check --json`. Returns the JSON
/// payload the CLI would print on success; structured validation errors
/// surface in `Err` so callers can format or assert on them.
pub fn check_payload(
    path: Option<&Path>,
) -> Result<serde_json::Value, Vec<PersonaValidationError>> {
    let catalog = load_catalog_validation(path)?;
    Ok(serde_json::json!({
        "ok": true,
        "manifest_path": catalog.manifest_path,
        "personas": catalog.personas.iter().map(|persona| {
            serde_json::json!({
                "name": persona.name.as_deref().unwrap_or_default(),
                "triggers": &persona.triggers,
                "tools": &persona.tools,
                "autonomy": persona.autonomy_tier.map(|tier| tier.as_str()).unwrap_or_default(),
                "receipts": persona.receipt_policy.map(|policy| policy.as_str()).unwrap_or_default(),
            })
        }).collect::<Vec<_>>(),
    }))
}

pub(crate) fn run_check(manifest: Option<&Path>, args: &PersonaCheckArgs) {
    let selected = args.path.as_deref().or(manifest);
    if args.json {
        match check_payload(selected) {
            Ok(payload) => println!(
                "{}",
                serde_json::to_string_pretty(&payload).unwrap_or_else(|error| fatal(&format!(
                    "failed to serialize persona check output: {error}"
                )))
            ),
            Err(errors) => {
                print_validation_errors_json(&errors);
                process::exit(1);
            }
        }
        return;
    }
    let catalog = match load_catalog_validation(selected) {
        Ok(catalog) => catalog,
        Err(errors) => fatal(
            &errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    };
    println!(
        "ok: {} persona manifest validates ({} personas)",
        catalog.manifest_path.display(),
        catalog.personas.len()
    );
}

/// In-process variant of `harn persona inspect <name> --json`.
pub fn inspect_payload(manifest: Option<&Path>, name: &str) -> Result<serde_json::Value, String> {
    let catalog = load_catalog_result(manifest)?;
    let persona = catalog
        .personas
        .iter()
        .find(|persona| persona.name.as_deref() == Some(name))
        .ok_or_else(|| {
            format!(
                "persona '{}' not found in {}",
                name,
                catalog.manifest_path.display()
            )
        })?;
    Ok(persona_to_json(persona, &catalog))
}

pub(crate) fn run_inspect(manifest: Option<&Path>, args: &PersonaInspectArgs) {
    if args.json {
        let json = inspect_payload(manifest, &args.name).unwrap_or_else(|error| fatal(&error));
        println!(
            "{}",
            serde_json::to_string_pretty(&json)
                .unwrap_or_else(|error| fatal(&format!("failed to serialize persona: {error}")))
        );
        return;
    }

    let catalog = load_catalog_or_exit(manifest);
    let Some(persona) = catalog
        .personas
        .iter()
        .find(|persona| persona.name.as_deref() == Some(args.name.as_str()))
    else {
        fatal(&format!(
            "persona '{}' not found in {}",
            args.name,
            catalog.manifest_path.display()
        ));
    };

    println!(
        "name:           {}",
        persona.name.as_deref().unwrap_or_default()
    );
    if let Some(version) = &persona.version {
        println!("version:        {version}");
    }
    println!(
        "description:    {}",
        persona.description.as_deref().unwrap_or_default()
    );
    println!(
        "entry_workflow: {}",
        persona.entry_workflow.as_deref().unwrap_or_default()
    );
    println!("tools:          {}", comma_or_dash(&persona.tools));
    println!("capabilities:   {}", comma_or_dash(&persona.capabilities));
    println!(
        "autonomy_tier:  {}",
        persona
            .autonomy_tier
            .map(|tier| tier.as_str())
            .unwrap_or_default()
    );
    println!(
        "receipt_policy: {}",
        persona
            .receipt_policy
            .map(|policy| policy.as_str())
            .unwrap_or_default()
    );
    println!("triggers:       {}", comma_or_dash(&persona.triggers));
    println!("schedules:      {}", comma_or_dash(&persona.schedules));
    println!("handoffs:       {}", comma_or_dash(&persona.handoffs));
    println!("context_packs:  {}", comma_or_dash(&persona.context_packs));
    println!("evals:          {}", comma_or_dash(&persona.evals));
    if !persona.steps.is_empty() {
        println!("steps:");
        for step in &persona.steps {
            let mut detail = format!("  - {} ({})", step.name, step.function);
            if let Some(model) = step.model.as_deref() {
                detail.push_str(&format!(" model={model}"));
            }
            if let Some(budget) = step.budget.as_ref() {
                if let Some(max_tokens) = budget.max_tokens {
                    detail.push_str(&format!(" max_tokens={max_tokens}"));
                }
                if let Some(max_usd) = budget.max_usd {
                    detail.push_str(&format!(" max_usd={max_usd}"));
                }
            }
            if let Some(boundary) = step.error_boundary.as_deref() {
                detail.push_str(&format!(" error_boundary={boundary}"));
            }
            println!("{detail}");
        }
    }
    if !persona.stages.is_empty() {
        println!("stages:");
        for stage in &persona.stages {
            let tools = stage
                .allowed_tools
                .as_deref()
                .map(comma_or_dash)
                .unwrap_or_else(|| "inherit".to_string());
            let mut detail = format!("  - {} tools={tools}", stage.name);
            if let Some(level) = stage.side_effect_level.as_deref() {
                detail.push_str(&format!(" side_effect={level}"));
            }
            if let Some(max) = stage.max_iterations {
                detail.push_str(&format!(" max_iterations={max}"));
            }
            println!("{detail}");
        }
    }
    if let Some(owner) = &persona.owner {
        println!("owner:          {owner}");
    }
    println!("manifest:       {}", catalog.manifest_path.display());
}

/// In-process variant of `harn persona status`.
pub async fn status_payload(
    manifest: Option<&Path>,
    state_dir: &Path,
    name: &str,
    at: Option<&str>,
) -> Result<harn_vm::PersonaStatus, String> {
    let catalog = load_catalog_result(manifest)?;
    let binding = runtime_binding_or_err(&catalog, name)?;
    let log = open_persona_log(state_dir)?;
    let now_ms = timestamp_arg(at)?;
    harn_vm::persona_status(&log, &binding, now_ms).await
}

pub(crate) async fn run_status(
    manifest: Option<&Path>,
    state_dir: &Path,
    args: &PersonaStatusArgs,
) -> Result<(), String> {
    let status = status_payload(manifest, state_dir, &args.name, args.at.as_deref()).await?;
    print_status(&status, args.json);
    Ok(())
}

/// In-process variant of `harn persona pause`.
pub async fn pause_payload(
    manifest: Option<&Path>,
    state_dir: &Path,
    name: &str,
    at: Option<&str>,
) -> Result<harn_vm::PersonaStatus, String> {
    let catalog = load_catalog_result(manifest)?;
    let binding = runtime_binding_or_err(&catalog, name)?;
    let log = open_persona_log(state_dir)?;
    let now_ms = timestamp_arg(at)?;
    harn_vm::pause_persona(&log, &binding, now_ms).await
}

pub(crate) async fn run_pause(
    manifest: Option<&Path>,
    state_dir: &Path,
    args: &PersonaControlArgs,
) -> Result<(), String> {
    let status = pause_payload(manifest, state_dir, &args.name, args.at.as_deref()).await?;
    print_status(&status, args.json);
    Ok(())
}

/// In-process variant of `harn persona resume`.
pub async fn resume_payload(
    manifest: Option<&Path>,
    state_dir: &Path,
    name: &str,
    at: Option<&str>,
) -> Result<harn_vm::PersonaStatus, String> {
    let catalog = load_catalog_result(manifest)?;
    let binding = runtime_binding_or_err(&catalog, name)?;
    let log = open_persona_log(state_dir)?;
    let now_ms = timestamp_arg(at)?;
    harn_vm::resume_persona(&log, &binding, now_ms).await
}

pub(crate) async fn run_resume(
    manifest: Option<&Path>,
    state_dir: &Path,
    args: &PersonaControlArgs,
) -> Result<(), String> {
    let status = resume_payload(manifest, state_dir, &args.name, args.at.as_deref()).await?;
    print_status(&status, args.json);
    Ok(())
}

/// In-process variant of `harn persona disable`.
pub async fn disable_payload(
    manifest: Option<&Path>,
    state_dir: &Path,
    name: &str,
    at: Option<&str>,
) -> Result<harn_vm::PersonaStatus, String> {
    let catalog = load_catalog_result(manifest)?;
    let binding = runtime_binding_or_err(&catalog, name)?;
    let log = open_persona_log(state_dir)?;
    let now_ms = timestamp_arg(at)?;
    harn_vm::disable_persona(&log, &binding, now_ms).await
}

pub(crate) async fn run_disable(
    manifest: Option<&Path>,
    state_dir: &Path,
    args: &PersonaControlArgs,
) -> Result<(), String> {
    let status = disable_payload(manifest, state_dir, &args.name, args.at.as_deref()).await?;
    print_status(&status, args.json);
    Ok(())
}

/// In-process variant of `harn persona tick`. Returns the run receipt that
/// the CLI would otherwise print to stdout.
pub async fn tick_payload(
    manifest: Option<&Path>,
    state_dir: &Path,
    name: &str,
    at: Option<&str>,
    cost_usd: f64,
    tokens: u64,
) -> Result<harn_vm::PersonaRunReceipt, String> {
    let catalog = load_catalog_result(manifest)?;
    let binding = runtime_binding_or_err(&catalog, name)?;
    let log = open_persona_log(state_dir)?;
    let now_ms = timestamp_arg(at)?;
    let receipt = harn_vm::fire_persona_schedule(
        &log,
        &binding,
        harn_vm::PersonaRunCost {
            cost_usd,
            tokens,
            ..Default::default()
        },
        now_ms,
    )
    .await?;
    log.flush().await.map_err(|error| error.to_string())?;
    Ok(receipt)
}

pub(crate) async fn run_tick(
    manifest: Option<&Path>,
    state_dir: &Path,
    args: &PersonaTickArgs,
) -> Result<(), String> {
    let receipt = tick_payload(
        manifest,
        state_dir,
        &args.name,
        args.at.as_deref(),
        args.cost_usd,
        args.tokens,
    )
    .await?;
    print_receipt(&receipt, args.json);
    Ok(())
}

/// In-process variant of `harn persona trigger`. `metadata_pairs` accepts
/// the same `KEY=VALUE` strings the CLI does.
#[allow(clippy::too_many_arguments)]
pub async fn trigger_payload(
    manifest: Option<&Path>,
    state_dir: &Path,
    name: &str,
    provider: &str,
    kind: &str,
    metadata_pairs: &[String],
    at: Option<&str>,
    cost_usd: f64,
    tokens: u64,
) -> Result<harn_vm::PersonaRunReceipt, String> {
    let catalog = load_catalog_result(manifest)?;
    let binding = runtime_binding_or_err(&catalog, name)?;
    let log = open_persona_log(state_dir)?;
    let now_ms = timestamp_arg(at)?;
    let metadata = parse_metadata(metadata_pairs)?;
    let receipt = harn_vm::fire_persona_trigger(
        &log,
        &binding,
        provider,
        kind,
        metadata,
        harn_vm::PersonaRunCost {
            cost_usd,
            tokens,
            ..Default::default()
        },
        now_ms,
    )
    .await?;
    log.flush().await.map_err(|error| error.to_string())?;
    Ok(receipt)
}

pub(crate) async fn run_trigger(
    manifest: Option<&Path>,
    state_dir: &Path,
    args: &PersonaTriggerArgs,
) -> Result<(), String> {
    let receipt = trigger_payload(
        manifest,
        state_dir,
        &args.name,
        &args.provider,
        &args.kind,
        &args.metadata,
        args.at.as_deref(),
        args.cost_usd,
        args.tokens,
    )
    .await?;
    print_receipt(&receipt, args.json);
    Ok(())
}

/// In-process variant of `harn persona spend`.
pub async fn spend_payload(
    manifest: Option<&Path>,
    state_dir: &Path,
    name: &str,
    at: Option<&str>,
    cost_usd: f64,
    tokens: u64,
) -> Result<harn_vm::PersonaBudgetStatus, String> {
    let catalog = load_catalog_result(manifest)?;
    let binding = runtime_binding_or_err(&catalog, name)?;
    let log = open_persona_log(state_dir)?;
    let now_ms = timestamp_arg(at)?;
    let budget = harn_vm::record_persona_spend(
        &log,
        &binding,
        harn_vm::PersonaRunCost {
            cost_usd,
            tokens,
            ..Default::default()
        },
        now_ms,
    )
    .await?;
    log.flush().await.map_err(|error| error.to_string())?;
    Ok(budget)
}

pub(crate) async fn run_spend(
    manifest: Option<&Path>,
    state_dir: &Path,
    args: &PersonaSpendArgs,
) -> Result<(), String> {
    let budget = spend_payload(
        manifest,
        state_dir,
        &args.name,
        args.at.as_deref(),
        args.cost_usd,
        args.tokens,
    )
    .await?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&budget)
                .unwrap_or_else(|error| fatal(&format!("failed to serialize budget: {error}")))
        );
    } else {
        println!(
            "budget: spent_today=${:.4} tokens_today={} exhausted={}",
            budget.spent_today_usd, budget.tokens_today, budget.exhausted
        );
    }
    Ok(())
}

fn load_catalog_or_exit(manifest: Option<&Path>) -> ResolvedPersonaManifest {
    match load_catalog_result(manifest) {
        Ok(catalog) => catalog,
        Err(message) => fatal(&message),
    }
}

fn load_catalog_result(manifest: Option<&Path>) -> Result<ResolvedPersonaManifest, String> {
    load_catalog_validation(manifest).map_err(|errors| validation_errors_to_string(&errors))
}

fn load_catalog_validation(
    manifest: Option<&Path>,
) -> Result<ResolvedPersonaManifest, Vec<PersonaValidationError>> {
    let result = if let Some(path) = manifest {
        package::load_personas_from_manifest_path(path).map(Some)
    } else {
        package::load_personas_config(None)
    };
    match result {
        Ok(Some(catalog)) => Ok(catalog),
        Ok(None) => Err(vec![PersonaValidationError {
            manifest_path: PathBuf::from("harn.toml"),
            field_path: "harn.toml".to_string(),
            message: "no harn.toml found; pass --manifest <path> or run inside a Harn project"
                .to_string(),
        }]),
        Err(errors) => Err(errors),
    }
}

fn validation_errors_to_string(errors: &[PersonaValidationError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn runtime_binding_or_err(
    catalog: &ResolvedPersonaManifest,
    name: &str,
) -> Result<harn_vm::PersonaRuntimeBinding, String> {
    let persona = catalog
        .personas
        .iter()
        .find(|persona| persona.name.as_deref() == Some(name))
        .ok_or_else(|| {
            format!(
                "persona '{}' not found in {}",
                name,
                catalog.manifest_path.display()
            )
        })?;
    Ok(crate::package::persona_runtime_binding(
        persona.name.as_deref().unwrap_or_default(),
        persona,
    ))
}

pub(super) fn open_persona_log(state_dir: &Path) -> Result<Arc<AnyEventLog>, String> {
    let state_dir = absolutize_from_cwd(state_dir)?;
    std::fs::create_dir_all(&state_dir).map_err(|error| {
        format!(
            "failed to create persona state dir {}: {error}",
            state_dir.display()
        )
    })?;
    harn_vm::event_log::install_default_for_base_dir(&state_dir)
        .map_err(|error| format!("failed to open persona event log: {error}"))
}

fn absolutize_from_cwd(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|error| format!("failed to read current directory: {error}"))
}

pub(super) fn timestamp_arg(value: Option<&str>) -> Result<i64, String> {
    match value {
        Some(value) => harn_vm::parse_persona_ms(value),
        None => Ok(harn_vm::persona_now_ms()),
    }
}

fn parse_metadata(values: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut metadata = BTreeMap::new();
    for value in values {
        let Some((key, raw)) = value.split_once('=') else {
            return Err(format!("metadata '{value}' must use KEY=VALUE syntax"));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(format!("metadata '{value}' has an empty key"));
        }
        metadata.insert(key.to_string(), raw.to_string());
    }
    Ok(metadata)
}

fn print_status(status: &harn_vm::PersonaStatus, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(status)
                .unwrap_or_else(|error| fatal(&format!("failed to serialize status: {error}")))
        );
        return;
    }
    println!("persona:        {}", status.name);
    println!(
        "template_ref:   {}",
        status.template_ref.as_deref().unwrap_or("-")
    );
    println!("state:          {}", status.state.as_str());
    println!("entry_workflow: {}", status.entry_workflow);
    println!("role:           {}", status.role);
    println!(
        "assignment:     {}",
        status
            .current_assignment
            .as_ref()
            .map(|assignment| assignment.work_key.as_str())
            .unwrap_or("-")
    );
    println!(
        "last_run:       {}",
        status.last_run.as_deref().unwrap_or("-")
    );
    println!(
        "next_run:       {}",
        status.next_scheduled_run.as_deref().unwrap_or("-")
    );
    println!("queued_events:  {}", status.queued_events);
    if !status.handoff_inbox.is_empty() {
        println!("handoffs:");
        for handoff in &status.handoff_inbox {
            println!(
                "  - {} kind={} from={} task={}",
                handoff
                    .handoff_id
                    .as_deref()
                    .unwrap_or(handoff.work_key.as_str()),
                handoff.handoff_kind.as_deref().unwrap_or("-"),
                handoff.source_persona.as_deref().unwrap_or("-"),
                handoff.task.as_deref().unwrap_or("-")
            );
        }
    }
    println!(
        "active_lease:   {}",
        status
            .active_lease
            .as_ref()
            .map(|lease| lease.id.as_str())
            .unwrap_or("-")
    );
    println!(
        "budget:         spent_today=${:.4} remaining_today={}",
        status.budget.spent_today_usd,
        status
            .budget
            .remaining_today_usd
            .map(|value| format!("${value:.4}"))
            .unwrap_or_else(|| "-".to_string())
    );
    if let Some(receipt) = status.value_receipts.last() {
        println!(
            "last_receipt:   {} paid=${:.4} avoided=${:.4}",
            receipt.kind.as_str(),
            receipt.paid_cost_usd,
            receipt.avoided_cost_usd
        );
    }
    if let Some(error) = &status.last_error {
        println!("last_error:     {error}");
    }
}

fn print_receipt(receipt: &harn_vm::PersonaRunReceipt, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(receipt)
                .unwrap_or_else(|error| fatal(&format!("failed to serialize receipt: {error}")))
        );
    } else {
        println!(
            "persona={} status={} work_key={} queued={}",
            receipt.persona, receipt.status, receipt.work_key, receipt.queued
        );
        if let Some(error) = &receipt.error {
            println!("error={error}");
        }
    }
}

fn print_validation_errors_json(errors: &[PersonaValidationError]) {
    let payload = serde_json::json!({
        "ok": false,
        "errors": errors.iter().map(|error| {
            serde_json::json!({
                "manifest_path": &error.manifest_path,
                "field_path": &error.field_path,
                "message": &error.message,
            })
        }).collect::<Vec<_>>(),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).unwrap_or_else(|error| {
            fatal(&format!(
                "failed to serialize persona validation errors: {error}"
            ))
        })
    );
}

fn persona_to_json(
    persona: &PersonaManifestEntry,
    catalog: &ResolvedPersonaManifest,
) -> serde_json::Value {
    serde_json::json!({
        "name": persona.name.as_deref().unwrap_or_default(),
        "version": persona.version.as_deref(),
        "description": persona.description.as_deref().unwrap_or_default(),
        "entry_workflow": persona.entry_workflow.as_deref().unwrap_or_default(),
        "tools": &persona.tools,
        "capabilities": &persona.capabilities,
        "autonomy_tier": persona.autonomy_tier.map(|tier| tier.as_str()).unwrap_or_default(),
        "receipt_policy": persona.receipt_policy.map(|policy| policy.as_str()).unwrap_or_default(),
        "triggers": &persona.triggers,
        "schedules": &persona.schedules,
        "model_policy": {
            "default_model": persona.model_policy.default_model.as_deref(),
            "escalation_model": persona.model_policy.escalation_model.as_deref(),
            "fallback_models": &persona.model_policy.fallback_models,
            "reasoning_effort": persona.model_policy.reasoning_effort.as_deref(),
        },
        "budget": {
            "daily_usd": persona.budget.daily_usd,
            "hourly_usd": persona.budget.hourly_usd,
            "run_usd": persona.budget.run_usd,
            "frontier_escalations": persona.budget.frontier_escalations,
            "max_tokens": persona.budget.max_tokens,
            "max_runtime_seconds": persona.budget.max_runtime_seconds,
        },
        "handoffs": &persona.handoffs,
        "context_packs": &persona.context_packs,
        "evals": &persona.evals,
        "steps": &persona.steps,
        "stages": &persona.stages,
        "owner": persona.owner.as_deref(),
        "package_source": {
            "package": persona.package_source.package.as_deref(),
            "path": persona.package_source.path.as_deref(),
            "git": persona.package_source.git.as_deref(),
            "rev": persona.package_source.rev.as_deref(),
        },
        "rollout_policy": {
            "mode": persona.rollout_policy.mode.as_deref(),
            "percentage": persona.rollout_policy.percentage,
            "cohorts": &persona.rollout_policy.cohorts,
        },
        "source": {
            "manifest_path": &catalog.manifest_path,
            "manifest_dir": &catalog.manifest_dir,
        },
    })
}

fn comma_or_dash(values: &[String]) -> String {
    if values.is_empty() {
        "-".to_string()
    } else {
        values.join(", ")
    }
}

fn fatal(message: &str) -> ! {
    eprintln!("error: {message}");
    process::exit(1);
}
