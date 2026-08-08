use std::path::Path;
use std::str::FromStr;

use harn_modules::personas::PersonaAutonomyTier;

use crate::cli::{PersonaActivateArgs, PersonaActivationsArgs, PersonaDeactivateArgs};
use crate::package::{self, PersonaActivationReceipt, PersonaActivationRecord, PersonaAttenuation};

pub(crate) fn run_activate(
    manifest: Option<&Path>,
    args: &PersonaActivateArgs,
) -> Result<(), String> {
    let receipt = activate_payload(manifest, args)?;
    print_receipt(&receipt, args.json)
}

pub(crate) fn activate_payload(
    manifest: Option<&Path>,
    args: &PersonaActivateArgs,
) -> Result<PersonaActivationReceipt, String> {
    let attenuation = attenuation_from_args(args)?;
    let now_ms = super::persona::timestamp_arg(args.at.as_deref())?;
    package::activate_persona(manifest, &args.name, &attenuation, now_ms)
        .map_err(|error| error.to_string())
}

pub(crate) fn run_deactivate(
    manifest: Option<&Path>,
    args: &PersonaDeactivateArgs,
) -> Result<(), String> {
    let receipt = deactivate_payload(manifest, args)?;
    print_receipt(&receipt, args.json)
}

pub(crate) fn deactivate_payload(
    manifest: Option<&Path>,
    args: &PersonaDeactivateArgs,
) -> Result<PersonaActivationReceipt, String> {
    let now_ms = super::persona::timestamp_arg(args.at.as_deref())?;
    package::deactivate_persona(manifest, &args.name, now_ms).map_err(|error| error.to_string())
}

pub(crate) fn run_activations(
    manifest: Option<&Path>,
    args: &PersonaActivationsArgs,
) -> Result<(), String> {
    let activations = activations_payload(manifest)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&activations).map_err(|error| error.to_string())?
        );
    } else {
        print_activations(&activations);
    }
    Ok(())
}

pub(crate) fn activations_payload(
    manifest: Option<&Path>,
) -> Result<Vec<PersonaActivationRecord>, String> {
    package::list_persona_activations(manifest).map_err(|error| error.to_string())
}

fn attenuation_from_args(args: &PersonaActivateArgs) -> Result<PersonaAttenuation, String> {
    let autonomy_tier = args
        .autonomy_tier
        .as_deref()
        .map(PersonaAutonomyTier::from_str)
        .transpose()
        .map_err(|()| "invalid autonomy tier".to_string())?;
    Ok(PersonaAttenuation {
        autonomy_tier,
        tools: selected_set(&args.tools, args.no_tools),
        capabilities: selected_set(&args.capabilities, args.no_capabilities),
    })
}

fn selected_set(values: &[String], deny_all: bool) -> Option<Vec<String>> {
    if deny_all {
        Some(Vec::new())
    } else if values.is_empty() {
        None
    } else {
        Some(values.to_vec())
    }
}

fn print_receipt(receipt: &PersonaActivationReceipt, json: bool) -> Result<(), String> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(receipt).map_err(|error| error.to_string())?
        );
    } else {
        println!(
            "{}: {} changed={} ledger={}",
            match receipt.action {
                package::PersonaActivationAction::Activate => "activated",
                package::PersonaActivationAction::Deactivate => "deactivated",
            },
            receipt.persona_id,
            receipt.changed,
            receipt.ledger_path
        );
    }
    Ok(())
}

fn print_activations(activations: &[PersonaActivationRecord]) {
    if activations.is_empty() {
        println!("No installed persona activations found.");
        return;
    }
    println!("Installed persona activations:");
    for activation in activations {
        let status = activation
            .migration
            .as_ref()
            .map(|_| " reactivation-required")
            .unwrap_or("");
        println!(
            "  {}  package={} digest={} autonomy={}{}",
            activation.persona_id,
            activation.package.alias,
            activation.package.content_hash,
            activation.effective_policy.autonomy_tier.as_str(),
            status
        );
    }
}
