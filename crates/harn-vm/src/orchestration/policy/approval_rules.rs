use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::thread_local;

use serde::de::{Error as DeError, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use crate::workspace_path::{WorkspacePathInfo, WorkspacePathKind};

use super::ToolApprovalPolicy;

const POLICY_RECEIPT_TYPE: &str = "harn.permission_policy_decision.v1";

thread_local! {
    static APPROVAL_CALL_COUNTS: RefCell<BTreeMap<String, u64>> = const { RefCell::new(BTreeMap::new()) };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAction {
    Allow,
    Ask,
    Deny,
}

impl PolicyAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Allow => 0,
            Self::Ask => 1,
            Self::Deny => 2,
        }
    }
}

impl<'de> Deserialize<'de> for PolicyAction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        parse_policy_action(&value).ok_or_else(|| {
            D::Error::custom(format!(
                "unsupported policy action {value:?}; expected allow, ask, require_approval, or deny"
            ))
        })
    }
}

fn parse_policy_action(value: &str) -> Option<PolicyAction> {
    match value {
        "allow" | "approve" | "auto_approve" => Some(PolicyAction::Allow),
        "ask" | "approval" | "require_approval" | "requires_approval" => Some(PolicyAction::Ask),
        "deny" | "block" | "auto_deny" => Some(PolicyAction::Deny),
        _ => None,
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApprovalShape {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reviewers: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub grant_options: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonValue>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyRuleMatch {
    #[serde(
        alias = "tools",
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tool: Vec<String>,
    #[serde(
        alias = "tool_kinds",
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tool_kind: Vec<String>,
    #[serde(
        alias = "side_effect_level",
        alias = "side_effect_levels",
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub side_effect: Vec<String>,
    #[serde(
        alias = "paths",
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub path: Vec<String>,
    #[serde(
        alias = "commands",
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub command: Vec<String>,
    #[serde(
        alias = "command_identities",
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub command_identity: Vec<String>,
    #[serde(
        alias = "urls",
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub url: Vec<String>,
    #[serde(
        alias = "domains",
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub domain: Vec<String>,
    #[serde(
        alias = "method",
        alias = "methods",
        alias = "http_methods",
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub http_method: Vec<String>,
    #[serde(
        alias = "mcp_servers",
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub mcp_server: Vec<String>,
    #[serde(
        alias = "mcp_tools",
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub mcp_tool: Vec<String>,
    #[serde(
        alias = "agents",
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub agent: Vec<String>,
    #[serde(
        alias = "personas",
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub persona: Vec<String>,
    #[serde(
        alias = "modes",
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub mode: Vec<String>,
    #[serde(
        alias = "capabilities",
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub capability: Vec<String>,
    #[serde(alias = "repeat_count_gte", alias = "repeat_at_least")]
    pub repeat_count_at_least: Option<u64>,
}

impl PolicyRuleMatch {
    fn from_shorthand(value: JsonValue) -> Result<Self, String> {
        match value {
            JsonValue::Null | JsonValue::Bool(true) => Ok(Self::default()),
            JsonValue::String(pattern) => Ok(Self {
                tool: vec![pattern],
                ..Default::default()
            }),
            JsonValue::Array(items) => {
                let mut tool = Vec::new();
                for item in items {
                    let Some(pattern) = item.as_str() else {
                        return Err(format!(
                            "policy rule shorthand list entries must be strings, got {item}"
                        ));
                    };
                    tool.push(pattern.to_string());
                }
                Ok(Self {
                    tool,
                    ..Default::default()
                })
            }
            JsonValue::Object(_) => {
                serde_json::from_value(value).map_err(|error| error.to_string())
            }
            other => Err(format!(
                "policy rule matcher must be a string, list, or dict, got {other}"
            )),
        }
    }

    fn is_empty(&self) -> bool {
        self.tool.is_empty()
            && self.tool_kind.is_empty()
            && self.side_effect.is_empty()
            && self.path.is_empty()
            && self.command.is_empty()
            && self.command_identity.is_empty()
            && self.url.is_empty()
            && self.domain.is_empty()
            && self.http_method.is_empty()
            && self.mcp_server.is_empty()
            && self.mcp_tool.is_empty()
            && self.agent.is_empty()
            && self.persona.is_empty()
            && self.mode.is_empty()
            && self.capability.is_empty()
            && self.repeat_count_at_least.is_none()
    }

    fn matches(&self, ctx: &EvaluationContext) -> bool {
        (self.tool.is_empty() || any_glob_matches(&self.tool, &[ctx.tool_name.clone()]))
            && (self.tool_kind.is_empty() || any_glob_matches(&self.tool_kind, &ctx.tool_kinds()))
            && (self.side_effect.is_empty()
                || any_glob_matches(&self.side_effect, &ctx.side_effects()))
            && (self.path.is_empty() || any_glob_matches(&self.path, &ctx.path_candidates))
            && (self.command.is_empty()
                || any_fragment_matches(&self.command, &ctx.command_candidates))
            && (self.command_identity.is_empty()
                || any_glob_matches(&self.command_identity, &ctx.command_identities))
            && (self.url.is_empty() || any_fragment_matches(&self.url, &ctx.urls))
            && (self.domain.is_empty() || any_glob_matches(&self.domain, &ctx.domains))
            && (self.http_method.is_empty()
                || any_glob_matches(
                    &normalize_patterns_upper(&self.http_method),
                    &ctx.http_methods,
                ))
            && (self.mcp_server.is_empty() || any_glob_matches(&self.mcp_server, &ctx.mcp_servers))
            && (self.mcp_tool.is_empty() || any_glob_matches(&self.mcp_tool, &ctx.mcp_tools))
            && (self.agent.is_empty()
                || ctx.agent.as_ref().is_some_and(|agent| {
                    any_glob_matches(&self.agent, std::slice::from_ref(agent))
                }))
            && (self.persona.is_empty()
                || ctx.persona.as_ref().is_some_and(|persona| {
                    any_glob_matches(&self.persona, std::slice::from_ref(persona))
                }))
            && (self.mode.is_empty()
                || ctx
                    .mode
                    .as_ref()
                    .is_some_and(|mode| any_glob_matches(&self.mode, std::slice::from_ref(mode))))
            && (self.capability.is_empty() || any_glob_matches(&self.capability, &ctx.capabilities))
            && self
                .repeat_count_at_least
                .map(|threshold| ctx.repeat_count.unwrap_or(0) >= threshold)
                .unwrap_or(true)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PolicyRule {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub action: PolicyAction,
    #[serde(rename = "match")]
    pub matches: PolicyRuleMatch,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "ApprovalShape::is_empty")]
    pub approval: ApprovalShape,
}

impl ApprovalShape {
    fn is_empty(&self) -> bool {
        self.prompt.is_none()
            && self.risk.is_none()
            && self.reviewers.is_empty()
            && self.grant_options.is_empty()
            && self.metadata.is_none()
    }
}

impl<'de> Deserialize<'de> for PolicyRule {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(PolicyRuleVisitor)
    }
}

struct PolicyRuleVisitor;

impl<'de> Visitor<'de> for PolicyRuleVisitor {
    type Value = PolicyRule;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a policy rule object")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut raw = serde_json::Map::new();
        while let Some((key, value)) = map.next_entry::<String, JsonValue>()? {
            raw.insert(key, value);
        }

        let id = raw
            .remove("id")
            .or_else(|| raw.remove("name"))
            .and_then(|value| value.as_str().map(ToOwned::to_owned));
        let reason = raw
            .remove("reason")
            .and_then(|value| value.as_str().map(ToOwned::to_owned));
        let approval = raw
            .remove("approval")
            .map(serde_json::from_value)
            .transpose()
            .map_err(M::Error::custom)?
            .unwrap_or_default();

        let mut action = match raw.remove("action") {
            Some(JsonValue::String(value)) => Some(parse_policy_action(&value).ok_or_else(|| {
                M::Error::custom(format!(
                    "unsupported policy action {value:?}; expected allow, ask, require_approval, or deny"
                ))
            })?),
            Some(other) => {
                return Err(M::Error::custom(format!(
                    "policy rule action must be a string, got {other}"
                )));
            }
            None => None,
        };
        let mut matcher_value = raw
            .remove("match")
            .or_else(|| raw.remove("matches"))
            .or_else(|| raw.remove("when"));

        for (key, candidate_action) in [
            ("deny", PolicyAction::Deny),
            ("ask", PolicyAction::Ask),
            ("require_approval", PolicyAction::Ask),
            ("allow", PolicyAction::Allow),
        ] {
            if let Some(value) = raw.remove(key) {
                if action.is_some() {
                    return Err(M::Error::custom(
                        "policy rule must not mix action with allow/ask/deny shorthand",
                    ));
                }
                action = Some(candidate_action);
                matcher_value = Some(value);
            }
        }

        if matcher_value.is_none() && !raw.is_empty() {
            matcher_value = Some(JsonValue::Object(raw));
        } else if matcher_value.is_some() && !raw.is_empty() {
            let mut fields = raw.keys().cloned().collect::<Vec<_>>();
            fields.sort();
            return Err(M::Error::custom(format!(
                "policy rule has matcher fields outside match/allow/ask/deny: {}",
                fields.join(", ")
            )));
        }

        let action = action.ok_or_else(|| {
            M::Error::custom("policy rule must include action or allow/ask/deny shorthand")
        })?;
        let matches = PolicyRuleMatch::from_shorthand(matcher_value.unwrap_or(JsonValue::Null))
            .map_err(M::Error::custom)?;
        Ok(PolicyRule {
            id,
            action,
            matches,
            reason,
            approval,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PolicyMatchedRule {
    pub source: String,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyEvaluation {
    pub action: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_rule: Option<PolicyMatchedRule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_approval: Option<ApprovalShape>,
    #[serde(default)]
    pub risk_labels: Vec<String>,
    pub receipt: JsonValue,
}

impl PolicyEvaluation {
    pub fn is_allow(&self) -> bool {
        self.action == PolicyAction::Allow.as_str()
    }

    pub fn is_ask(&self) -> bool {
        self.action == PolicyAction::Ask.as_str()
    }

    pub fn is_deny(&self) -> bool {
        self.action == PolicyAction::Deny.as_str()
    }

    pub fn has_audit_signal(&self) -> bool {
        self.matched_rule.is_some() || !self.risk_labels.is_empty()
    }
}

#[derive(Clone, Debug)]
struct EvaluationContext {
    tool_name: String,
    tool_kind: Option<String>,
    side_effect: Option<String>,
    capabilities: Vec<String>,
    path_entries: Vec<WorkspacePathInfo>,
    path_candidates: Vec<String>,
    string_candidates: Vec<String>,
    command_candidates: Vec<String>,
    command_identities: Vec<String>,
    urls: Vec<String>,
    domains: Vec<String>,
    http_methods: Vec<String>,
    mcp_servers: Vec<String>,
    mcp_tools: Vec<String>,
    agent: Option<String>,
    persona: Option<String>,
    mode: Option<String>,
    repeat_count: Option<u64>,
}

impl EvaluationContext {
    fn new(tool_name: &str, args: &JsonValue, repeat_count: Option<u64>) -> Self {
        let annotations = super::current_tool_annotations(tool_name);
        let path_entries = super::current_tool_declared_path_entries(tool_name, args);
        let mut path_candidates = Vec::new();
        for entry in &path_entries {
            path_candidates.extend(entry.policy_candidates());
        }
        dedup(&mut path_candidates);

        let mut string_candidates = Vec::new();
        collect_string_values(args, &mut string_candidates);
        dedup(&mut string_candidates);

        let (command_candidates, command_identities) = command_candidates(args);
        let (urls, domains) = url_candidates(&string_candidates);
        let http_methods = http_method_candidates(args);
        let (mcp_servers, mcp_tools) = mcp_candidates(tool_name, args);
        let dispatch = crate::triggers::dispatcher::current_dispatch_context();
        let agent = string_field(args, "agent")
            .or_else(|| string_field(args, "agent_id"))
            .or_else(|| dispatch.as_ref().map(|context| context.agent_id.clone()));
        let persona = string_field(args, "persona").or_else(|| string_field(args, "persona_id"));
        let mode = string_field(args, "mode")
            .or_else(|| string_field(args, "action"))
            .or_else(|| dispatch.as_ref().map(|context| context.action.clone()));
        let capabilities = annotations
            .as_ref()
            .map(|annotations| {
                annotations
                    .capabilities
                    .iter()
                    .flat_map(|(capability, ops)| {
                        ops.iter()
                            .map(|op| format!("{capability}.{op}"))
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Self {
            tool_name: tool_name.to_string(),
            tool_kind: annotations
                .as_ref()
                .map(|annotations| tool_kind_string(annotations.kind).to_string()),
            side_effect: annotations
                .as_ref()
                .map(|annotations| annotations.side_effect_level.as_str().to_string()),
            capabilities,
            path_entries,
            path_candidates,
            string_candidates,
            command_candidates,
            command_identities,
            urls,
            domains,
            http_methods,
            mcp_servers,
            mcp_tools,
            agent,
            persona,
            mode,
            repeat_count,
        }
    }

    fn tool_kinds(&self) -> Vec<String> {
        self.tool_kind.iter().cloned().collect()
    }

    fn side_effects(&self) -> Vec<String> {
        self.side_effect.iter().cloned().collect()
    }

    fn receipt_context(&self) -> JsonValue {
        serde_json::json!({
            "tool_name": self.tool_name,
            "tool_kind": self.tool_kind,
            "side_effect": self.side_effect,
            "capabilities": self.capabilities,
            "paths": self.path_entries.iter().map(path_entry_json).collect::<Vec<_>>(),
            "command_identities": self.command_identities,
            "urls": self.urls,
            "domains": self.domains,
            "http_methods": self.http_methods,
            "mcp_servers": self.mcp_servers,
            "mcp_tools": self.mcp_tools,
            "agent": self.agent,
            "persona": self.persona,
            "mode": self.mode,
            "repeat_count": self.repeat_count,
        })
    }
}

struct Candidate {
    source: String,
    index: Option<usize>,
    id: Option<String>,
    action: PolicyAction,
    reason: String,
    approval: ApprovalShape,
    risk_labels: Vec<String>,
}

impl Candidate {
    fn matched_rule(&self) -> PolicyMatchedRule {
        PolicyMatchedRule {
            source: self.source.clone(),
            action: self.action.as_str().to_string(),
            id: self.id.clone(),
            index: self.index,
        }
    }
}

pub fn next_approval_policy_repeat_count(
    session_id: &str,
    tool_name: &str,
    args: &JsonValue,
) -> u64 {
    let key = format!("{session_id}:{tool_name}:{}", stable_json_digest(args));
    APPROVAL_CALL_COUNTS.with(|counts| {
        let mut counts = counts.borrow_mut();
        let count = counts.entry(key).or_insert(0);
        *count += 1;
        *count
    })
}

pub fn clear_approval_policy_repeat_counts(session_id: &str) {
    let prefix = format!("{session_id}:");
    APPROVAL_CALL_COUNTS.with(|counts| {
        counts
            .borrow_mut()
            .retain(|key, _| !key.starts_with(prefix.as_str()));
    });
}

pub fn clear_all_approval_policy_repeat_counts() {
    APPROVAL_CALL_COUNTS.with(|counts| counts.borrow_mut().clear());
}

pub fn evaluate_tool_approval_policy(
    policy: &ToolApprovalPolicy,
    tool_name: &str,
    args: &JsonValue,
    repeat_count: Option<u64>,
) -> PolicyEvaluation {
    let ctx = EvaluationContext::new(tool_name, args, repeat_count);
    if let Some(default) = default_guard(policy, &ctx) {
        return evaluation_from_candidate(default, &ctx);
    }

    let mut candidates = Vec::new();
    candidates.extend(legacy_candidates(policy, &ctx));
    candidates.extend(rule_candidates(policy, &ctx));
    if let Some(repeat_limit) = policy.repeat_limit {
        if ctx.repeat_count.is_some_and(|count| count > repeat_limit) {
            let action = policy.repeat_action.unwrap_or(PolicyAction::Ask);
            candidates.push(Candidate {
                source: "repeat_limit".to_string(),
                index: None,
                id: Some("repeat_limit".to_string()),
                action,
                reason: format!(
                    "tool '{}' repeated more than {repeat_limit} time(s) with the same arguments",
                    ctx.tool_name
                ),
                approval: ApprovalShape::default(),
                risk_labels: vec!["repeated_call".to_string()],
            });
        }
    }

    if let Some(candidate) = strongest_candidate(candidates) {
        return evaluation_from_candidate(candidate, &ctx);
    }

    default_allow(&ctx)
}

fn default_guard(policy: &ToolApprovalPolicy, ctx: &EvaluationContext) -> Option<Candidate> {
    if !policy.allow_sensitive_paths {
        if let Some(path) = first_sensitive_candidate(policy, ctx) {
            return Some(Candidate {
                source: "default_sensitive_path".to_string(),
                index: None,
                id: Some("sensitive_path".to_string()),
                action: PolicyAction::Deny,
                reason: format!("path '{path}' is denied by the sensitive-path default"),
                approval: ApprovalShape::default(),
                risk_labels: vec!["sensitive_path".to_string()],
            });
        }
    }

    if !policy.allow_external_paths {
        for entry in &ctx.path_entries {
            if matches!(entry.kind, WorkspacePathKind::Invalid) {
                return Some(Candidate {
                    source: "default_path_guard".to_string(),
                    index: None,
                    id: Some("invalid_path".to_string()),
                    action: PolicyAction::Deny,
                    reason: entry
                        .reason
                        .clone()
                        .unwrap_or_else(|| format!("path '{}' is invalid", entry.display_path())),
                    approval: ApprovalShape::default(),
                    risk_labels: vec!["invalid_path".to_string()],
                });
            }
            if entry.workspace_path.is_none()
                && entry
                    .host_path
                    .as_ref()
                    .is_some_and(|path| !under_external_root(path, &policy.external_roots))
            {
                return Some(Candidate {
                    source: "default_external_path".to_string(),
                    index: None,
                    id: Some("external_path".to_string()),
                    action: PolicyAction::Deny,
                    reason: format!(
                        "path '{}' is outside the workspace and no external root allows it",
                        entry.display_path()
                    ),
                    approval: ApprovalShape::default(),
                    risk_labels: vec!["external_path".to_string()],
                });
            }
        }
    }

    None
}

fn legacy_candidates(policy: &ToolApprovalPolicy, ctx: &EvaluationContext) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for (index, pattern) in policy.auto_deny.iter().enumerate() {
        if super::super::glob_match(pattern, &ctx.tool_name) {
            candidates.push(Candidate {
                source: "auto_deny".to_string(),
                index: Some(index),
                id: Some(pattern.clone()),
                action: PolicyAction::Deny,
                reason: format!("tool '{}' matches deny pattern '{pattern}'", ctx.tool_name),
                approval: ApprovalShape::default(),
                risk_labels: vec!["matched_deny_rule".to_string()],
            });
        }
    }

    if !policy.write_path_allowlist.is_empty()
        && super::tool_kind_participates_in_write_allowlist(&ctx.tool_name)
    {
        for path in &ctx.path_entries {
            let allowed = policy.write_path_allowlist.iter().any(|pattern| {
                path.policy_candidates()
                    .iter()
                    .any(|candidate| super::super::glob_match(pattern, candidate))
            });
            if !allowed {
                candidates.push(Candidate {
                    source: "write_path_allowlist".to_string(),
                    index: None,
                    id: None,
                    action: PolicyAction::Deny,
                    reason: format!(
                        "tool '{}' targets '{}' which is not in the write-path allowlist",
                        ctx.tool_name,
                        path.display_path()
                    ),
                    approval: ApprovalShape::default(),
                    risk_labels: vec!["write_path_not_allowed".to_string()],
                });
            }
        }
    }

    for (index, pattern) in policy.require_approval.iter().enumerate() {
        if super::super::glob_match(pattern, &ctx.tool_name) {
            candidates.push(Candidate {
                source: "require_approval".to_string(),
                index: Some(index),
                id: Some(pattern.clone()),
                action: PolicyAction::Ask,
                reason: format!(
                    "tool '{}' matches approval pattern '{pattern}'",
                    ctx.tool_name
                ),
                approval: ApprovalShape::default(),
                risk_labels: vec!["approval_required".to_string()],
            });
        }
    }

    for (index, pattern) in policy.auto_approve.iter().enumerate() {
        if super::super::glob_match(pattern, &ctx.tool_name) {
            candidates.push(Candidate {
                source: "auto_approve".to_string(),
                index: Some(index),
                id: Some(pattern.clone()),
                action: PolicyAction::Allow,
                reason: format!("tool '{}' matches allow pattern '{pattern}'", ctx.tool_name),
                approval: ApprovalShape::default(),
                risk_labels: Vec::new(),
            });
        }
    }
    candidates
}

fn rule_candidates(policy: &ToolApprovalPolicy, ctx: &EvaluationContext) -> Vec<Candidate> {
    policy
        .rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| rule.matches.is_empty() || rule.matches.matches(ctx))
        .map(|(index, rule)| Candidate {
            source: "rules".to_string(),
            index: Some(index),
            id: rule.id.clone(),
            action: rule.action,
            reason: rule
                .reason
                .clone()
                .or_else(|| rule.approval.risk.clone())
                .unwrap_or_else(|| format!("tool '{}' matched policy rule", ctx.tool_name)),
            approval: rule.approval.clone(),
            risk_labels: risk_labels_for_rule(rule),
        })
        .collect()
}

fn strongest_candidate(candidates: Vec<Candidate>) -> Option<Candidate> {
    let mut best: Option<Candidate> = None;
    for candidate in candidates {
        if best
            .as_ref()
            .map(|best| candidate.action.rank() > best.action.rank())
            .unwrap_or(true)
        {
            best = Some(candidate);
        }
    }
    best
}

fn evaluation_from_candidate(candidate: Candidate, ctx: &EvaluationContext) -> PolicyEvaluation {
    let matched_rule = Some(candidate.matched_rule());
    let required_approval = (candidate.action == PolicyAction::Ask).then_some(candidate.approval);
    let mut risk_labels = candidate.risk_labels;
    risk_labels.sort();
    risk_labels.dedup();
    let receipt = receipt_json(
        candidate.action,
        &candidate.reason,
        matched_rule.as_ref(),
        required_approval.as_ref(),
        &risk_labels,
        ctx,
    );
    PolicyEvaluation {
        action: candidate.action.as_str().to_string(),
        reason: candidate.reason,
        matched_rule,
        required_approval,
        risk_labels,
        receipt,
    }
}

fn default_allow(ctx: &EvaluationContext) -> PolicyEvaluation {
    let action = PolicyAction::Allow;
    let reason = format!("tool '{}' approved by default", ctx.tool_name);
    let receipt = receipt_json(action, &reason, None, None, &[], ctx);
    PolicyEvaluation {
        action: action.as_str().to_string(),
        reason,
        matched_rule: None,
        required_approval: None,
        risk_labels: Vec::new(),
        receipt,
    }
}

fn receipt_json(
    action: PolicyAction,
    reason: &str,
    matched_rule: Option<&PolicyMatchedRule>,
    approval: Option<&ApprovalShape>,
    risk_labels: &[String],
    ctx: &EvaluationContext,
) -> JsonValue {
    serde_json::json!({
        "type": POLICY_RECEIPT_TYPE,
        "action": action.as_str(),
        "reason": reason,
        "matched_rule": matched_rule,
        "required_approval": approval,
        "risk_labels": risk_labels,
        "context": ctx.receipt_context(),
    })
}

fn risk_labels_for_rule(rule: &PolicyRule) -> Vec<String> {
    let mut labels = Vec::new();
    if rule.action == PolicyAction::Ask {
        labels.push("approval_required".to_string());
    }
    if rule.action == PolicyAction::Deny {
        labels.push("matched_deny_rule".to_string());
    }
    if !rule.matches.path.is_empty() {
        labels.push("path_rule".to_string());
    }
    if !rule.matches.command.is_empty() || !rule.matches.command_identity.is_empty() {
        labels.push("command_rule".to_string());
    }
    if !rule.matches.url.is_empty()
        || !rule.matches.domain.is_empty()
        || !rule.matches.http_method.is_empty()
    {
        labels.push("network_rule".to_string());
    }
    if !rule.matches.mcp_server.is_empty() || !rule.matches.mcp_tool.is_empty() {
        labels.push("mcp_rule".to_string());
    }
    if rule.matches.repeat_count_at_least.is_some() {
        labels.push("repeated_call".to_string());
    }
    labels
}

fn first_sensitive_candidate(
    policy: &ToolApprovalPolicy,
    ctx: &EvaluationContext,
) -> Option<String> {
    let custom = &policy.sensitive_path_patterns;
    ctx.path_candidates
        .iter()
        .chain(ctx.string_candidates.iter())
        .find(|candidate| {
            if custom.is_empty() {
                is_sensitive_path_candidate(
                    candidate,
                    DEFAULT_SENSITIVE_PATH_PATTERNS.iter().copied(),
                )
            } else {
                is_sensitive_path_candidate(candidate, custom.iter().map(String::as_str))
            }
        })
        .cloned()
}

fn is_sensitive_path_candidate<'a>(
    candidate: &str,
    patterns: impl IntoIterator<Item = &'a str>,
) -> bool {
    let normalized = candidate.replace('\\', "/").to_ascii_lowercase();
    let basename = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    patterns.into_iter().any(|pattern| {
        let pattern = pattern.to_ascii_lowercase();
        super::super::glob_match(&pattern, &normalized)
            || super::super::glob_match(&pattern, basename)
            || glob_or_contains(&pattern, &normalized)
    })
}

const DEFAULT_SENSITIVE_PATH_PATTERNS: &[&str] = &[
    ".env",
    ".env.*",
    "**/.env",
    "**/.env.*",
    "id_rsa",
    "id_ed25519",
    "**/.aws/credentials",
    "**/.npmrc",
    "**/.netrc",
    "*.pem",
    "*.key",
];

fn under_external_root(path: &str, roots: &[String]) -> bool {
    if roots.is_empty() {
        return false;
    }
    let path = normalize_path(Path::new(path));
    roots
        .iter()
        .map(|root| normalize_path(Path::new(root)))
        .any(|root| path.starts_with(root))
}

fn path_entry_json(entry: &WorkspacePathInfo) -> JsonValue {
    serde_json::json!({
        "input": entry.input,
        "kind": entry.kind,
        "normalized": entry.normalized,
        "workspace_path": entry.workspace_path,
        "host_path": entry.host_path,
        "recovered_root_drift": entry.recovered_root_drift,
        "reason": entry.reason,
    })
}

fn command_candidates(args: &JsonValue) -> (Vec<String>, Vec<String>) {
    let mut commands = Vec::new();
    let mut identities = Vec::new();
    if let Some(command) = string_field(args, "command").or_else(|| string_field(args, "cmd")) {
        commands.push(collapse_whitespace(&command));
        if let Some(identity) = shell_command_identity(&command) {
            identities.push(identity);
        }
    }
    if let Some(argv) = args.get("argv").and_then(|value| value.as_array()) {
        let parts = argv
            .iter()
            .filter_map(|value| value.as_str().map(ToOwned::to_owned))
            .collect::<Vec<_>>();
        if !parts.is_empty() {
            commands.push(parts.join(" "));
            identities.push(parts[0].clone());
        }
    }
    dedup(&mut commands);
    dedup(&mut identities);
    (commands, identities)
}

fn shell_command_identity(command: &str) -> Option<String> {
    command
        .split_whitespace()
        .next()
        .map(|part| part.trim_matches(|c| matches!(c, '"' | '\'')))
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
}

fn url_candidates(strings: &[String]) -> (Vec<String>, Vec<String>) {
    let mut urls = Vec::new();
    let mut domains = Vec::new();
    for candidate in strings {
        if let Ok(url) = url::Url::parse(candidate) {
            if matches!(url.scheme(), "http" | "https") {
                urls.push(url.to_string());
                if let Some(host) = url.host_str() {
                    domains.push(host.to_ascii_lowercase());
                }
            }
        }
    }
    dedup(&mut urls);
    dedup(&mut domains);
    (urls, domains)
}

fn http_method_candidates(args: &JsonValue) -> Vec<String> {
    let mut methods = Vec::new();
    for key in ["method", "http_method"] {
        if let Some(method) = string_field(args, key) {
            methods.push(method.to_ascii_uppercase());
        }
    }
    dedup(&mut methods);
    methods
}

fn mcp_candidates(tool_name: &str, args: &JsonValue) -> (Vec<String>, Vec<String>) {
    let mut servers = Vec::new();
    let mut tools = Vec::new();
    if let Some((server, tool)) = tool_name.split_once("__") {
        if !server.is_empty() && !tool.is_empty() {
            servers.push(server.to_string());
            tools.push(tool.to_string());
        }
    }
    for key in ["mcp_server", "_mcp_server", "server"] {
        if let Some(value) = string_field(args, key) {
            servers.push(value);
        }
    }
    for key in ["mcp_tool", "tool"] {
        if let Some(value) = string_field(args, key) {
            tools.push(value);
        }
    }
    dedup(&mut servers);
    dedup(&mut tools);
    (servers, tools)
}

fn string_field(args: &JsonValue, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
}

fn collect_string_values(value: &JsonValue, out: &mut Vec<String>) {
    match value {
        JsonValue::String(text) => out.push(text.clone()),
        JsonValue::Array(items) => {
            for item in items {
                collect_string_values(item, out);
            }
        }
        JsonValue::Object(map) => {
            for value in map.values() {
                collect_string_values(value, out);
            }
        }
        _ => {}
    }
}

fn any_glob_matches(patterns: &[String], candidates: &[String]) -> bool {
    candidates.iter().any(|candidate| {
        patterns
            .iter()
            .any(|pattern| super::super::glob_match(pattern, candidate))
    })
}

fn any_fragment_matches(patterns: &[String], candidates: &[String]) -> bool {
    candidates.iter().any(|candidate| {
        patterns
            .iter()
            .any(|pattern| glob_or_contains(pattern, candidate))
    })
}

fn glob_or_contains(pattern: &str, text: &str) -> bool {
    if super::super::glob_match(pattern, text) {
        return true;
    }
    if pattern.contains('*') {
        let mut rest = text;
        for part in pattern.split('*').filter(|part| !part.is_empty()) {
            let Some(index) = rest.find(part) else {
                return false;
            };
            rest = &rest[index + part.len()..];
        }
        true
    } else {
        text.contains(pattern)
    }
}

fn normalize_patterns_upper(patterns: &[String]) -> Vec<String> {
    patterns
        .iter()
        .map(|pattern| pattern.to_ascii_uppercase())
        .collect()
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_path(path: &Path) -> PathBuf {
    let raw = if path.is_absolute() {
        path.to_path_buf()
    } else {
        crate::stdlib::process::execution_root_path().join(path)
    };
    let mut out = PathBuf::new();
    for component in raw.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            std::path::Component::RootDir => out.push(component.as_os_str()),
            std::path::Component::Normal(part) => out.push(part),
        }
    }
    out
}

fn tool_kind_string(kind: crate::tool_annotations::ToolKind) -> &'static str {
    match kind {
        crate::tool_annotations::ToolKind::Read => "read",
        crate::tool_annotations::ToolKind::Edit => "edit",
        crate::tool_annotations::ToolKind::Delete => "delete",
        crate::tool_annotations::ToolKind::Move => "move",
        crate::tool_annotations::ToolKind::Search => "search",
        crate::tool_annotations::ToolKind::Execute => "execute",
        crate::tool_annotations::ToolKind::Think => "think",
        crate::tool_annotations::ToolKind::Fetch => "fetch",
        crate::tool_annotations::ToolKind::Other => "other",
    }
}

fn deserialize_string_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<JsonValue>::deserialize(deserializer)?.unwrap_or(JsonValue::Null);
    match value {
        JsonValue::Null => Ok(Vec::new()),
        JsonValue::String(value) => Ok(vec![value]),
        JsonValue::Array(items) => items
            .into_iter()
            .map(|item| match item {
                JsonValue::String(value) => Ok(value),
                other => Err(D::Error::custom(format!(
                    "expected string list item, got {other}"
                ))),
            })
            .collect(),
        other => Err(D::Error::custom(format!(
            "expected string or string list, got {other}"
        ))),
    }
}

fn dedup(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}

fn stable_json_digest(value: &JsonValue) -> String {
    let canonical = serde_json::to_string(value).unwrap_or_default();
    let digest = Sha256::digest(canonical.as_bytes());
    hex::encode(digest)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};
    use crate::tool_annotations::{SideEffectLevel, ToolAnnotations, ToolArgSchema, ToolKind};

    fn policy_with_path_annotation(tool: &str, kind: ToolKind) {
        let mut annotations = BTreeMap::new();
        annotations.insert(
            tool.to_string(),
            ToolAnnotations {
                kind,
                side_effect_level: match kind {
                    ToolKind::Fetch => SideEffectLevel::Network,
                    ToolKind::Execute => SideEffectLevel::ProcessExec,
                    ToolKind::Edit | ToolKind::Delete | ToolKind::Move => {
                        SideEffectLevel::WorkspaceWrite
                    }
                    _ => SideEffectLevel::ReadOnly,
                },
                arg_schema: ToolArgSchema {
                    path_params: vec!["path".to_string()],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        push_execution_policy(CapabilityPolicy {
            tool_annotations: annotations,
            ..Default::default()
        });
    }

    #[test]
    fn compact_rule_shorthand_deserializes() {
        let rule: PolicyRule = serde_json::from_value(serde_json::json!({
            "deny": {"tool": "read_*", "path": "**/.env"},
            "reason": "secret file"
        }))
        .expect("rule");
        assert_eq!(rule.action, PolicyAction::Deny);
        assert_eq!(rule.matches.tool, vec!["read_*"]);
        assert_eq!(rule.matches.path, vec!["**/.env"]);
        assert_eq!(rule.reason.as_deref(), Some("secret file"));
    }

    #[test]
    fn ambiguous_or_invalid_rule_shapes_are_rejected() {
        let invalid_action = serde_json::from_value::<PolicyRule>(serde_json::json!({
            "action": "maybe",
            "match": {"tool": "read_file"}
        }));
        assert!(invalid_action.is_err());

        let mixed_matchers = serde_json::from_value::<PolicyRule>(serde_json::json!({
            "action": "deny",
            "match": {"tool": "read_file"},
            "path": "**/.env"
        }));
        assert!(mixed_matchers.is_err());
    }

    #[test]
    fn deny_beats_ask_and_allow_regardless_of_order() {
        let policy: ToolApprovalPolicy = serde_json::from_value(serde_json::json!({
            "rules": [
                {"allow": {"tool": "write_file"}},
                {"ask": {"tool": "write_*"}},
                {"deny": {"tool": "write_file"}, "reason": "blocked"}
            ]
        }))
        .expect("policy");
        let decision =
            evaluate_tool_approval_policy(&policy, "write_file", &serde_json::json!({}), None);
        assert!(decision.is_deny());
        assert_eq!(decision.reason, "blocked");
        assert_eq!(
            decision.matched_rule.as_ref().and_then(|rule| rule.index),
            Some(2)
        );
    }

    #[test]
    fn sensitive_paths_are_denied_by_default() {
        let policy = ToolApprovalPolicy::default();
        let decision = evaluate_tool_approval_policy(
            &policy,
            "read_file",
            &serde_json::json!({"path": "config/.env"}),
            None,
        );
        assert!(decision.is_deny());
        assert!(decision.risk_labels.contains(&"sensitive_path".to_string()));
    }

    #[test]
    fn explicit_sensitive_opt_out_allows_regular_evaluation() {
        let policy = ToolApprovalPolicy {
            allow_sensitive_paths: true,
            ..Default::default()
        };
        let decision = evaluate_tool_approval_policy(
            &policy,
            "read_file",
            &serde_json::json!({"path": "config/.env"}),
            None,
        );
        assert!(decision.is_allow());
        assert!(!decision.has_audit_signal());
    }

    #[test]
    fn external_declared_paths_are_denied_without_root() {
        let temp = tempfile::tempdir().unwrap();
        crate::stdlib::process::set_thread_execution_context(Some(
            crate::orchestration::RunExecutionRecord {
                cwd: Some(temp.path().to_string_lossy().into_owned()),
                source_dir: Some(temp.path().to_string_lossy().into_owned()),
                env: BTreeMap::new(),
                adapter: None,
                repo_path: None,
                worktree_path: None,
                branch: None,
                base_ref: None,
                cleanup: None,
            },
        ));
        policy_with_path_annotation("read_file", ToolKind::Read);
        let decision = evaluate_tool_approval_policy(
            &ToolApprovalPolicy::default(),
            "read_file",
            &serde_json::json!({"path": "/tmp/outside.txt"}),
            None,
        );
        assert!(decision.is_deny());
        assert!(decision.risk_labels.contains(&"external_path".to_string()));
        pop_execution_policy();
        crate::stdlib::process::set_thread_execution_context(None);
    }

    #[test]
    fn path_rule_uses_declared_path_params() {
        policy_with_path_annotation("write_file", ToolKind::Edit);
        let policy: ToolApprovalPolicy = serde_json::from_value(serde_json::json!({
            "allow_sensitive_paths": true,
            "rules": [{"ask": {"tool": "write_*", "path": "src/**"}, "reason": "source edit"}]
        }))
        .expect("policy");
        let decision = evaluate_tool_approval_policy(
            &policy,
            "write_file",
            &serde_json::json!({"path": "src/lib.rs"}),
            None,
        );
        assert!(decision.is_ask());
        assert_eq!(decision.reason, "source edit");
        pop_execution_policy();
    }

    #[test]
    fn command_url_mcp_identity_and_repeat_rules_match() {
        let policy: ToolApprovalPolicy = serde_json::from_value(serde_json::json!({
            "allow_sensitive_paths": true,
            "rules": [
                {"ask": {"tool": "run_command", "command_identity": "npm"}},
                {"deny": {"tool": "fetch_url", "domain": "*.example.com", "method": "POST"}},
                {"deny": {"mcp_server": "github", "mcp_tool": "create_issue"}},
                {"deny": {"tool": "read_file", "repeat_count_gte": 3}}
            ]
        }))
        .expect("policy");
        assert!(evaluate_tool_approval_policy(
            &policy,
            "run_command",
            &serde_json::json!({"argv": ["npm", "install"]}),
            None,
        )
        .is_ask());
        assert!(evaluate_tool_approval_policy(
            &policy,
            "fetch_url",
            &serde_json::json!({"url": "https://api.example.com/v1", "method": "post"}),
            None,
        )
        .is_deny());
        assert!(evaluate_tool_approval_policy(
            &policy,
            "github__create_issue",
            &serde_json::json!({}),
            None,
        )
        .is_deny());
        assert!(evaluate_tool_approval_policy(
            &policy,
            "read_file",
            &serde_json::json!({"path": "README.md"}),
            Some(3),
        )
        .is_deny());
    }

    #[test]
    fn persona_agent_and_mode_rules_match_args() {
        let policy: ToolApprovalPolicy = serde_json::from_value(serde_json::json!({
            "allow_sensitive_paths": true,
            "rules": [{"deny": {"agent": "release-*", "persona": "shipper", "mode": "act"}}]
        }))
        .expect("policy");
        let decision = evaluate_tool_approval_policy(
            &policy,
            "publish",
            &serde_json::json!({"agent": "release-1", "persona": "shipper", "mode": "act"}),
            None,
        );
        assert!(decision.is_deny());
    }
}
