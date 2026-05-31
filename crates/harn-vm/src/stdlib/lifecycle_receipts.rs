//! Harn-facing bindings for [`crate::orchestration::lifecycle_receipts`].
//!
//! Pipelines record / replay suspension, resumption, and drain-decision
//! receipts via these builtins. Conformance fixtures use them to assert
//! that the on-disk lifecycle journal round-trips byte-identically
//! between record and replay runs, that cached resume inputs survive a
//! re-run, and that the signed timestamps reject tampered receipts.

use std::collections::BTreeMap;

use serde_json::Value as JsonValue;

use crate::orchestration::{
    hash_drain_decision_prompt, hash_resume_input, lifecycle_receipts_snapshot,
    record_drain_decision_receipt, record_resumption_receipt, record_suspension_receipt,
    replay_drain_decision, replay_resume_input, DrainAction, DrainDecisionReceipt, DrainItem,
    DrainItemCategory, LifecycleReceiptError, RedactionPath, RedactionPolicy, ResumeInitiator,
    ResumptionReceipt, SignedLifecycleTimestamp, SuspendInitiator, SuspensionReceipt,
    TriggerMatchInfo,
};
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_lifecycle_receipt_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
    sig = "lifecycle_receipt_record_suspension(receipt: dict) -> dict",
    category = "lifecycle_receipts"
)]
fn lifecycle_receipt_record_suspension_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let receipt = parse_suspension_args(args)?;
    let entry = record_suspension_receipt(&receipt);
    Ok(json_to_vm(&entry.to_json()))
}

#[harn_builtin(
    sig = "lifecycle_receipt_record_resumption(receipt: dict) -> dict",
    category = "lifecycle_receipts"
)]
fn lifecycle_receipt_record_resumption_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let receipt = parse_resumption_args(args)?;
    let entry = record_resumption_receipt(&receipt);
    Ok(json_to_vm(&entry.to_json()))
}

#[harn_builtin(
    sig = "lifecycle_receipt_record_drain_decision(receipt: dict) -> dict",
    category = "lifecycle_receipts"
)]
fn lifecycle_receipt_record_drain_decision_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let receipt = parse_drain_decision_args(args)?;
    let entry = record_drain_decision_receipt(&receipt);
    Ok(json_to_vm(&entry.to_json()))
}

#[harn_builtin(
    sig = "lifecycle_receipts_snapshot() -> list",
    category = "lifecycle_receipts"
)]
fn lifecycle_receipts_snapshot_impl(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let snapshot = lifecycle_receipts_snapshot();
    let entries: Vec<VmValue> = snapshot
        .into_iter()
        .map(|entry| json_to_vm(&entry.to_json()))
        .collect();
    Ok(VmValue::List(std::sync::Arc::new(entries)))
}

#[harn_builtin(
    sig = "verify_lifecycle_receipt_signature(kind: string, payload: dict) -> dict",
    category = "lifecycle_receipts"
)]
fn verify_lifecycle_receipt_signature_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let kind = args
        .first()
        .map(|value| value.display())
        .unwrap_or_default();
    let payload = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let payload_json = crate::llm::vm_value_to_json(&payload);
    let outcome = verify_receipt_by_kind(&kind, &payload_json);
    Ok(verification_value(outcome))
}

#[harn_builtin(
    sig = "lifecycle_resume_input_hash(input: any) -> string",
    category = "lifecycle_receipts"
)]
fn lifecycle_resume_input_hash_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let input = args.first().cloned().unwrap_or(VmValue::Nil);
    let json = if matches!(input, VmValue::Nil) {
        None
    } else {
        Some(crate::llm::vm_value_to_json(&input))
    };
    let hash = hash_resume_input(json.as_ref());
    Ok(VmValue::String(std::sync::Arc::from(hash)))
}

#[harn_builtin(
    sig = "lifecycle_drain_decision_prompt_hash(prompt: string) -> string",
    category = "lifecycle_receipts"
)]
fn lifecycle_drain_decision_prompt_hash_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let prompt = args
        .first()
        .map(|value| value.display())
        .unwrap_or_default();
    Ok(VmValue::String(std::sync::Arc::from(
        hash_drain_decision_prompt(&prompt),
    )))
}

#[harn_builtin(
    sig = "lifecycle_replay_resume_input(receipt: dict, candidate?: any) -> dict",
    category = "lifecycle_receipts"
)]
fn lifecycle_replay_resume_input_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let receipt_value = args.first().cloned().unwrap_or(VmValue::Nil);
    let candidate = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let receipt_json = crate::llm::vm_value_to_json(&receipt_value);
    let receipt: ResumptionReceipt = serde_json::from_value(receipt_json).map_err(|e| {
        VmError::Runtime(format!(
            "lifecycle_replay_resume_input: invalid receipt payload: {e}"
        ))
    })?;
    let candidate_json = if matches!(candidate, VmValue::Nil) {
        None
    } else {
        Some(crate::llm::vm_value_to_json(&candidate))
    };
    match replay_resume_input(&receipt, candidate_json.as_ref()) {
        Ok(cached) => Ok(VmValue::Dict(std::sync::Arc::new({
            let mut map = BTreeMap::new();
            map.insert("ok".to_string(), VmValue::Bool(true));
            map.insert(
                "input".to_string(),
                cached
                    .as_ref()
                    .map(crate::stdlib::json_to_vm_value)
                    .unwrap_or(VmValue::Nil),
            );
            map
        }))),
        Err(error) => Ok(error_value(error)),
    }
}

#[harn_builtin(
    sig = "lifecycle_replay_drain_decision(receipt: dict, candidate_prompt?: string) -> dict",
    category = "lifecycle_receipts"
)]
fn lifecycle_replay_drain_decision_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let receipt_value = args.first().cloned().unwrap_or(VmValue::Nil);
    let candidate_prompt = args.get(1).and_then(|value| match value {
        VmValue::Nil => None,
        other => Some(other.display()),
    });
    let receipt_json = crate::llm::vm_value_to_json(&receipt_value);
    let receipt: DrainDecisionReceipt = serde_json::from_value(receipt_json).map_err(|e| {
        VmError::Runtime(format!(
            "lifecycle_replay_drain_decision: invalid receipt payload: {e}"
        ))
    })?;
    match replay_drain_decision(&receipt, candidate_prompt.as_deref()) {
        Ok(action) => Ok(VmValue::Dict(std::sync::Arc::new({
            let mut map = BTreeMap::new();
            map.insert("ok".to_string(), VmValue::Bool(true));
            map.insert(
                "action".to_string(),
                VmValue::String(std::sync::Arc::from(drain_action_str(action))),
            );
            map
        }))),
        Err(error) => Ok(error_value(error)),
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &LIFECYCLE_RECEIPT_RECORD_SUSPENSION_IMPL_DEF,
    &LIFECYCLE_RECEIPT_RECORD_RESUMPTION_IMPL_DEF,
    &LIFECYCLE_RECEIPT_RECORD_DRAIN_DECISION_IMPL_DEF,
    &LIFECYCLE_RECEIPTS_SNAPSHOT_IMPL_DEF,
    &VERIFY_LIFECYCLE_RECEIPT_SIGNATURE_IMPL_DEF,
    &LIFECYCLE_RESUME_INPUT_HASH_IMPL_DEF,
    &LIFECYCLE_DRAIN_DECISION_PROMPT_HASH_IMPL_DEF,
    &LIFECYCLE_REPLAY_RESUME_INPUT_IMPL_DEF,
    &LIFECYCLE_REPLAY_DRAIN_DECISION_IMPL_DEF,
];

fn parse_suspension_args(args: &[VmValue]) -> Result<SuspensionReceipt, VmError> {
    let dict = args
        .first()
        .and_then(VmValue::as_dict)
        .cloned()
        .ok_or_else(|| {
            VmError::Runtime(
                "lifecycle_receipt_record_suspension: first argument must be a dict".to_string(),
            )
        })?;
    let handle = require_string(&dict, "handle")?;
    let session_id = optional_string(&dict, "session_id");
    let initiator = parse_suspend_initiator(dict.get("initiator"));
    let initiator_id = require_string(&dict, "initiator_id")?;
    let reason = optional_string(&dict, "reason").unwrap_or_default();
    let conditions = dict
        .get("conditions")
        .filter(|value| !matches!(value, VmValue::Nil))
        .map(crate::llm::vm_value_to_json);
    let span_id = optional_string(&dict, "span_id");

    Ok(SuspensionReceipt::new(
        handle,
        session_id,
        initiator,
        initiator_id,
        reason,
        conditions,
        span_id,
    ))
}

fn parse_resumption_args(args: &[VmValue]) -> Result<ResumptionReceipt, VmError> {
    let dict = args
        .first()
        .and_then(VmValue::as_dict)
        .cloned()
        .ok_or_else(|| {
            VmError::Runtime(
                "lifecycle_receipt_record_resumption: first argument must be a dict".to_string(),
            )
        })?;
    let handle = require_string(&dict, "handle")?;
    let session_id = optional_string(&dict, "session_id");
    let initiator = parse_resume_initiator(dict.get("initiator"));
    let initiator_id = require_string(&dict, "initiator_id")?;
    let input = dict
        .get("input")
        .filter(|value| !matches!(value, VmValue::Nil))
        .map(crate::llm::vm_value_to_json);
    let redaction_policy = parse_redaction_policy(dict.get("redact"));
    let journaled_input = match (input.as_ref(), redaction_policy) {
        (Some(value), Some(policy)) => Some(policy.redact(value)),
        (Some(value), None) => Some(value.clone()),
        (None, _) => None,
    };
    let continue_transcript = dict
        .get("continue_transcript")
        .map(|value| value.is_truthy())
        .unwrap_or(true);
    let linked_suspension_span_id = optional_string(&dict, "linked_suspension_span_id");
    let trigger_match = dict
        .get("trigger_match")
        .and_then(VmValue::as_dict)
        .map(parse_trigger_match);

    Ok(ResumptionReceipt::new(
        handle,
        session_id,
        initiator,
        initiator_id,
        input.as_ref(),
        journaled_input,
        continue_transcript,
        linked_suspension_span_id,
        trigger_match,
    ))
}

fn parse_drain_decision_args(args: &[VmValue]) -> Result<DrainDecisionReceipt, VmError> {
    let dict = args
        .first()
        .and_then(VmValue::as_dict)
        .cloned()
        .ok_or_else(|| {
            VmError::Runtime(
                "lifecycle_receipt_record_drain_decision: first argument must be a dict"
                    .to_string(),
            )
        })?;
    let pipeline_id = require_string(&dict, "pipeline_id")?;
    let item_dict = dict
        .get("item")
        .and_then(VmValue::as_dict)
        .cloned()
        .ok_or_else(|| {
            VmError::Runtime(
                "lifecycle_receipt_record_drain_decision: item must be a dict".to_string(),
            )
        })?;
    let item = DrainItem {
        category: parse_drain_category(item_dict.get("category")),
        id: optional_string(&item_dict, "id").unwrap_or_default(),
        summary: optional_string(&item_dict, "summary").unwrap_or_default(),
    };
    let action = parse_drain_action(dict.get("action"));
    let reason = optional_string(&dict, "reason").unwrap_or_default();
    let decided_by = require_string(&dict, "decided_by")?;
    let prompt_hash = if let Some(prompt) = optional_string(&dict, "prompt") {
        Some(hash_drain_decision_prompt(&prompt))
    } else {
        optional_string(&dict, "prompt_hash")
    };

    Ok(DrainDecisionReceipt::new(
        pipeline_id,
        item,
        action,
        reason,
        decided_by,
        prompt_hash,
    ))
}

fn parse_suspend_initiator(value: Option<&VmValue>) -> SuspendInitiator {
    match value.map(|v| v.display()) {
        Some(text) => match text.trim() {
            "self" | "self_initiated" => SuspendInitiator::SelfInitiated,
            "parent" => SuspendInitiator::Parent,
            "triggered" => SuspendInitiator::Triggered,
            _ => SuspendInitiator::Operator,
        },
        None => SuspendInitiator::Operator,
    }
}

fn parse_resume_initiator(value: Option<&VmValue>) -> ResumeInitiator {
    let text = value.map(|v| v.display()).unwrap_or_default();
    ResumeInitiator::parse(&text)
}

fn parse_drain_category(value: Option<&VmValue>) -> DrainItemCategory {
    match value.map(|v| v.display()).as_deref() {
        Some("queued_trigger") => DrainItemCategory::QueuedTrigger,
        Some("partial_handoff") => DrainItemCategory::PartialHandoff,
        Some("in_flight_llm_call") => DrainItemCategory::InFlightLlmCall,
        _ => DrainItemCategory::SuspendedSubagent,
    }
}

fn parse_drain_action(value: Option<&VmValue>) -> DrainAction {
    match value.map(|v| v.display()).as_deref() {
        Some("cancel") => DrainAction::Cancel,
        Some("handoff") => DrainAction::Handoff,
        Some("acknowledge") => DrainAction::Acknowledge,
        Some("defer") => DrainAction::Defer,
        Some("wait") => DrainAction::Wait,
        Some("finalize") => DrainAction::Finalize,
        _ => DrainAction::Resume,
    }
}

fn drain_action_str(action: DrainAction) -> String {
    match action {
        DrainAction::Resume => "resume",
        DrainAction::Cancel => "cancel",
        DrainAction::Handoff => "handoff",
        DrainAction::Acknowledge => "acknowledge",
        DrainAction::Defer => "defer",
        DrainAction::Wait => "wait",
        DrainAction::Finalize => "finalize",
    }
    .to_string()
}

fn parse_trigger_match(dict: &BTreeMap<String, VmValue>) -> TriggerMatchInfo {
    TriggerMatchInfo {
        source: optional_string(dict, "source").unwrap_or_default(),
        event_id: optional_string(dict, "event_id").unwrap_or_default(),
        filter_summary: optional_string(dict, "filter_summary").unwrap_or_default(),
    }
}

fn parse_redaction_policy(value: Option<&VmValue>) -> Option<RedactionPolicy> {
    let dict = value.and_then(VmValue::as_dict)?;
    let paths_value = dict.get("paths")?;
    let VmValue::List(items) = paths_value else {
        return None;
    };
    let mut paths = Vec::new();
    for item in items.iter() {
        let Some(entry) = item.as_dict() else {
            continue;
        };
        let Some(pointer) = optional_string(entry, "pointer") else {
            continue;
        };
        let reason = optional_string(entry, "reason").unwrap_or_else(|| "redacted".to_string());
        paths.push(RedactionPath { pointer, reason });
    }
    if paths.is_empty() {
        None
    } else {
        Some(RedactionPolicy { paths })
    }
}

fn require_string(dict: &BTreeMap<String, VmValue>, key: &str) -> Result<String, VmError> {
    optional_string(dict, key)
        .ok_or_else(|| VmError::Runtime(format!("lifecycle receipt: missing {key}")))
}

fn optional_string(dict: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    dict.get(key).and_then(|value| match value {
        VmValue::String(text) if !text.is_empty() => Some(text.to_string()),
        VmValue::Nil => None,
        other => {
            let display = other.display();
            if display.is_empty() {
                None
            } else {
                Some(display)
            }
        }
    })
}

fn verify_receipt_by_kind(kind: &str, payload: &JsonValue) -> Result<(), LifecycleReceiptError> {
    match kind {
        "suspension_receipt" => {
            let receipt: SuspensionReceipt = serde_json::from_value(payload.clone())
                .map_err(|e| LifecycleReceiptError::Persistence(e.to_string()))?;
            receipt.verify_signature()
        }
        "resumption_receipt" => {
            let receipt: ResumptionReceipt = serde_json::from_value(payload.clone())
                .map_err(|e| LifecycleReceiptError::Persistence(e.to_string()))?;
            receipt.verify_signature()
        }
        "drain_decision_receipt" => {
            let receipt: DrainDecisionReceipt = serde_json::from_value(payload.clone())
                .map_err(|e| LifecycleReceiptError::Persistence(e.to_string()))?;
            receipt.verify_signature()
        }
        // Allow callers to pass a raw signed timestamp + a kind-bound
        // subject/initiator for ad-hoc verification.
        _ => Err(LifecycleReceiptError::Persistence(format!(
            "unknown lifecycle receipt kind: {kind}"
        ))),
    }
}

fn verification_value(result: Result<(), LifecycleReceiptError>) -> VmValue {
    let mut map = BTreeMap::new();
    match result {
        Ok(()) => {
            map.insert("verified".to_string(), VmValue::Bool(true));
        }
        Err(error) => {
            map.insert("verified".to_string(), VmValue::Bool(false));
            map.insert(
                "error".to_string(),
                VmValue::String(std::sync::Arc::from(error.to_string())),
            );
        }
    }
    VmValue::Dict(std::sync::Arc::new(map))
}

fn error_value(error: LifecycleReceiptError) -> VmValue {
    let mut map = BTreeMap::new();
    map.insert("ok".to_string(), VmValue::Bool(false));
    map.insert(
        "error".to_string(),
        VmValue::String(std::sync::Arc::from(error.to_string())),
    );
    let code = match &error {
        LifecycleReceiptError::ResumeInputHashMismatch { .. } => "HARN-SUS-011",
        LifecycleReceiptError::DrainDecisionPromptHashMismatch { .. } => "HARN-SUS-012",
        LifecycleReceiptError::SignatureMismatch { .. }
        | LifecycleReceiptError::SignatureAlgorithmMismatch { .. }
        | LifecycleReceiptError::SignatureKeyMismatch { .. } => "HARN-SUS-013",
        LifecycleReceiptError::Persistence(_) => "HARN-SUS-013",
    };
    map.insert(
        "code".to_string(),
        VmValue::String(std::sync::Arc::from(code)),
    );
    VmValue::Dict(std::sync::Arc::new(map))
}

fn json_to_vm(value: &JsonValue) -> VmValue {
    crate::stdlib::json_to_vm_value(value)
}

// Build a verifiable bare-bones `SignedLifecycleTimestamp` for ad-hoc
// fixtures that want to mint timestamps directly. Kept here so the
// orchestration module's surface stays focused on receipt types.
#[allow(dead_code)]
pub(crate) fn mint_signed_timestamp(
    kind: &str,
    subject: &str,
    initiator: &str,
) -> SignedLifecycleTimestamp {
    SignedLifecycleTimestamp::now_for(kind, subject, initiator)
}
