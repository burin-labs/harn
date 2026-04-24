use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::thread_local;

use crate::llm::agent_tools::stable_hash;
use crate::trust_graph::{AutonomyTier, TrustOutcome, TrustRecord};
use crate::value::{VmError, VmValue};

#[derive(Clone)]
pub(crate) struct DynamicPermissionPolicy {
    allow: Vec<PermissionRule>,
    deny: Vec<PermissionRule>,
    on_escalation: Option<VmValue>,
}

#[derive(Clone)]
struct PermissionRule {
    tool_pattern: String,
    matcher: PermissionMatcher,
}

#[derive(Clone)]
enum PermissionMatcher {
    Any,
    Bool(bool),
    Patterns(Vec<String>),
    KeyedPatterns {
        arg_key: String,
        patterns: Vec<String>,
    },
    Predicate(VmValue),
}

pub(crate) enum PermissionCheck {
    Granted { reason: String, escalated: bool },
    Denied { reason: String, escalated: bool },
}

thread_local! {
    static DYNAMIC_PERMISSION_STACK: RefCell<Vec<DynamicPermissionPolicy>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn push_dynamic_permission_policy(policy: DynamicPermissionPolicy) {
    DYNAMIC_PERMISSION_STACK.with(|stack| stack.borrow_mut().push(policy));
}

pub(crate) fn pop_dynamic_permission_policy() {
    DYNAMIC_PERMISSION_STACK.with(|stack| {
        stack.borrow_mut().pop();
    });
}

pub(crate) fn current_dynamic_permission_policies() -> Vec<DynamicPermissionPolicy> {
    DYNAMIC_PERMISSION_STACK.with(|stack| stack.borrow().clone())
}

pub(crate) fn parse_dynamic_permission_policy(
    value: Option<&VmValue>,
    label: &str,
) -> Result<Option<DynamicPermissionPolicy>, VmError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if matches!(value, VmValue::Nil) {
        return Ok(None);
    }
    let dict = value
        .as_dict()
        .ok_or_else(|| VmError::Runtime(format!("{label}: permissions must be a dict")))?;
    let allow = parse_rule_set(dict.get("allow"), &format!("{label}.allow"))?;
    let deny = parse_rule_set(dict.get("deny"), &format!("{label}.deny"))?;
    let on_escalation = dict
        .get("on_escalation")
        .filter(|value| matches!(value, VmValue::Closure(_)))
        .cloned();
    Ok(Some(DynamicPermissionPolicy {
        allow,
        deny,
        on_escalation,
    }))
}

fn parse_rule_set(value: Option<&VmValue>, label: &str) -> Result<Vec<PermissionRule>, VmError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    match value {
        VmValue::Nil => Ok(Vec::new()),
        VmValue::List(list) => {
            let mut rules = Vec::new();
            for item in list.iter() {
                let VmValue::String(pattern) = item else {
                    return Err(VmError::Runtime(format!(
                        "{label}: list entries must be strings"
                    )));
                };
                let pattern = pattern.trim();
                if !pattern.is_empty() {
                    rules.push(PermissionRule {
                        tool_pattern: pattern.to_string(),
                        matcher: PermissionMatcher::Any,
                    });
                }
            }
            Ok(rules)
        }
        VmValue::Dict(map) => {
            let mut rules = Vec::new();
            for (tool_pattern, matcher) in map.iter() {
                if tool_pattern.trim().is_empty() {
                    continue;
                }
                rules.push(PermissionRule {
                    tool_pattern: tool_pattern.clone(),
                    matcher: parse_matcher(matcher, &format!("{label}.{tool_pattern}"))?,
                });
            }
            Ok(rules)
        }
        other => Err(VmError::Runtime(format!(
            "{label}: expected a list or dict, got {}",
            other.type_name()
        ))),
    }
}

fn parse_matcher(value: &VmValue, label: &str) -> Result<PermissionMatcher, VmError> {
    match value {
        VmValue::Nil => Ok(PermissionMatcher::Any),
        VmValue::Bool(value) => Ok(PermissionMatcher::Bool(*value)),
        VmValue::String(pattern) => Ok(PermissionMatcher::Patterns(vec![pattern.to_string()])),
        VmValue::Closure(_) => Ok(PermissionMatcher::Predicate(value.clone())),
        VmValue::List(list) => {
            let mut patterns = Vec::new();
            for item in list.iter() {
                let VmValue::String(pattern) = item else {
                    return Err(VmError::Runtime(format!(
                        "{label}: pattern list entries must be strings"
                    )));
                };
                patterns.push(pattern.to_string());
            }
            Ok(PermissionMatcher::Patterns(patterns))
        }
        VmValue::Dict(map) => {
            let arg_key = map
                .get("arg_key")
                .or_else(|| map.get("key"))
                .map(|value| value.display())
                .filter(|value| !value.trim().is_empty());
            let patterns_value = map
                .get("patterns")
                .or_else(|| map.get("arg_patterns"))
                .or_else(|| map.get("allow"));
            if let (Some(arg_key), Some(patterns_value)) = (arg_key, patterns_value) {
                let patterns = parse_pattern_list(patterns_value, label)?;
                return Ok(PermissionMatcher::KeyedPatterns { arg_key, patterns });
            }
            Err(VmError::Runtime(format!(
                "{label}: dict matchers must include arg_key and patterns"
            )))
        }
        other => Err(VmError::Runtime(format!(
            "{label}: unsupported matcher type {}",
            other.type_name()
        ))),
    }
}

fn parse_pattern_list(value: &VmValue, label: &str) -> Result<Vec<String>, VmError> {
    match value {
        VmValue::String(pattern) => Ok(vec![pattern.to_string()]),
        VmValue::List(list) => list
            .iter()
            .map(|item| match item {
                VmValue::String(pattern) => Ok(pattern.to_string()),
                _ => Err(VmError::Runtime(format!(
                    "{label}: patterns must be a string or list of strings"
                ))),
            })
            .collect(),
        _ => Err(VmError::Runtime(format!(
            "{label}: patterns must be a string or list of strings"
        ))),
    }
}

pub(crate) async fn check_dynamic_permission(
    session_grants: &mut BTreeSet<String>,
    tool_name: &str,
    args: &serde_json::Value,
    session_id: &str,
) -> Result<Option<PermissionCheck>, VmError> {
    let policies = current_dynamic_permission_policies();
    if policies.is_empty() {
        return Ok(None);
    }

    let mut grant_result: Option<PermissionCheck> = None;
    for (index, policy) in policies.iter().enumerate() {
        match check_one_dynamic_permission(
            policy,
            index,
            session_grants,
            tool_name,
            args,
            session_id,
        )
        .await?
        {
            PermissionCheck::Denied { reason, escalated } => {
                return Ok(Some(PermissionCheck::Denied { reason, escalated }));
            }
            grant @ PermissionCheck::Granted { .. } => {
                grant_result = Some(grant);
            }
        }
    }
    Ok(grant_result)
}

async fn check_one_dynamic_permission(
    policy: &DynamicPermissionPolicy,
    scope_index: usize,
    session_grants: &mut BTreeSet<String>,
    tool_name: &str,
    args: &serde_json::Value,
    session_id: &str,
) -> Result<PermissionCheck, VmError> {
    let grant_key = session_grant_key(scope_index, tool_name, args);
    if session_grants.contains(&grant_key) {
        return Ok(PermissionCheck::Granted {
            reason: "session grant".to_string(),
            escalated: false,
        });
    }

    let denied = first_matching_rule(&policy.deny, tool_name, args).await?;
    let allowed = if policy.allow.is_empty() {
        None
    } else {
        Some(first_matching_rule(&policy.allow, tool_name, args).await?)
    };

    let denial_reason = if let Some(reason) = denied {
        Some(format!("permission denied by deny rule: {reason}"))
    } else {
        match allowed {
            Some(Some(_)) | None => None,
            Some(None) => Some(format!(
                "permission denied: tool '{tool_name}' is not allowed by this agent's permissions"
            )),
        }
    };

    let Some(reason) = denial_reason else {
        return Ok(PermissionCheck::Granted {
            reason: "permission allow rule matched".to_string(),
            escalated: false,
        });
    };

    let Some(on_escalation) = policy.on_escalation.as_ref() else {
        return Ok(PermissionCheck::Denied {
            reason,
            escalated: false,
        });
    };

    let request = permission_request_value(tool_name, args, session_id, &reason);
    let response = invoke_escalation_callback(on_escalation, &request).await?;
    if response.granted {
        emit_tier_promotion_if_needed(tool_name, args, response.approver.clone()).await;
        if matches!(response.scope, GrantScope::Session) {
            session_grants.insert(grant_key);
        }
        Ok(PermissionCheck::Granted {
            reason: response
                .reason
                .unwrap_or_else(|| "permission escalation granted".to_string()),
            escalated: true,
        })
    } else {
        Ok(PermissionCheck::Denied {
            reason: response
                .reason
                .unwrap_or_else(|| "permission escalation denied".to_string()),
            escalated: true,
        })
    }
}

async fn first_matching_rule(
    rules: &[PermissionRule],
    tool_name: &str,
    args: &serde_json::Value,
) -> Result<Option<String>, VmError> {
    for rule in rules {
        if !crate::orchestration::glob_match(&rule.tool_pattern, tool_name) {
            continue;
        }
        if matcher_allows(&rule.matcher, args).await? {
            return Ok(Some(rule.tool_pattern.clone()));
        }
    }
    Ok(None)
}

async fn matcher_allows(
    matcher: &PermissionMatcher,
    args: &serde_json::Value,
) -> Result<bool, VmError> {
    match matcher {
        PermissionMatcher::Any => Ok(true),
        PermissionMatcher::Bool(value) => Ok(*value),
        PermissionMatcher::Patterns(patterns) => Ok(args_match_any_pattern(args, patterns)),
        PermissionMatcher::KeyedPatterns { arg_key, patterns } => Ok(args
            .get(arg_key)
            .and_then(|value| value.as_str())
            .is_some_and(|value| {
                patterns
                    .iter()
                    .any(|pattern| crate::orchestration::glob_match(pattern, value))
            })),
        PermissionMatcher::Predicate(value) => {
            let VmValue::Closure(closure) = value else {
                return Ok(false);
            };
            let Some(mut vm) = crate::vm::clone_async_builtin_child_vm() else {
                return Err(VmError::Runtime(
                    "permissions predicate requires an async builtin VM context".to_string(),
                ));
            };
            let result = vm
                .call_closure_pub(closure, &[crate::stdlib::json_to_vm_value(args)])
                .await?;
            Ok(value_truthy(&result))
        }
    }
}

fn args_match_any_pattern(args: &serde_json::Value, patterns: &[String]) -> bool {
    let mut values = Vec::new();
    collect_string_values(args, &mut values);
    if values.is_empty() {
        values.push(serde_json::to_string(args).unwrap_or_default());
    }
    values.iter().any(|candidate| {
        patterns
            .iter()
            .any(|pattern| crate::orchestration::glob_match(pattern, candidate))
    })
}

fn collect_string_values(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => out.push(text.clone()),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_string_values(value, out);
            }
        }
        serde_json::Value::Object(map) => {
            for value in map.values() {
                collect_string_values(value, out);
            }
        }
        _ => {}
    }
}

fn value_truthy(value: &VmValue) -> bool {
    match value {
        VmValue::Nil => false,
        VmValue::Bool(value) => *value,
        VmValue::Int(value) => *value != 0,
        VmValue::Float(value) => *value != 0.0,
        VmValue::String(value) => !value.is_empty(),
        VmValue::List(value) => !value.is_empty(),
        VmValue::Dict(value) => !value.is_empty(),
        _ => true,
    }
}

#[derive(Clone, Copy)]
enum GrantScope {
    Once,
    Session,
}

struct EscalationResponse {
    granted: bool,
    scope: GrantScope,
    reason: Option<String>,
    approver: Option<String>,
}

async fn invoke_escalation_callback(
    callback: &VmValue,
    request: &VmValue,
) -> Result<EscalationResponse, VmError> {
    let VmValue::Closure(closure) = callback else {
        return Ok(EscalationResponse {
            granted: false,
            scope: GrantScope::Once,
            reason: Some("permission escalation callback is not callable".to_string()),
            approver: None,
        });
    };
    let Some(mut vm) = crate::vm::clone_async_builtin_child_vm() else {
        return Err(VmError::Runtime(
            "permissions on_escalation requires an async builtin VM context".to_string(),
        ));
    };
    parse_escalation_response(vm.call_closure_pub(closure, &[request.clone()]).await?)
}

fn parse_escalation_response(value: VmValue) -> Result<EscalationResponse, VmError> {
    match value {
        VmValue::Bool(true) => Ok(EscalationResponse {
            granted: true,
            scope: GrantScope::Once,
            reason: None,
            approver: None,
        }),
        VmValue::Bool(false) | VmValue::Nil => Ok(EscalationResponse {
            granted: false,
            scope: GrantScope::Once,
            reason: None,
            approver: None,
        }),
        VmValue::String(scope) => grant_scope_from_string(scope.as_ref()).map(|scope| {
            EscalationResponse {
                granted: true,
                scope,
                reason: None,
                approver: None,
            }
        }),
        VmValue::Dict(map) => {
            let grant = map.get("grant").or_else(|| map.get("granted"));
            let (granted, scope) = match grant {
                Some(VmValue::Bool(false)) => (false, GrantScope::Once),
                Some(VmValue::Bool(true)) => (true, GrantScope::Once),
                Some(VmValue::String(scope)) => (true, grant_scope_from_string(scope.as_ref())?),
                Some(other) => {
                    return Err(VmError::Runtime(format!(
                        "permissions on_escalation grant must be false, true, 'once', or 'session', got {}",
                        other.type_name()
                    )))
                }
                None => (false, GrantScope::Once),
            };
            Ok(EscalationResponse {
                granted,
                scope,
                reason: map
                    .get("reason")
                    .map(|value| value.display())
                    .filter(|value| !value.is_empty()),
                approver: map
                    .get("approver")
                    .map(|value| value.display())
                    .filter(|value| !value.is_empty()),
            })
        }
        other => Err(VmError::Runtime(format!(
            "permissions on_escalation must return false, true, 'once', 'session', or {{grant}}, got {}",
            other.type_name()
        ))),
    }
}

fn grant_scope_from_string(value: &str) -> Result<GrantScope, VmError> {
    match value {
        "once" => Ok(GrantScope::Once),
        "session" => Ok(GrantScope::Session),
        "deny" | "denied" | "false" => Err(VmError::Runtime(
            "permissions on_escalation string denial must return false".to_string(),
        )),
        other => Err(VmError::Runtime(format!(
            "permissions on_escalation unsupported grant scope '{other}'"
        ))),
    }
}

fn permission_request_value(
    tool_name: &str,
    args: &serde_json::Value,
    session_id: &str,
    reason: &str,
) -> VmValue {
    let mut request = BTreeMap::new();
    request.insert(
        "_type".to_string(),
        VmValue::String(Rc::from("PermissionRequest")),
    );
    request.insert(
        "tool".to_string(),
        VmValue::String(Rc::from(tool_name.to_string())),
    );
    request.insert("args".to_string(), crate::stdlib::json_to_vm_value(args));
    request.insert(
        "session_id".to_string(),
        VmValue::String(Rc::from(session_id.to_string())),
    );
    request.insert(
        "reason".to_string(),
        VmValue::String(Rc::from(reason.to_string())),
    );
    if let Some(context) = crate::triggers::dispatcher::current_dispatch_context() {
        request.insert(
            "agent".to_string(),
            VmValue::String(Rc::from(context.agent_id)),
        );
        request.insert(
            "action".to_string(),
            VmValue::String(Rc::from(context.action)),
        );
        request.insert(
            "trace_id".to_string(),
            VmValue::String(Rc::from(context.trigger_event.trace_id.0)),
        );
        request.insert(
            "autonomy_tier".to_string(),
            VmValue::String(Rc::from(context.autonomy_tier.as_str())),
        );
        if matches!(
            context.autonomy_tier,
            AutonomyTier::Shadow | AutonomyTier::Suggest
        ) {
            request.insert(
                "requested_tier".to_string(),
                VmValue::String(Rc::from(AutonomyTier::ActWithApproval.as_str())),
            );
        }
    }
    request.insert(
        "grant_options".to_string(),
        VmValue::List(Rc::new(vec![
            VmValue::String(Rc::from("once")),
            VmValue::String(Rc::from("session")),
            VmValue::Bool(false),
        ])),
    );
    VmValue::Dict(Rc::new(request))
}

async fn emit_tier_promotion_if_needed(
    tool_name: &str,
    args: &serde_json::Value,
    approver: Option<String>,
) {
    let Some(context) = crate::triggers::dispatcher::current_dispatch_context() else {
        return;
    };
    if !matches!(
        context.autonomy_tier,
        AutonomyTier::Shadow | AutonomyTier::Suggest
    ) {
        return;
    }
    let mut record = TrustRecord::new(
        context.agent_id,
        "trust.promote",
        approver,
        TrustOutcome::Success,
        context.trigger_event.trace_id.0,
        AutonomyTier::ActWithApproval,
    );
    record.metadata.insert(
        "reason".to_string(),
        serde_json::json!("permission escalation granted"),
    );
    record
        .metadata
        .insert("tool".to_string(), serde_json::json!(tool_name));
    record.metadata.insert("args".to_string(), args.clone());
    record.metadata.insert(
        "from_tier".to_string(),
        serde_json::json!(context.autonomy_tier.as_str()),
    );
    record.metadata.insert(
        "to_tier".to_string(),
        serde_json::json!(AutonomyTier::ActWithApproval.as_str()),
    );
    if let Err(error) = crate::trust_graph::append_active_trust_record(&record).await {
        crate::events::log_warn(
            "permissions.trust_graph",
            &format!("failed to append permission escalation trust record: {error}"),
        );
    }
}

pub(crate) fn permission_transcript_event(
    kind: &str,
    tool_name: &str,
    args: &serde_json::Value,
    reason: &str,
    escalated: bool,
) -> VmValue {
    crate::llm::helpers::transcript_event(
        kind,
        "tool",
        "internal",
        reason,
        Some(serde_json::json!({
            "tool_name": tool_name,
            "arguments": args,
            "reason": reason,
            "escalated": escalated,
        })),
    )
}

fn session_grant_key(scope_index: usize, tool_name: &str, args: &serde_json::Value) -> String {
    format!("{scope_index}:{tool_name}:{}", stable_hash(args))
}
