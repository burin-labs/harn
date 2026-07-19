//! Programmable command-runner policy hooks.
//!
//! Command policy is intentionally separate from the generic tool hook
//! registry: it sees normalized command-runner context before a process
//! spawns, records deterministic risk labels, and can block or rewrite
//! constrained request fields without relying on model prompt text.

use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use serde_json::{Map as JsonMap, Value as JsonValue};
use sha2::{Digest, Sha256};

use crate::value::{VmClosure, VmError, VmValue};

const DEFAULT_SHELL_MODE: &str = "argv_only";
const INLINE_OUTPUT_LIMIT: usize = 8_192;

thread_local! {
    static COMMAND_POLICY_STACK: RefCell<Vec<CommandPolicy>> = const { RefCell::new(Vec::new()) };
    static COMMAND_POLICY_HOOK_DEPTH: RefCell<usize> = const { RefCell::new(0) };
}

#[derive(Clone, Debug, Default)]
pub struct CommandPolicy {
    pub tools: Vec<String>,
    pub workspace_roots: Vec<String>,
    pub default_shell_mode: String,
    pub deny_patterns: Vec<String>,
    pub require_approval: BTreeSet<String>,
    /// Risk classes that are hard-denied (blocked before the child spawns) and
    /// NEVER routed to the consent gate. Distinct from `require_approval`, whose
    /// labels are consent-eligible. The built-in `catastrophic` label is always
    /// hard-denied regardless of this set (the never-approvable command floor);
    /// `deny_labels` lets a policy additionally promote other scanner labels
    /// (e.g. `destructive`, `network_exfil`) to the same never-approvable tier.
    pub deny_labels: BTreeSet<String>,
    pub pre: Option<Arc<VmClosure>>,
    pub post: Option<Arc<VmClosure>>,
    /// Optional consent gate for the `require_approval` disposition. When set,
    /// a command whose risk class is in `require_approval` (or a pre-hook
    /// `require_approval` decision) is routed through this closure — the
    /// `std/llm/tool_middleware::with_consent` prompt_fn contract — instead of
    /// being hard-blocked: `true` / `{decision: "approved"}` lets the command
    /// run; `false` / `{decision: "denied", reason?}` (and any non-bool,
    /// non-dict verdict) blocks it with a `consent_denied` disposition. When
    /// unset, `require_approval` keeps its legacy hard-block behavior so the
    /// default (no consent) path stays byte-identical.
    pub consent: Option<Arc<VmClosure>>,
    pub allow_recursive: bool,
}

#[derive(Clone, Debug)]
pub struct CommandPolicyDecision {
    pub action: String,
    pub reason: Option<String>,
    pub source: String,
    pub risk_labels: Vec<String>,
    pub confidence: f64,
    pub display: Option<JsonValue>,
}

#[derive(Clone, Debug)]
pub enum CommandPolicyPreflight {
    Proceed {
        params: crate::value::DictMap,
        context: JsonValue,
        decisions: Vec<CommandPolicyDecision>,
    },
    Blocked {
        status: &'static str,
        message: String,
        context: JsonValue,
        decisions: Vec<CommandPolicyDecision>,
    },
}

struct HookDepthGuard;

impl Drop for HookDepthGuard {
    fn drop(&mut self) {
        COMMAND_POLICY_HOOK_DEPTH.with(|depth| {
            let mut depth = depth.borrow_mut();
            *depth = depth.saturating_sub(1);
        });
    }
}

pub fn push_command_policy(policy: CommandPolicy) {
    COMMAND_POLICY_STACK.with(|stack| stack.borrow_mut().push(policy));
}

pub fn pop_command_policy() {
    COMMAND_POLICY_STACK.with(|stack| {
        stack.borrow_mut().pop();
    });
}

pub fn clear_command_policies() {
    COMMAND_POLICY_STACK.with(|stack| stack.borrow_mut().clear());
    COMMAND_POLICY_HOOK_DEPTH.with(|depth| *depth.borrow_mut() = 0);
}

pub fn current_command_policy() -> Option<CommandPolicy> {
    COMMAND_POLICY_STACK.with(|stack| stack.borrow().last().cloned())
}

/// Per-task ambient-scope swap of the command-policy stack and hook depth. See
/// `orchestration::ambient_scope` — these move whole stacks so a spawned worker
/// task can carry its own command scope across `.await` without leaking into
/// cooperatively-scheduled siblings on the same thread.
pub(crate) fn swap_command_policy_stack(next: Vec<CommandPolicy>) -> Vec<CommandPolicy> {
    COMMAND_POLICY_STACK.with(|stack| std::mem::replace(&mut *stack.borrow_mut(), next))
}

pub(crate) fn swap_command_policy_hook_depth(next: usize) -> usize {
    COMMAND_POLICY_HOOK_DEPTH.with(|depth| std::mem::replace(&mut *depth.borrow_mut(), next))
}

pub fn command_policy_hook_depth() -> usize {
    COMMAND_POLICY_HOOK_DEPTH.with(|depth| *depth.borrow())
}

pub fn parse_command_policy_value(
    value: Option<&VmValue>,
    label: &str,
) -> Result<Option<CommandPolicy>, VmError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if matches!(value, VmValue::Nil) {
        return Ok(None);
    }
    let Some(map) = value.as_dict() else {
        return Err(VmError::Runtime(format!(
            "{label}: command_policy must be a dict"
        )));
    };
    Ok(Some(CommandPolicy {
        tools: string_list_field(map, "tools")?.unwrap_or_default(),
        workspace_roots: string_list_field(map, "workspace_roots")?.unwrap_or_default(),
        default_shell_mode: string_field(map, "default_shell_mode")?
            .unwrap_or_else(|| DEFAULT_SHELL_MODE.to_string()),
        deny_patterns: string_list_field(map, "deny_patterns")?.unwrap_or_default(),
        require_approval: string_list_field(map, "require_approval")?
            .unwrap_or_default()
            .into_iter()
            .collect(),
        deny_labels: string_list_field(map, "deny_labels")?
            .unwrap_or_default()
            .into_iter()
            .collect(),
        pre: closure_field(map, "pre")?,
        post: closure_field(map, "post")?,
        consent: closure_field(map, "consent")?,
        allow_recursive: bool_field(map, "allow_recursive")?.unwrap_or(false),
    }))
}

pub fn normalize_command_policy_value(config: &VmValue) -> Result<VmValue, VmError> {
    let Some(map) = config.as_dict() else {
        return Err(VmError::Runtime(
            "command_policy: config must be a dict".to_string(),
        ));
    };
    let mut normalized = (*map).clone();
    normalized
        .entry(crate::value::intern_key("_type"))
        .or_insert_with(|| VmValue::String(arcstr::ArcStr::from("command_policy")));
    normalized
        .entry(crate::value::intern_key("default_shell_mode"))
        .or_insert_with(|| VmValue::String(arcstr::ArcStr::from(DEFAULT_SHELL_MODE)));
    normalized
        .entry(crate::value::intern_key("workspace_roots"))
        .or_insert_with(|| VmValue::List(std::sync::Arc::new(Vec::new())));
    normalized
        .entry(crate::value::intern_key("deny_patterns"))
        .or_insert_with(|| VmValue::List(std::sync::Arc::new(Vec::new())));
    normalized
        .entry(crate::value::intern_key("require_approval"))
        .or_insert_with(|| VmValue::List(std::sync::Arc::new(Vec::new())));
    normalized
        .entry(crate::value::intern_key("deny_labels"))
        .or_insert_with(|| VmValue::List(std::sync::Arc::new(Vec::new())));
    parse_command_policy_value(Some(&VmValue::dict(normalized.clone())), "command_policy")?;
    Ok(VmValue::dict(normalized))
}

pub fn command_risk_scan_value(ctx: &VmValue) -> Result<VmValue, VmError> {
    let json = crate::llm::vm_value_to_json(ctx);
    let scan = command_risk_scan_json(&json, None);
    Ok(crate::stdlib::json_to_vm_value(&scan))
}

pub fn command_result_scan_value(ctx: &VmValue) -> Result<VmValue, VmError> {
    let json = crate::llm::vm_value_to_json(ctx);
    let mut labels = Vec::new();
    let output = inline_output_for_scan(json.pointer("/result/stdout"))
        + &inline_output_for_scan(json.pointer("/result/stderr"));
    let lower = output.to_ascii_lowercase();
    if contains_secret_like_text(&lower) {
        labels.push("credential_output".to_string());
    }
    if lower.contains("permission denied") || lower.contains("operation not permitted") {
        labels.push("permission_boundary_hit".to_string());
    }
    if lower.contains("fatal:") || lower.contains("error:") {
        labels.push("error_output".to_string());
    }
    labels.sort();
    labels.dedup();
    let action = if labels.iter().any(|label| label == "credential_output") {
        "mark_unsafe"
    } else {
        "allow"
    };
    Ok(crate::stdlib::json_to_vm_value(&serde_json::json!({
        "action": action,
        "recommended_action": action,
        "risk_labels": labels,
        "confidence": if action == "allow" { 0.35 } else { 0.82 },
        "rationale": if action == "allow" {
            "no high-risk command output patterns detected"
        } else {
            "command output appears to contain credential-like material"
        },
    })))
}

pub fn command_llm_risk_scan_value(
    ctx: &VmValue,
    options: Option<&VmValue>,
) -> Result<VmValue, VmError> {
    let mut scan = crate::llm::vm_value_to_json(&command_risk_scan_value(ctx)?);
    let options_json = options
        .map(crate::llm::vm_value_to_json)
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = scan.as_object_mut() {
        obj.insert(
            "scan_kind".to_string(),
            JsonValue::String("deterministic_fallback".to_string()),
        );
        obj.insert("llm".to_string(), redact_json_for_llm(&options_json));
        obj.entry("rationale".to_string()).or_insert_with(|| {
            JsonValue::String("deterministic fallback used without external model call".to_string())
        });
    }
    Ok(crate::stdlib::json_to_vm_value(&scan))
}

pub async fn run_command_policy_preflight(
    params: &crate::value::DictMap,
    caller: JsonValue,
) -> Result<CommandPolicyPreflight, VmError> {
    run_command_policy_preflight_with_ctx(None, params, caller).await
}

pub async fn run_command_policy_preflight_with_ctx(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    params: &crate::value::DictMap,
    caller: JsonValue,
) -> Result<CommandPolicyPreflight, VmError> {
    let Some(policy) = current_command_policy() else {
        // No `command_policy` is on the stack. The catastrophic floor still
        // applies as a universal backstop — a bare `host_process_exec` must not
        // be able to run `rm -rf /`, a fork bomb, `mkfs`, `dd of=<disk>`, or the
        // git-destructive family (`git reset --hard`, `git clean -fd`,
        // force-push) just because no policy was pushed. Structured git
        // builtins such as `git.push(..., lease)` remain the reviewed path for
        // legitimate force-with-lease workflows; arbitrary textual git
        // destructors stay behind the same never-approvable floor as disk/data
        // destruction. The scan runs on `floor_command_text` exactly as the
        // policy path does, so the argv-quoting `sh -c "…"` wrapper bypass stays
        // closed here too.
        let default_policy = CommandPolicy::default();
        let context = command_context_json(params, &default_policy, caller);
        let scan = command_risk_scan_json(&context, None);
        let is_catastrophic = scan
            .get("catastrophic_reason")
            .and_then(|value| value.as_str())
            .is_some();
        if is_catastrophic {
            let labels = risk_labels_from_scan(&scan);
            let deny = hard_deny_decision(&scan, &default_policy, &labels)
                .expect("catastrophic_reason implies a hard-deny decision");
            let msg = deny.reason.clone().unwrap_or_default();
            return Ok(CommandPolicyPreflight::Blocked {
                status: "blocked",
                message: msg,
                context,
                decisions: vec![deny],
            });
        }
        return Ok(CommandPolicyPreflight::Proceed {
            params: params.clone(),
            context: JsonValue::Null,
            decisions: Vec::new(),
        });
    };

    if command_policy_hook_depth() > 0 && !policy.allow_recursive {
        let context = command_context_json(params, &policy, caller);
        let decision = decision(
            "deny",
            Some("command policy hooks cannot recursively call process.exec".to_string()),
            "recursion_guard",
            Vec::new(),
            1.0,
        );
        return Ok(CommandPolicyPreflight::Blocked {
            status: "blocked",
            message: decision.reason.clone().unwrap_or_default(),
            context,
            decisions: vec![decision],
        });
    }

    let mut current_params = params.clone();
    let mut context = command_context_json(&current_params, &policy, caller);
    let mut decisions = Vec::new();
    let mut rewritten_by_hook = false;
    let scan = command_risk_scan_json(&context, Some(&policy));
    if let Some(labels) = scan.get("risk_labels").and_then(|value| value.as_array()) {
        let labels = labels
            .iter()
            .filter_map(|value| value.as_str().map(ToString::to_string))
            .collect::<Vec<_>>();
        if !labels.is_empty() {
            decisions.push(decision(
                "classify",
                scan.get("rationale")
                    .and_then(|value| value.as_str())
                    .map(ToString::to_string),
                "deterministic",
                labels,
                scan.get("confidence")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.7),
            ));
        }
    }

    // Never-approvable command floor: enforced before deny-patterns and before
    // any consent/approval routing. The child is never spawned and the decision
    // is never sent to the consent gate.
    if let Some(deny) = hard_deny_decision(&scan, &policy, &risk_labels_from_scan(&scan)) {
        let msg = deny.reason.clone().unwrap_or_default();
        decisions.push(deny);
        return Ok(CommandPolicyPreflight::Blocked {
            status: "blocked",
            message: msg,
            context,
            decisions,
        });
    }

    if let Some(matched) = first_deny_pattern(&policy, &context) {
        let msg = if matched.candidate == command_text(&context) {
            format!("command denied by policy pattern {:?}", matched.pattern)
        } else {
            format!(
                "command segment {:?} denied by policy pattern {:?}",
                matched.candidate, matched.pattern
            )
        };
        let decision = decision("deny", Some(msg.clone()), "deny_patterns", Vec::new(), 1.0);
        decisions.push(decision);
        return Ok(CommandPolicyPreflight::Blocked {
            status: "blocked",
            message: msg,
            context,
            decisions,
        });
    }

    let risk_labels = risk_labels_from_scan(&scan);
    let matched_approval = risk_labels
        .iter()
        .find(|label| policy.require_approval.contains(label.as_str()))
        .cloned();
    if let Some(label) = matched_approval {
        let msg = format!("command requires approval for risk class {label}");
        decisions.push(decision(
            "require_approval",
            Some(msg.clone()),
            "deterministic",
            risk_labels.clone(),
            0.9,
        ));
        match command_consent_verdict(ctx, &policy, &context, &risk_labels, &msg).await? {
            ConsentVerdict::NoGate => {
                return Ok(CommandPolicyPreflight::Blocked {
                    status: "blocked",
                    message: msg,
                    context,
                    decisions,
                });
            }
            ConsentVerdict::Denied(reason) => {
                decisions.push(decision(
                    "consent_denied",
                    Some(reason.clone()),
                    "consent",
                    risk_labels.clone(),
                    1.0,
                ));
                return Ok(CommandPolicyPreflight::Blocked {
                    status: "consent_denied",
                    message: reason,
                    context,
                    decisions,
                });
            }
            ConsentVerdict::Approved => {
                decisions.push(decision(
                    "consent_granted",
                    Some(format!("consent granted for {msg}")),
                    "consent",
                    risk_labels.clone(),
                    1.0,
                ));
            }
        }
    }

    if let Some(pre) = policy.pre.as_ref() {
        let action = invoke_command_hook(ctx, pre, &context).await?;
        match parse_pre_hook_action(action)? {
            ParsedPreHookAction::Allow => {}
            ParsedPreHookAction::Deny(message) => {
                decisions.push(decision(
                    "deny",
                    Some(message.clone()),
                    "pre_hook",
                    risk_labels,
                    1.0,
                ));
                return Ok(CommandPolicyPreflight::Blocked {
                    status: "blocked",
                    message,
                    context,
                    decisions,
                });
            }
            ParsedPreHookAction::RequireApproval(message, display) => {
                decisions.push(CommandPolicyDecision {
                    action: "require_approval".to_string(),
                    reason: Some(message.clone()),
                    source: "pre_hook".to_string(),
                    risk_labels: risk_labels.clone(),
                    confidence: 1.0,
                    display,
                });
                match command_consent_verdict(ctx, &policy, &context, &risk_labels, &message)
                    .await?
                {
                    ConsentVerdict::NoGate => {
                        return Ok(CommandPolicyPreflight::Blocked {
                            status: "blocked",
                            message,
                            context,
                            decisions,
                        });
                    }
                    ConsentVerdict::Denied(reason) => {
                        decisions.push(decision(
                            "consent_denied",
                            Some(reason.clone()),
                            "consent",
                            risk_labels,
                            1.0,
                        ));
                        return Ok(CommandPolicyPreflight::Blocked {
                            status: "consent_denied",
                            message: reason,
                            context,
                            decisions,
                        });
                    }
                    ConsentVerdict::Approved => {
                        decisions.push(decision(
                            "consent_granted",
                            Some(format!("consent granted for {message}")),
                            "consent",
                            risk_labels,
                            1.0,
                        ));
                    }
                }
            }
            ParsedPreHookAction::DryRun(message) => {
                decisions.push(decision(
                    "dry_run",
                    Some(message.clone()),
                    "pre_hook",
                    risk_labels,
                    1.0,
                ));
                return Ok(CommandPolicyPreflight::Blocked {
                    status: "dry_run",
                    message,
                    context,
                    decisions,
                });
            }
            ParsedPreHookAction::ExplainOnly(message) => {
                decisions.push(decision(
                    "explain_only",
                    Some(message.clone()),
                    "pre_hook",
                    risk_labels,
                    1.0,
                ));
                return Ok(CommandPolicyPreflight::Blocked {
                    status: "explain_only",
                    message,
                    context,
                    decisions,
                });
            }
            ParsedPreHookAction::Rewrite(rewrite) => {
                apply_command_rewrite(&mut current_params, &rewrite)?;
                rewritten_by_hook = true;
                decisions.push(decision(
                    "rewrite",
                    Some("command request rewritten by pre-hook".to_string()),
                    "pre_hook",
                    risk_labels,
                    1.0,
                ));
                context = command_context_json(&current_params, &policy, context["caller"].clone());
            }
        }
    }

    if rewritten_by_hook {
        let scan = command_risk_scan_json(&context, Some(&policy));
        // Re-apply the never-approvable floor to the rewritten command: a
        // pre-hook rewrite must not be able to smuggle a catastrophic command
        // past the floor.
        if let Some(deny) = hard_deny_decision(&scan, &policy, &risk_labels_from_scan(&scan)) {
            let msg = deny.reason.clone().unwrap_or_default();
            decisions.push(deny);
            return Ok(CommandPolicyPreflight::Blocked {
                status: "blocked",
                message: msg,
                context,
                decisions,
            });
        }
        if let Some(matched) = first_deny_pattern(&policy, &context) {
            let msg = format!("rewritten command denied by policy pattern {matched:?}");
            decisions.push(decision(
                "deny",
                Some(msg.clone()),
                "deny_patterns",
                risk_labels_from_scan(&scan),
                1.0,
            ));
            return Ok(CommandPolicyPreflight::Blocked {
                status: "blocked",
                message: msg,
                context,
                decisions,
            });
        }
        let risk_labels = risk_labels_from_scan(&scan);
        let matched_approval = risk_labels
            .iter()
            .find(|label| policy.require_approval.contains(label.as_str()))
            .cloned();
        if let Some(label) = matched_approval {
            let msg = format!("rewritten command requires approval for risk class {label}");
            decisions.push(decision(
                "require_approval",
                Some(msg.clone()),
                "deterministic",
                risk_labels.clone(),
                0.9,
            ));
            match command_consent_verdict(ctx, &policy, &context, &risk_labels, &msg).await? {
                ConsentVerdict::NoGate => {
                    return Ok(CommandPolicyPreflight::Blocked {
                        status: "blocked",
                        message: msg,
                        context,
                        decisions,
                    });
                }
                ConsentVerdict::Denied(reason) => {
                    decisions.push(decision(
                        "consent_denied",
                        Some(reason.clone()),
                        "consent",
                        risk_labels,
                        1.0,
                    ));
                    return Ok(CommandPolicyPreflight::Blocked {
                        status: "consent_denied",
                        message: reason,
                        context,
                        decisions,
                    });
                }
                ConsentVerdict::Approved => {
                    decisions.push(decision(
                        "consent_granted",
                        Some(format!("consent granted for {msg}")),
                        "consent",
                        risk_labels,
                        1.0,
                    ));
                }
            }
        }
    }

    Ok(CommandPolicyPreflight::Proceed {
        params: current_params,
        context,
        decisions,
    })
}

pub async fn run_command_policy_postflight(
    params: &crate::value::DictMap,
    result: VmValue,
    pre_context: JsonValue,
    decisions: Vec<CommandPolicyDecision>,
) -> Result<VmValue, VmError> {
    run_command_policy_postflight_with_ctx(None, params, result, pre_context, decisions).await
}

pub async fn run_command_policy_postflight_with_ctx(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    _params: &crate::value::DictMap,
    result: VmValue,
    pre_context: JsonValue,
    mut decisions: Vec<CommandPolicyDecision>,
) -> Result<VmValue, VmError> {
    let Some(policy) = current_command_policy() else {
        return Ok(result);
    };
    let Some(post) = policy.post.as_ref() else {
        return Ok(attach_policy_audit(result, pre_context, decisions, None));
    };
    let mut context = pre_context;
    let result_json = crate::llm::vm_value_to_json(&result);
    let mut scan_context = context.clone();
    if let Some(obj) = scan_context.as_object_mut() {
        obj.insert("result".to_string(), result_json.clone());
    }
    let post_scan = crate::llm::vm_value_to_json(&command_result_scan_value(
        &crate::stdlib::json_to_vm_value(&scan_context),
    )?);
    if let Some(obj) = context.as_object_mut() {
        obj.insert("result".to_string(), result_json);
        obj.insert("post_scan".to_string(), post_scan);
    }
    let action = invoke_command_hook(ctx, post, &context).await?;
    let (result, annotation) = parse_post_hook_action(action, result)?;
    if annotation.is_some() {
        decisions.push(decision(
            "annotate",
            Some("command result annotated by post-hook".to_string()),
            "post_hook",
            Vec::new(),
            1.0,
        ));
    }
    Ok(attach_policy_audit(result, context, decisions, annotation))
}

pub fn blocked_command_response(
    params: &crate::value::DictMap,
    status: &str,
    message: &str,
    context: JsonValue,
    decisions: Vec<CommandPolicyDecision>,
) -> VmValue {
    let command_id = format!("cmd_blocked_{}", crate::orchestration::new_id("policy"));
    let now = chrono::Utc::now().to_rfc3339();
    let mut result = BTreeMap::new();
    result.put_str("command_id", command_id.clone());
    result.put_str("status", status);
    result.insert("pid".to_string(), VmValue::Nil);
    result.insert("process_group_id".to_string(), VmValue::Nil);
    result.insert("handle_id".to_string(), VmValue::Nil);
    result.put_str("started_at", now.clone());
    result.put_str("ended_at", now);
    result.insert("duration_ms".to_string(), VmValue::Int(0));
    result.insert("exit_code".to_string(), VmValue::Int(-1));
    result.insert("signal".to_string(), VmValue::Nil);
    result.insert("timed_out".to_string(), VmValue::Bool(false));
    result.put_str("stdout", "");
    result.put_str("stderr", message);
    result.put_str("combined", message);
    result.insert("exit_status".to_string(), VmValue::Int(-1));
    result.insert("legacy_status".to_string(), VmValue::Int(-1));
    result.insert("success".to_string(), VmValue::Bool(false));
    result.put_str("error", "permission_denied");
    result.put_str("reason", message);
    result.put_str("audit_id", format!("audit_{command_id}"));
    result.insert(
        "request".to_string(),
        VmValue::dict(redacted_vm_request(params)),
    );
    attach_policy_audit(VmValue::dict(result), context, decisions, None)
}

fn attach_policy_audit(
    result: VmValue,
    context: JsonValue,
    decisions: Vec<CommandPolicyDecision>,
    annotation: Option<JsonValue>,
) -> VmValue {
    let Some(map) = result.as_dict() else {
        return result;
    };
    let mut out = (*map).clone();
    let mut audit = serde_json::json!({
        "context": context,
        "decisions": decisions.iter().map(decision_json).collect::<Vec<_>>(),
    });
    if let Some(annotation) = annotation {
        audit["annotation"] = annotation;
    }
    out.insert(
        crate::value::intern_key("command_policy"),
        crate::stdlib::json_to_vm_value(&audit),
    );
    VmValue::dict(out)
}

fn decision(
    action: &str,
    reason: Option<String>,
    source: &str,
    risk_labels: Vec<String>,
    confidence: f64,
) -> CommandPolicyDecision {
    CommandPolicyDecision {
        action: action.to_string(),
        reason,
        source: source.to_string(),
        risk_labels,
        confidence,
        display: None,
    }
}

fn decision_json(decision: &CommandPolicyDecision) -> JsonValue {
    serde_json::json!({
        "action": decision.action,
        "reason": decision.reason,
        "source": decision.source,
        "risk_labels": decision.risk_labels,
        "confidence": decision.confidence,
        "display": decision.display,
    })
}

async fn invoke_command_hook(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    closure: &Arc<VmClosure>,
    payload: &JsonValue,
) -> Result<VmValue, VmError> {
    let Some(mut vm) = ctx.map(crate::vm::AsyncBuiltinCtx::child_vm) else {
        return Err(VmError::Runtime(
            "command policy hook requires an async builtin VM context".to_string(),
        ));
    };
    COMMAND_POLICY_HOOK_DEPTH.with(|depth| *depth.borrow_mut() += 1);
    let _guard = HookDepthGuard;
    let arg = crate::stdlib::json_to_vm_value(payload);
    vm.call_closure_pub(closure, &[arg]).await
}

/// Outcome of routing a `require_approval` disposition through the policy's
/// optional consent gate (`CommandPolicy.consent`). Mirrors the
/// `std/llm/tool_middleware::with_consent` prompt_fn contract.
#[derive(Clone, Debug)]
enum ConsentVerdict {
    /// No consent gate is configured — preserve the legacy hard-block so the
    /// default (no consent) path stays byte-identical.
    NoGate,
    /// The consent callback approved the command; let it run.
    Approved,
    /// The consent callback (or a fail-closed default) denied the command.
    Denied(String),
}

/// Consult the policy's consent gate for a command that landed on a
/// `require_approval` disposition. The consent closure receives the command
/// context enriched with the deterministic classification (`consent.reason`
/// and `consent.risk_labels`) and returns the `with_consent` prompt_fn shape:
/// `true` / `{decision: "approved"}` to allow, `false` /
/// `{decision: "denied", reason?}` to deny. Any non-bool, non-dict verdict is
/// treated as a denial (fail closed), matching `with_consent`.
async fn command_consent_verdict(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    policy: &CommandPolicy,
    context: &JsonValue,
    risk_labels: &[String],
    reason: &str,
) -> Result<ConsentVerdict, VmError> {
    let Some(consent) = policy.consent.as_ref() else {
        return Ok(ConsentVerdict::NoGate);
    };
    let mut consent_ctx = context.clone();
    if let Some(obj) = consent_ctx.as_object_mut() {
        obj.insert(
            "consent".to_string(),
            serde_json::json!({
                "reason": reason,
                "risk_labels": risk_labels,
            }),
        );
    }
    let outcome = invoke_command_hook(ctx, consent, &consent_ctx).await?;
    Ok(parse_consent_outcome(outcome, reason))
}

fn parse_consent_outcome(value: VmValue, reason: &str) -> ConsentVerdict {
    match value {
        VmValue::Bool(true) => ConsentVerdict::Approved,
        VmValue::Bool(false) => ConsentVerdict::Denied(default_consent_denial(reason)),
        VmValue::Dict(map) => {
            let verdict = map
                .get("decision")
                .map(|value| value.display())
                .unwrap_or_else(|| "denied".to_string());
            if verdict == "denied" {
                let message = map
                    .get("reason")
                    .or_else(|| map.get("message"))
                    .map(|value| value.display())
                    .unwrap_or_else(|| default_consent_denial(reason));
                ConsentVerdict::Denied(message)
            } else {
                ConsentVerdict::Approved
            }
        }
        // Mirror `with_consent`: a verdict that is neither a bool nor a dict
        // carries no `decision` key, so it resolves to a denial. Fail closed.
        _ => ConsentVerdict::Denied(default_consent_denial(reason)),
    }
}

fn default_consent_denial(reason: &str) -> String {
    format!("consent denied: {reason}")
}

#[derive(Clone, Debug)]
enum ParsedPreHookAction {
    Allow,
    Deny(String),
    RequireApproval(String, Option<JsonValue>),
    Rewrite(crate::value::DictMap),
    DryRun(String),
    ExplainOnly(String),
}

fn parse_pre_hook_action(value: VmValue) -> Result<ParsedPreHookAction, VmError> {
    match value {
        VmValue::Nil => Ok(ParsedPreHookAction::Allow),
        VmValue::String(text) if text.as_str() == "allow" => Ok(ParsedPreHookAction::Allow),
        VmValue::Dict(map) => {
            if truthy(map.get("allow")) || map.get("action").is_some_and(|v| v.display() == "allow")
            {
                return Ok(ParsedPreHookAction::Allow);
            }
            if let Some(reason) = map.get("deny").or_else(|| {
                map.get("message")
                    .filter(|_| map.get("action").is_some_and(|v| v.display() == "deny"))
            }) {
                return Ok(ParsedPreHookAction::Deny(reason.display()));
            }
            if map
                .get("action")
                .is_some_and(|v| v.display() == "require_approval")
                || map.contains_key("require_approval")
            {
                let message = map
                    .get("reason")
                    .or_else(|| map.get("message"))
                    .or_else(|| map.get("require_approval"))
                    .map(|v| v.display())
                    .unwrap_or_else(|| "command requires approval".to_string());
                let display = map.get("display").map(crate::llm::vm_value_to_json);
                return Ok(ParsedPreHookAction::RequireApproval(message, display));
            }
            if map.get("action").is_some_and(|v| v.display() == "dry_run")
                || truthy(map.get("dry_run"))
            {
                return Ok(ParsedPreHookAction::DryRun(
                    map.get("reason")
                        .or_else(|| map.get("message"))
                        .map(|v| v.display())
                        .unwrap_or_else(|| "command dry-run requested by policy".to_string()),
                ));
            }
            if map
                .get("action")
                .is_some_and(|v| v.display() == "explain_only")
                || truthy(map.get("explain_only"))
            {
                return Ok(ParsedPreHookAction::ExplainOnly(
                    map.get("reason")
                        .or_else(|| map.get("message"))
                        .map(|v| v.display())
                        .unwrap_or_else(|| "command explanation requested by policy".to_string()),
                ));
            }
            if let Some(rewrite) = map.get("rewrite").or_else(|| map.get("request")) {
                let Some(rewrite) = rewrite.as_dict() else {
                    return Err(VmError::Runtime(
                        "command policy pre-hook rewrite must be a dict".to_string(),
                    ));
                };
                return Ok(ParsedPreHookAction::Rewrite(rewrite.clone()));
            }
            Ok(ParsedPreHookAction::Allow)
        }
        other => Err(VmError::Runtime(format!(
            "command policy pre-hook must return nil, 'allow', or a decision dict, got {}",
            other.type_name()
        ))),
    }
}

fn parse_post_hook_action(
    value: VmValue,
    current_result: VmValue,
) -> Result<(VmValue, Option<JsonValue>), VmError> {
    match value {
        VmValue::Nil => Ok((current_result, None)),
        VmValue::Dict(map) => {
            let mut result = current_result;
            if let Some(replacement) = map.get("result") {
                result = replacement.clone();
            }
            if let Some(feedback) = map.get("feedback").and_then(|v| v.as_dict()) {
                let session_id = feedback
                    .get("session_id")
                    .map(|v| v.display())
                    .or_else(crate::llm::current_agent_session_id);
                if let Some(session_id) = session_id {
                    let kind = feedback
                        .get("kind")
                        .map(|v| v.display())
                        .unwrap_or_else(|| "command_policy".to_string());
                    let content =
                        feedback
                            .get("content")
                            .map(|v| v.display())
                            .unwrap_or_else(|| {
                                crate::llm::vm_value_to_json(&VmValue::dict(feedback.clone()))
                                    .to_string()
                            });
                    crate::orchestration::agent_inbox::push(
                        &session_id,
                        &kind,
                        &content,
                        "orchestration.command_policy",
                    );
                }
            }
            let annotation = if map.contains_key("unsafe")
                || map.contains_key("annotations")
                || map.contains_key("audit")
            {
                Some(crate::llm::vm_value_to_json(&VmValue::Dict(map)))
            } else {
                None
            };
            Ok((result, annotation))
        }
        other => Err(VmError::Runtime(format!(
            "command policy post-hook must return nil or a dict, got {}",
            other.type_name()
        ))),
    }
}

fn apply_command_rewrite(
    params: &mut crate::value::DictMap,
    rewrite: &crate::value::DictMap,
) -> Result<(), VmError> {
    for (key, value) in rewrite {
        match key.as_str() {
            "mode" | "argv" | "command" | "shell" | "cwd" | "env" | "env_remove" | "env_mode"
            | "stdin" | "timeout" | "timeout_ms" | "capture" | "capture_stderr"
            | "max_inline_bytes" => {
                params.insert(key.clone(), value.clone());
            }
            other => {
                return Err(VmError::Runtime(format!(
                    "command policy rewrite cannot modify field {other:?}"
                )));
            }
        }
    }
    Ok(())
}

fn command_context_json(
    params: &crate::value::DictMap,
    policy: &CommandPolicy,
    caller: JsonValue,
) -> JsonValue {
    let request = command_request_json(params);
    let active_cwd = request
        .get("cwd")
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            crate::stdlib::process::execution_root_path()
                .display()
                .to_string()
        });
    let workspace_roots = if policy.workspace_roots.is_empty() {
        vec![crate::stdlib::process::execution_root_path()
            .display()
            .to_string()]
    } else {
        policy.workspace_roots.clone()
    };
    serde_json::json!({
        "request": request,
        "active_cwd": active_cwd,
        "workspace_roots": workspace_roots,
        "policy": {
            "default_shell_mode": policy.default_shell_mode,
            "deny_patterns": policy.deny_patterns,
            "require_approval": policy.require_approval.iter().cloned().collect::<Vec<_>>(),
            "deny_labels": policy.deny_labels.iter().cloned().collect::<Vec<_>>(),
            "ceiling": crate::orchestration::current_execution_policy(),
        },
        "tool_annotations": crate::orchestration::current_execution_policy()
            .map(|policy| policy.tool_annotations)
            .unwrap_or_default(),
        "transcript": {
            "summary": JsonValue::Null,
            "recent_messages": [],
            "redacted": true,
        },
        "caller": caller,
    })
}

fn command_request_json(params: &crate::value::DictMap) -> JsonValue {
    let mode = string_field_raw(params, "mode")
        .or_else(|| params.get("argv").map(|_| "argv".to_string()))
        .unwrap_or_else(|| "shell".to_string());
    let command = string_field_raw(params, "command");
    let argv = params.get("argv").and_then(|value| match value {
        VmValue::List(values) => Some(
            values
                .iter()
                .map(|value| value.display())
                .collect::<Vec<_>>(),
        ),
        _ => None,
    });
    let stdin = string_field_raw(params, "stdin").unwrap_or_default();
    let mut env_diff = JsonMap::new();
    if let Some(env) = params.get("env").and_then(|value| value.as_dict()) {
        for (key, value) in env.iter() {
            env_diff.insert(
                key.to_string(),
                serde_json::json!({
                    "present": true,
                    "redacted": true,
                    "value_sha256": sha256_hex(value.display().as_bytes()),
                }),
            );
        }
    }
    serde_json::json!({
        "mode": mode,
        "argv": argv,
        "command": command,
        "shell": params.get("shell").map(crate::llm::vm_value_to_json).unwrap_or(JsonValue::Null),
        "cwd": string_field_raw(params, "cwd").unwrap_or_else(|| crate::stdlib::process::execution_root_path().display().to_string()),
        "env_diff": env_diff,
        "env_mode": string_field_raw(params, "env_mode"),
        "stdin": {
            "size": stdin.len(),
            "sha256": if stdin.is_empty() { JsonValue::Null } else { JsonValue::String(sha256_hex(stdin.as_bytes())) },
        },
        "timeout_ms": params.get("timeout_ms").or_else(|| params.get("timeout")).and_then(vm_i64),
    })
}

mod catastrophic;
mod scan;

use scan::*;

pub fn command_risk_scan_json(ctx: &JsonValue, policy: Option<&CommandPolicy>) -> JsonValue {
    scan::scan_command_risk_scan_json(ctx, policy)
}

/// Universal catastrophic-command floor for resolved argv.
pub fn universal_catastrophic_reason(
    program: &str,
    args: &[String],
    workspace_roots: &[String],
) -> Option<String> {
    scan::scan_universal_catastrophic_reason(program, args, workspace_roots)
}

#[cfg(test)]
mod tests;
