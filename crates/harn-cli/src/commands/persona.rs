use std::path::Path;
use std::process;

use crate::cli::{PersonaInspectArgs, PersonaListArgs};
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

fn load_catalog_or_exit(manifest: Option<&Path>) -> ResolvedPersonaManifest {
    let result = if let Some(path) = manifest {
        package::load_personas_from_manifest_path(path).map(Some)
    } else {
        package::load_personas_config(None)
    };
    match result {
        Ok(Some(catalog)) => catalog,
        Ok(None) => {
            fatal("no harn.toml found; pass --manifest <path> or run inside a Harn project")
        }
        Err(errors) => {
            for error in &errors {
                eprintln!("error: {error}");
            }
            process::exit(1);
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
