use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;

use harn_vm::event_log::{AnyEventLog, EventLog};

use crate::cli::{
    PersonaControlArgs, PersonaInspectArgs, PersonaListArgs, PersonaSpendArgs, PersonaStatusArgs,
    PersonaTickArgs, PersonaTriggerArgs,
};
use crate::package::{self, PersonaManifestEntry, ResolvedPersonaManifest};

pub(crate) fn run_list(manifest: Option<&Path>, args: &PersonaListArgs) {
    let catalog = load_catalog_or_exit(manifest);
    if args.json {
        let personas: Vec<serde_json::Value> = catalog
            .personas
            .iter()
            .map(|persona| persona_to_json(persona, &catalog))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&personas)
                .unwrap_or_else(|error| fatal(&format!("failed to serialize personas: {error}")))
        );
        return;
    }

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

pub(crate) fn run_inspect(manifest: Option<&Path>, args: &PersonaInspectArgs) {
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

    if args.json {
        let json = persona_to_json(persona, &catalog);
        println!(
            "{}",
            serde_json::to_string_pretty(&json)
                .unwrap_or_else(|error| fatal(&format!("failed to serialize persona: {error}")))
        );
        return;
    }

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
    if let Some(owner) = &persona.owner {
        println!("owner:          {owner}");
    }
    println!("manifest:       {}", catalog.manifest_path.display());
}

pub(crate) async fn run_status(
    manifest: Option<&Path>,
    state_dir: &Path,
    args: &PersonaStatusArgs,
) -> Result<(), String> {
    let catalog = load_catalog_result(manifest)?;
    let binding = runtime_binding_or_err(&catalog, &args.name)?;
    let log = open_persona_log(state_dir)?;
    let status = harn_vm::persona_status(&log, &binding, harn_vm::persona_now_ms()).await?;
    print_status(&status, args.json);
    Ok(())
}

pub(crate) async fn run_pause(
    manifest: Option<&Path>,
    state_dir: &Path,
    args: &PersonaControlArgs,
) -> Result<(), String> {
    let catalog = load_catalog_result(manifest)?;
    let binding = runtime_binding_or_err(&catalog, &args.name)?;
    let log = open_persona_log(state_dir)?;
    let status = harn_vm::pause_persona(&log, &binding, harn_vm::persona_now_ms()).await?;
    print_status(&status, args.json);
    Ok(())
}

pub(crate) async fn run_resume(
    manifest: Option<&Path>,
    state_dir: &Path,
    args: &PersonaControlArgs,
) -> Result<(), String> {
    let catalog = load_catalog_result(manifest)?;
    let binding = runtime_binding_or_err(&catalog, &args.name)?;
    let log = open_persona_log(state_dir)?;
    let status = harn_vm::resume_persona(&log, &binding, harn_vm::persona_now_ms()).await?;
    print_status(&status, args.json);
    Ok(())
}

pub(crate) async fn run_disable(
    manifest: Option<&Path>,
    state_dir: &Path,
    args: &PersonaControlArgs,
) -> Result<(), String> {
    let catalog = load_catalog_result(manifest)?;
    let binding = runtime_binding_or_err(&catalog, &args.name)?;
    let log = open_persona_log(state_dir)?;
    let status = harn_vm::disable_persona(&log, &binding, harn_vm::persona_now_ms()).await?;
    print_status(&status, args.json);
    Ok(())
}

pub(crate) async fn run_tick(
    manifest: Option<&Path>,
    state_dir: &Path,
    args: &PersonaTickArgs,
) -> Result<(), String> {
    let catalog = load_catalog_result(manifest)?;
    let binding = runtime_binding_or_err(&catalog, &args.name)?;
    let log = open_persona_log(state_dir)?;
    let now_ms = timestamp_arg(args.at.as_deref())?;
    let receipt = harn_vm::fire_persona_schedule(
        &log,
        &binding,
        harn_vm::PersonaRunCost {
            cost_usd: args.cost_usd,
            tokens: args.tokens,
        },
        now_ms,
    )
    .await?;
    log.flush().await.map_err(|error| error.to_string())?;
    print_receipt(&receipt, args.json);
    Ok(())
}

pub(crate) async fn run_trigger(
    manifest: Option<&Path>,
    state_dir: &Path,
    args: &PersonaTriggerArgs,
) -> Result<(), String> {
    let catalog = load_catalog_result(manifest)?;
    let binding = runtime_binding_or_err(&catalog, &args.name)?;
    let log = open_persona_log(state_dir)?;
    let now_ms = timestamp_arg(args.at.as_deref())?;
    let metadata = parse_metadata(&args.metadata)?;
    let receipt = harn_vm::fire_persona_trigger(
        &log,
        &binding,
        &args.provider,
        &args.kind,
        metadata,
        harn_vm::PersonaRunCost {
            cost_usd: args.cost_usd,
            tokens: args.tokens,
        },
        now_ms,
    )
    .await?;
    log.flush().await.map_err(|error| error.to_string())?;
    print_receipt(&receipt, args.json);
    Ok(())
}

pub(crate) async fn run_spend(
    manifest: Option<&Path>,
    state_dir: &Path,
    args: &PersonaSpendArgs,
) -> Result<(), String> {
    let catalog = load_catalog_result(manifest)?;
    let binding = runtime_binding_or_err(&catalog, &args.name)?;
    let log = open_persona_log(state_dir)?;
    let now_ms = timestamp_arg(args.at.as_deref())?;
    let budget = harn_vm::record_persona_spend(
        &log,
        &binding,
        harn_vm::PersonaRunCost {
            cost_usd: args.cost_usd,
            tokens: args.tokens,
        },
        now_ms,
    )
    .await?;
    log.flush().await.map_err(|error| error.to_string())?;
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
    let result = if let Some(path) = manifest {
        package::load_personas_from_manifest_path(path).map(Some)
    } else {
        package::load_personas_config(None)
    };
    match result {
        Ok(Some(catalog)) => Ok(catalog),
        Ok(None) => Err(
            "no harn.toml found; pass --manifest <path> or run inside a Harn project".to_string(),
        ),
        Err(errors) => Err(errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")),
    }
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
    Ok(harn_vm::PersonaRuntimeBinding {
        name: persona.name.clone().unwrap_or_default(),
        entry_workflow: persona.entry_workflow.clone().unwrap_or_default(),
        schedules: persona.schedules.clone(),
        triggers: persona.triggers.clone(),
        budget: harn_vm::PersonaBudgetPolicy {
            daily_usd: persona.budget.daily_usd,
            hourly_usd: persona.budget.hourly_usd,
            run_usd: persona.budget.run_usd,
            max_tokens: persona.budget.max_tokens,
        },
    })
}

fn open_persona_log(state_dir: &Path) -> Result<Arc<AnyEventLog>, String> {
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

fn timestamp_arg(value: Option<&str>) -> Result<i64, String> {
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
    println!("state:          {}", status.state.as_str());
    println!("entry_workflow: {}", status.entry_workflow);
    println!(
        "last_run:       {}",
        status.last_run.as_deref().unwrap_or("-")
    );
    println!(
        "next_run:       {}",
        status.next_scheduled_run.as_deref().unwrap_or("-")
    );
    println!("queued_events:  {}", status.queued_events);
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
