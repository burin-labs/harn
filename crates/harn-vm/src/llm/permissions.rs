use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::thread_local;

use serde::{Deserialize, Serialize};

use crate::llm::agent_tools::stable_hash;
use crate::trust_graph::{AutonomyTier, TrustOutcome, TrustRecord};
use crate::value::{VmError, VmValue};
use crate::workspace_anchor::{MountMode, WorkspaceAnchor};

const DEFAULT_PATH_SCOPE_ARG_KEYS: &[&str] = &["path", "destination", "source", "file"];

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
    PathScope(PathScopeMatcher),
    Predicate(VmValue),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct PathScopeMatcher {
    scope: PathScopeMode,
    arg_keys: Vec<String>,
    on_violation: PathScopeAction,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PathScopeMode {
    AnchorOnly,
    AnchorPlusMounted(MountModeFilter),
    Custom(Vec<String>),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct MountModeFilter {
    modes: Vec<MountMode>,
}

impl MountModeFilter {
    fn all() -> Self {
        Self {
            modes: vec![MountMode::ReadOnly, MountMode::Extend, MountMode::Sandboxed],
        }
    }

    fn allows(&self, mode: MountMode) -> bool {
        self.modes.contains(&mode)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PathScopeAction {
    Deny,
    ReminderOnly,
}

pub(crate) enum PermissionCheck {
    Granted { reason: String, escalated: bool },
    Denied { reason: String, escalated: bool },
}

/// Build the transcript event that callers append to a session when a
/// dynamic permission rule grants, denies, or escalates a tool call.
pub(crate) fn permission_transcript_event(
    kind: &str,
    tool_name: &str,
    args: &serde_json::Value,
    reason: &str,
    escalated: bool,
) -> VmValue {
    permission_transcript_event_with_policy(kind, tool_name, args, reason, escalated, None)
}

pub(crate) fn permission_transcript_event_with_policy(
    kind: &str,
    tool_name: &str,
    args: &serde_json::Value,
    reason: &str,
    escalated: bool,
    policy_decision: Option<serde_json::Value>,
) -> VmValue {
    let mut metadata = serde_json::json!({
        "tool_name": tool_name,
        "arguments": args,
        "reason": reason,
        "escalated": escalated,
    });
    if let Some(policy_decision) = policy_decision {
        metadata["policy_decision"] = policy_decision;
    }
    crate::llm::helpers::transcript_event(kind, "tool", "internal", reason, Some(metadata))
}

thread_local! {
    static DYNAMIC_PERMISSION_STACK: RefCell<Vec<DynamicPermissionPolicy>> = const { RefCell::new(Vec::new()) };
    static SESSION_PERMISSION_GRANTS: RefCell<BTreeMap<String, BTreeSet<String>>> = const {
        RefCell::new(BTreeMap::new())
    };
}

/// Pull the per-session "session"-scoped grant cache out of the store
/// for the duration of a single dispatch. Pair with [`store_session_grants`].
/// Caching across dispatch calls within a session ensures an
/// `on_escalation` callback returning `{grant: "session"}` only fires
/// once per session.
pub(crate) fn take_session_grants(session_id: &str) -> BTreeSet<String> {
    SESSION_PERMISSION_GRANTS
        .with(|store| store.borrow_mut().remove(session_id).unwrap_or_default())
}

pub(crate) fn store_session_grants(session_id: &str, grants: BTreeSet<String>) {
    SESSION_PERMISSION_GRANTS.with(|store| {
        store.borrow_mut().insert(session_id.to_string(), grants);
    });
}

/// Drop the session-scoped permission grant cache for a session, e.g. on
/// agent loop finalize.
pub(crate) fn clear_session_grants(session_id: &str) {
    SESSION_PERMISSION_GRANTS.with(|store| {
        store.borrow_mut().remove(session_id);
    });
}

pub(crate) fn push_dynamic_permission_policy(policy: DynamicPermissionPolicy) {
    DYNAMIC_PERMISSION_STACK.with(|stack| stack.borrow_mut().push(policy));
}

pub(crate) fn pop_dynamic_permission_policy() {
    DYNAMIC_PERMISSION_STACK.with(|stack| {
        stack.borrow_mut().pop();
    });
}

pub(crate) fn clear_dynamic_permission_state() {
    DYNAMIC_PERMISSION_STACK.with(|stack| stack.borrow_mut().clear());
    SESSION_PERMISSION_GRANTS.with(|store| store.borrow_mut().clear());
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
            if is_path_scope_matcher(map) {
                return Ok(PermissionMatcher::PathScope(parse_path_scope_matcher(
                    map, label,
                )?));
            }
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
                "{label}: dict matchers must be path_scope or include arg_key and patterns"
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

fn is_path_scope_matcher(map: &BTreeMap<String, VmValue>) -> bool {
    map.get("type")
        .or_else(|| map.get("kind"))
        .is_some_and(|value| value.display() == "path_scope")
}

fn parse_path_scope_matcher(
    map: &BTreeMap<String, VmValue>,
    label: &str,
) -> Result<PathScopeMatcher, VmError> {
    let arg_keys = match map
        .get("arg_keys")
        .or_else(|| map.get("keys"))
        .or_else(|| map.get("arg_key"))
    {
        Some(value) => non_empty_string_list(value, &format!("{label}.arg_keys"))?,
        None => DEFAULT_PATH_SCOPE_ARG_KEYS
            .iter()
            .map(|key| key.to_string())
            .collect(),
    };
    let on_violation = match map
        .get("on_violation")
        .or_else(|| map.get("violation"))
        .map(VmValue::display)
        .as_deref()
    {
        None | Some("deny") => PathScopeAction::Deny,
        Some("reminder_only") | Some("reminder") => PathScopeAction::ReminderOnly,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{label}.on_violation: unknown action '{other}'; expected deny or reminder_only"
            )));
        }
    };
    let scope = match map
        .get("scope")
        .or_else(|| map.get("mode"))
        .map(VmValue::display)
        .unwrap_or_else(|| "anchor_plus_mounted".to_string())
        .as_str()
    {
        "anchor_only" | "anchor" | "workspace_only" => PathScopeMode::AnchorOnly,
        "anchor_plus_mounted" | "mounted" => {
            let filter = match map
                .get("mount_modes")
                .or_else(|| map.get("mount_mode"))
                .or_else(|| map.get("modes"))
            {
                Some(value) => MountModeFilter {
                    modes: parse_mount_modes(value, &format!("{label}.mount_modes"))?,
                },
                None => MountModeFilter::all(),
            };
            PathScopeMode::AnchorPlusMounted(filter)
        }
        "custom" => {
            let patterns = map
                .get("patterns")
                .or_else(|| map.get("globs"))
                .ok_or_else(|| {
                    VmError::Runtime(format!(
                        "{label}.patterns: custom path_scope requires patterns"
                    ))
                })
                .and_then(|value| non_empty_string_list(value, &format!("{label}.patterns")))?;
            PathScopeMode::Custom(patterns)
        }
        other => {
            return Err(VmError::Runtime(format!(
                "{label}.scope: unknown scope '{other}'; expected anchor_only, anchor_plus_mounted, or custom"
            )));
        }
    };
    Ok(PathScopeMatcher {
        scope,
        arg_keys,
        on_violation,
    })
}

fn non_empty_string_list(value: &VmValue, label: &str) -> Result<Vec<String>, VmError> {
    let values = parse_pattern_list(value, label)?
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if values.is_empty() {
        return Err(VmError::Runtime(format!(
            "{label}: expected at least one non-empty string"
        )));
    }
    Ok(values)
}

fn parse_mount_modes(value: &VmValue, label: &str) -> Result<Vec<MountMode>, VmError> {
    non_empty_string_list(value, label)?
        .into_iter()
        .map(|mode| {
            MountMode::parse(&mode)
                .map_err(|message| VmError::Runtime(format!("{label}: {message}")))
        })
        .collect()
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
        // Cached session grant: the escalation callback already fired
        // (and was logged) the first time; subsequent calls only emit a
        // PermissionGrant.
        return Ok(PermissionCheck::Granted {
            reason: format!(
                "permission granted by prior session-scoped escalation for tool '{tool_name}'"
            ),
            escalated: false,
        });
    }

    let denied = first_matching_deny_rule(&policy.deny, tool_name, args, session_id).await?;
    let allowed = if policy.allow.is_empty() {
        None
    } else {
        Some(first_matching_allow_rule(&policy.allow, tool_name, args, session_id).await?)
    };

    let denial_reason = if let Some(reason) = denied {
        Some(format!("permission denied by deny rule: {reason}"))
    } else {
        match allowed.as_ref() {
            Some(AllowRuleMatch::Matched(_)) | None => None,
            Some(AllowRuleMatch::NoMatch) => Some(format!(
                "permission denied: tool '{tool_name}' is not allowed by this agent's permissions"
            )),
            Some(AllowRuleMatch::Rejected(reason)) => Some(format!("permission denied: {reason}")),
        }
    };

    let Some(reason) = denial_reason else {
        let allow_reason = match allowed {
            Some(AllowRuleMatch::Matched(reason)) => {
                format!("permission granted by allow rule: {reason}")
            }
            Some(AllowRuleMatch::NoMatch | AllowRuleMatch::Rejected(_)) | None => {
                format!("permission granted for tool '{tool_name}'")
            }
        };
        return Ok(PermissionCheck::Granted {
            reason: allow_reason,
            escalated: false,
        });
    };

    let ask_payload = serde_json::json!({
        "event": crate::orchestration::HookEvent::PermissionAsked.as_str(),
        "session": {"id": session_id},
        "tool": {"name": tool_name, "args": args},
        "reason": &reason,
    });
    match crate::orchestration::run_lifecycle_hooks_with_control(
        crate::orchestration::HookEvent::PermissionAsked,
        &ask_payload,
    )
    .await?
    {
        crate::orchestration::HookControl::Allow => {}
        crate::orchestration::HookControl::Block {
            reason: block_reason,
        } => {
            return Ok(PermissionCheck::Denied {
                reason: block_reason,
                escalated: true,
            });
        }
        crate::orchestration::HookControl::Decision {
            kind,
            reason: decision_reason,
        } => match kind.as_str() {
            "allow" => {
                let grant_reason = decision_reason
                    .unwrap_or_else(|| format!("permission granted by session hook: {reason}"));
                session_grants.insert(grant_key);
                fire_permission_replied(session_id, tool_name, args, "allow", &grant_reason).await;
                return Ok(PermissionCheck::Granted {
                    reason: grant_reason,
                    escalated: true,
                });
            }
            "deny" => {
                let denied_reason = decision_reason
                    .unwrap_or_else(|| format!("permission denied by session hook: {reason}"));
                fire_permission_replied(session_id, tool_name, args, "deny", &denied_reason).await;
                return Ok(PermissionCheck::Denied {
                    reason: denied_reason,
                    escalated: true,
                });
            }
            _ => {}
        },
        // `PermissionAsked` does not support payload Modify; ignore.
        crate::orchestration::HookControl::Modify { .. } => {}
    }

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
        let grant_reason = response.reason.unwrap_or(reason);
        fire_permission_replied(session_id, tool_name, args, "allow", &grant_reason).await;
        Ok(PermissionCheck::Granted {
            reason: grant_reason,
            escalated: true,
        })
    } else {
        let deny_reason = response
            .reason
            .unwrap_or_else(|| "permission escalation denied".to_string());
        fire_permission_replied(session_id, tool_name, args, "deny", &deny_reason).await;
        Ok(PermissionCheck::Denied {
            reason: deny_reason,
            escalated: true,
        })
    }
}

async fn fire_permission_replied(
    session_id: &str,
    tool_name: &str,
    args: &serde_json::Value,
    decision: &str,
    reason: &str,
) {
    let payload = serde_json::json!({
        "event": crate::orchestration::HookEvent::PermissionReplied.as_str(),
        "session": {"id": session_id},
        "tool": {"name": tool_name, "args": args},
        "decision": decision,
        "reason": reason,
    });
    if let Err(err) = crate::orchestration::run_lifecycle_hooks(
        crate::orchestration::HookEvent::PermissionReplied,
        &payload,
    )
    .await
    {
        crate::events::log_warn(
            "agent.permission_replied_hook",
            &format!("session={session_id} tool={tool_name} hook error: {err}"),
        );
    }
}

enum AllowRuleMatch {
    Matched(String),
    Rejected(String),
    NoMatch,
}

struct MatcherEvaluation {
    matched: bool,
    reason: Option<String>,
    rejection: Option<String>,
}

async fn first_matching_deny_rule(
    rules: &[PermissionRule],
    tool_name: &str,
    args: &serde_json::Value,
    session_id: &str,
) -> Result<Option<String>, VmError> {
    for rule in rules {
        if !crate::orchestration::glob_match(&rule.tool_pattern, tool_name) {
            continue;
        }
        let evaluation = evaluate_matcher(&rule.matcher, args, session_id).await?;
        if matches!(rule.matcher, PermissionMatcher::PathScope(_)) {
            if let Some(reason) = evaluation.rejection {
                return Ok(Some(reason));
            }
            continue;
        }
        if evaluation.matched {
            return Ok(Some(
                evaluation
                    .reason
                    .unwrap_or_else(|| rule.tool_pattern.clone()),
            ));
        }
    }
    Ok(None)
}

async fn first_matching_allow_rule(
    rules: &[PermissionRule],
    tool_name: &str,
    args: &serde_json::Value,
    session_id: &str,
) -> Result<AllowRuleMatch, VmError> {
    for rule in rules {
        if !crate::orchestration::glob_match(&rule.tool_pattern, tool_name) {
            continue;
        }
        let evaluation = evaluate_matcher(&rule.matcher, args, session_id).await?;
        if let Some(rejection) = evaluation.rejection {
            return Ok(AllowRuleMatch::Rejected(rejection));
        }
        if evaluation.matched {
            return Ok(AllowRuleMatch::Matched(
                evaluation
                    .reason
                    .unwrap_or_else(|| rule.tool_pattern.clone()),
            ));
        }
    }
    Ok(AllowRuleMatch::NoMatch)
}

async fn evaluate_matcher(
    matcher: &PermissionMatcher,
    args: &serde_json::Value,
    session_id: &str,
) -> Result<MatcherEvaluation, VmError> {
    match matcher {
        PermissionMatcher::Any => Ok(MatcherEvaluation::matched()),
        PermissionMatcher::Bool(value) => Ok(MatcherEvaluation::from_bool(*value)),
        PermissionMatcher::Patterns(patterns) => Ok(MatcherEvaluation::from_bool(
            args_match_any_pattern(args, patterns),
        )),
        PermissionMatcher::KeyedPatterns { arg_key, patterns } => Ok(MatcherEvaluation::from_bool(
            args.get(arg_key)
                .and_then(|value| value.as_str())
                .is_some_and(|value| {
                    patterns
                        .iter()
                        .any(|pattern| crate::orchestration::glob_match(pattern, value))
                }),
        )),
        PermissionMatcher::PathScope(matcher) => {
            Ok(evaluate_path_scope_matcher(matcher, args, session_id))
        }
        PermissionMatcher::Predicate(value) => {
            let VmValue::Closure(closure) = value else {
                return Ok(MatcherEvaluation::from_bool(false));
            };
            let Some(mut vm) = crate::vm::clone_async_builtin_child_vm() else {
                return Err(VmError::Runtime(
                    "permissions predicate requires an async builtin VM context".to_string(),
                ));
            };
            let result = vm
                .call_closure_pub(closure, &[crate::stdlib::json_to_vm_value(args)])
                .await?;
            Ok(MatcherEvaluation::from_bool(value_truthy(&result)))
        }
    }
}

impl MatcherEvaluation {
    fn matched() -> Self {
        Self {
            matched: true,
            reason: None,
            rejection: None,
        }
    }

    fn from_bool(value: bool) -> Self {
        Self {
            matched: value,
            reason: None,
            rejection: None,
        }
    }

    fn rejected(reason: String) -> Self {
        Self {
            matched: false,
            reason: None,
            rejection: Some(reason),
        }
    }

    fn reminder(reason: String) -> Self {
        Self {
            matched: true,
            reason: Some(format!("path scope reminder: {reason}")),
            rejection: None,
        }
    }
}

fn evaluate_path_scope_matcher(
    matcher: &PathScopeMatcher,
    args: &serde_json::Value,
    session_id: &str,
) -> MatcherEvaluation {
    let path_args = collect_path_scope_args(args, &matcher.arg_keys);
    if path_args.is_empty() {
        return MatcherEvaluation::from_bool(false);
    }

    for path_arg in path_args {
        if let Some(reason) = path_scope_violation_reason(&path_arg.value, matcher, session_id) {
            return match matcher.on_violation {
                PathScopeAction::Deny => MatcherEvaluation::rejected(reason),
                PathScopeAction::ReminderOnly => MatcherEvaluation::reminder(reason),
            };
        }
    }
    MatcherEvaluation::matched()
}

struct PathScopeArg {
    value: String,
}

fn collect_path_scope_args(args: &serde_json::Value, arg_keys: &[String]) -> Vec<PathScopeArg> {
    let mut out = Vec::new();
    if let serde_json::Value::String(text) = args {
        if arg_keys.iter().any(|key| key == "path") {
            out.push(PathScopeArg {
                value: text.clone(),
            });
        }
        return out;
    }
    collect_path_scope_args_inner(args, arg_keys, &mut out);
    out
}

fn collect_path_scope_args_inner(
    value: &serde_json::Value,
    arg_keys: &[String],
    out: &mut Vec<PathScopeArg>,
) {
    match value {
        serde_json::Value::String(_) => {}
        serde_json::Value::Array(values) => {
            for value in values {
                collect_path_scope_args_inner(value, arg_keys, out);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if arg_keys.iter().any(|arg_key| arg_key == key) {
                    collect_string_leaves(value, out);
                } else {
                    collect_path_scope_args_inner(value, arg_keys, out);
                }
            }
        }
        _ => {}
    }
}

fn collect_string_leaves(value: &serde_json::Value, out: &mut Vec<PathScopeArg>) {
    match value {
        serde_json::Value::String(text) => out.push(PathScopeArg {
            value: text.clone(),
        }),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_string_leaves(value, out);
            }
        }
        serde_json::Value::Object(map) => {
            for value in map.values() {
                collect_string_leaves(value, out);
            }
        }
        _ => {}
    }
}

fn path_scope_violation_reason(
    path: &str,
    matcher: &PathScopeMatcher,
    session_id: &str,
) -> Option<String> {
    match &matcher.scope {
        PathScopeMode::Custom(patterns) => {
            if custom_path_scope_allows(path, patterns) {
                None
            } else {
                Some(format!(
                    "path '{}' is outside custom path scope (patterns: {})",
                    path,
                    patterns.join(", ")
                ))
            }
        }
        PathScopeMode::AnchorOnly | PathScopeMode::AnchorPlusMounted(_) => {
            let Some(anchor) = crate::agent_sessions::workspace_anchor(session_id) else {
                return Some(format!(
                    "path '{path}' cannot be checked because session '{session_id}' has no workspace anchor"
                ));
            };
            anchor_path_scope_violation(path, &matcher.scope, &anchor)
        }
    }
}

fn custom_path_scope_allows(path: &str, patterns: &[String]) -> bool {
    let info = crate::workspace_path::classify_workspace_path(path, None);
    info.policy_candidates().iter().any(|candidate| {
        patterns
            .iter()
            .any(|pattern| crate::orchestration::glob_match(pattern, candidate))
    })
}

/// Public surface used by the `path_scope_guard` PreToolUse hook
/// (#2221) and sub-agent anchor validation (#2223). Returns `None` when
/// `path` is inside the anchor (or one of the writable mounted roots
/// when `accept_modes` is non-empty) and `Some(reason)` otherwise.
/// `accept_modes` empty means "primary anchor only".
pub fn anchor_scope_violation(
    path: &str,
    anchor: &WorkspaceAnchor,
    accept_modes: &[MountMode],
) -> Option<String> {
    let scope = if accept_modes.is_empty() {
        PathScopeMode::AnchorOnly
    } else {
        PathScopeMode::AnchorPlusMounted(MountModeFilter {
            modes: accept_modes.to_vec(),
        })
    };
    anchor_path_scope_violation(path, &scope, anchor)
}

fn anchor_path_scope_violation(
    path: &str,
    scope: &PathScopeMode,
    anchor: &WorkspaceAnchor,
) -> Option<String> {
    let primary = crate::workspace_path::classify_workspace_path(path, Some(&anchor.primary));
    if let Some(reason) = primary.reason.as_deref() {
        return Some(format!(
            "path '{}' is invalid for anchor {}: {}",
            path,
            anchor.primary.display(),
            reason
        ));
    }
    if primary.workspace_path.is_some() {
        return None;
    }

    if let PathScopeMode::AnchorPlusMounted(filter) = scope {
        for root in &anchor.additional_roots {
            if !filter.allows(root.mount_mode) {
                continue;
            }
            let mounted = crate::workspace_path::classify_workspace_path(path, Some(&root.path));
            if mounted.reason.is_none() && mounted.workspace_path.is_some() {
                return None;
            }
        }
    }

    Some(format!(
        "path '{}' is outside anchor {} (mounted roots: {})",
        path,
        anchor.primary.display(),
        mounted_roots_summary(anchor)
    ))
}

fn mounted_roots_summary(anchor: &WorkspaceAnchor) -> String {
    if anchor.additional_roots.is_empty() {
        return "none".to_string();
    }
    anchor
        .additional_roots
        .iter()
        .map(|root| format!("{} [{}]", root.path.display(), root.mount_mode.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
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

fn session_grant_key(scope_index: usize, tool_name: &str, args: &serde_json::Value) -> String {
    format!("{scope_index}:{tool_name}:{}", stable_hash(args))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::workspace_anchor::MountedRoot;

    use super::*;

    fn anchor(base: &Path) -> WorkspaceAnchor {
        WorkspaceAnchor {
            primary: base.join("harn-anchor-a"),
            additional_roots: vec![
                MountedRoot {
                    path: base.join("harn-anchor-b"),
                    mount_mode: MountMode::Extend,
                    mounted_at: "2026-05-24T00:00:00Z".to_string(),
                },
                MountedRoot {
                    path: base.join("harn-anchor-c"),
                    mount_mode: MountMode::ReadOnly,
                    mounted_at: "2026-05-24T00:00:00Z".to_string(),
                },
            ],
            anchored_at: "2026-05-24T00:00:00Z".to_string(),
        }
    }

    fn path_string(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn path_scope_matcher_round_trips_through_serde() {
        let matcher = PathScopeMatcher {
            scope: PathScopeMode::AnchorPlusMounted(MountModeFilter {
                modes: vec![MountMode::Extend],
            }),
            arg_keys: vec!["path".to_string(), "destination".to_string()],
            on_violation: PathScopeAction::Deny,
        };
        let encoded = serde_json::to_string(&matcher).expect("serialize");
        let decoded: PathScopeMatcher = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, matcher);
    }

    #[test]
    fn path_scope_allows_extend_mounts_and_rejects_filtered_modes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let anchor = anchor(temp.path());
        let scope = PathScopeMode::AnchorPlusMounted(MountModeFilter {
            modes: vec![MountMode::Extend],
        });
        let extend_path = path_string(&anchor.additional_roots[0].path.join("file.txt"));
        assert!(anchor_path_scope_violation(&extend_path, &scope, &anchor).is_none());
        let read_only_root = anchor.additional_roots[1].path.display().to_string();
        let read_only_path = path_string(&anchor.additional_roots[1].path.join("file.txt"));
        let reason = anchor_path_scope_violation(&read_only_path, &scope, &anchor)
            .expect("read-only root is outside writable scope");
        assert!(
            reason.contains(&format!("{read_only_root} [read_only]")),
            "{reason}"
        );
    }

    #[test]
    fn path_scope_rejects_absolute_paths_outside_anchor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let anchor = anchor(temp.path());
        let outside_path = path_string(&temp.path().join("harn-anchor-z").join("file.txt"));
        let extend_root = anchor.additional_roots[0].path.display().to_string();
        let reason =
            anchor_path_scope_violation(&outside_path, &PathScopeMode::AnchorOnly, &anchor)
                .expect("outside path is rejected");
        assert!(reason.contains(&format!("path '{outside_path}' is outside anchor")));
        assert!(reason.contains(&format!("{extend_root} [extend]")));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn path_scope_allow_violation_is_not_overridden_by_later_allow() {
        crate::reset_thread_local_state();
        let temp = tempfile::tempdir().expect("tempdir");
        let anchor = anchor(temp.path());
        let mounted_path = path_string(&anchor.additional_roots[0].path.join("file.txt"));
        let session_id =
            crate::agent_sessions::open_or_create(Some("path-scope-terminal-allow".to_string()));
        crate::agent_sessions::set_workspace_anchor(&session_id, Some(anchor)).expect("set anchor");
        let rules = vec![
            PermissionRule {
                tool_pattern: "Read".to_string(),
                matcher: PermissionMatcher::PathScope(PathScopeMatcher {
                    scope: PathScopeMode::AnchorOnly,
                    arg_keys: vec!["path".to_string()],
                    on_violation: PathScopeAction::Deny,
                }),
            },
            PermissionRule {
                tool_pattern: "*".to_string(),
                matcher: PermissionMatcher::Any,
            },
        ];
        let result = first_matching_allow_rule(
            &rules,
            "Read",
            &serde_json::json!({"path": mounted_path}),
            &session_id,
        )
        .await
        .expect("check allow rules");
        assert!(
            matches!(result, AllowRuleMatch::Rejected(reason) if reason.contains("outside anchor"))
        );
    }
}
