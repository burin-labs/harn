use std::cell::RefCell;
use std::collections::BTreeMap;
use std::thread_local;

use serde::de::{Error as DeError, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use crate::tool_annotations::ToolAnnotations;
use crate::workspace_path::{WorkspacePathInfo, WorkspacePathKind};

use super::ToolApprovalPolicy;

mod host_request;
mod identity_match;
mod invocation_memory;
mod path_guards;
mod path_inputs;
mod remembered_paths;
mod rule_source;
mod sensitive_paths;
pub use host_request::{ToolApprovalRequest, ToolApprovalWorkspaceBoundary};
use identity_match::LiteralResourceIdentity;
pub use identity_match::PolicyIdentityMatch;
use path_guards::default_guard;
pub use path_guards::{
    denial_gate_for_source, EXTERNAL_ROOT_READ_ONLY, SOURCE_DEFAULT_EXTERNAL_PATH,
    SOURCE_DEFAULT_PATH_GUARD, SOURCE_DEFAULT_SENSITIVE_PATH, SOURCE_NET_POLICY,
};
pub use rule_source::PolicyRuleSource;

const POLICY_RECEIPT_TYPE: &str = "harn.permission_policy_decision.v1";

thread_local! {
    static APPROVAL_CALL_COUNTS: RefCell<BTreeMap<String, u64>> = const { RefCell::new(BTreeMap::new()) };
    static APPROVAL_UNAVAILABLE_CLASS_COUNTS: RefCell<BTreeMap<String, u64>> = const { RefCell::new(BTreeMap::new()) };
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
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
pub struct PolicyRuleMatch {
    /// Opaque exact-invocation scope constructed by Harn, never a glob.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invocation_sha256: Option<String>,
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
        alias = "env_modes",
        deserialize_with = "deserialize_string_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub env_mode: Vec<String>,
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
    /// Canonical persisted matcher names exposed to native host projections.
    pub const KEYS: &'static [&'static str] = &[
        "invocation_sha256",
        "tool",
        "tool_kind",
        "side_effect",
        "path",
        "command",
        "command_identity",
        "url",
        "domain",
        "method",
        "mcp_server",
        "mcp_tool",
        "agent",
        "persona",
        "mode",
        "env_mode",
        "capability",
    ];

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
            && self.env_mode.is_empty()
            && self.capability.is_empty()
            && self.repeat_count_at_least.is_none()
            && self.invocation_sha256.is_none()
    }

    fn matches(
        &self,
        ctx: &EvaluationContext,
        identity: PolicyIdentityMatch,
        action: PolicyAction,
    ) -> bool {
        (self.tool.is_empty() || identity.matches(&self.tool, std::slice::from_ref(&ctx.tool_name)))
            && self
                .invocation_sha256
                .as_ref()
                .is_none_or(|digest| ctx.invocation_sha256.as_ref() == Some(digest))
            && identity_match::resources_match(self, ctx, identity, action)
            && (self.command.is_empty() || identity.matches_command(&self.command, ctx))
            && identity_match::invocation_match(self, ctx, identity, action)
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
    #[serde(default, skip_serializing_if = "PolicyRuleSource::is_policy")]
    pub source: PolicyRuleSource,
    /// Captured values in a remembered request are literal, not authored patterns.
    #[serde(default, skip_serializing_if = "PolicyIdentityMatch::is_pattern")]
    pub identity_match: PolicyIdentityMatch,
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
        let source = raw
            .remove("source")
            .map(serde_json::from_value)
            .transpose()
            .map_err(M::Error::custom)?
            .unwrap_or_default();
        let approval = raw
            .remove("approval")
            .map(serde_json::from_value)
            .transpose()
            .map_err(M::Error::custom)?
            .unwrap_or_default();
        let identity_match = raw
            .remove("identity_match")
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
                if action.is_some() || matcher_value.is_some() {
                    return Err(M::Error::custom(
                        "policy rule must not mix action or a nested matcher with allow/ask/deny shorthand",
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
            source,
            identity_match,
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
    /// Actual grants whose combined scopes authorize the complete invocation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contributing_rules: Vec<PolicyMatchedRule>,
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
    /// The declared paths the deciding rule refused ON, when it refused on a
    /// path at all. Additive and empty for every other decision, so a reader
    /// and a host both get the subject of a path refusal as a typed value
    /// rather than having to parse it back out of the reason prose.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub denied_paths: Vec<String>,
    /// Network destinations refused by the deciding policy, as declared URLs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub denied_network_targets: Vec<String>,
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

    /// Rewrite this decision as an allow granted by the automated reviewer.
    ///
    /// The original action and reason are kept in the receipt under
    /// `auto_review`, so the record still says what the policy decided and who
    /// overrode it. An override that erased what it overrode would make the
    /// receipt agree with the outcome by construction and prove nothing.
    pub fn grant_by_auto_review(&mut self, rationale: &str) {
        let overridden_action =
            std::mem::replace(&mut self.action, PolicyAction::Allow.as_str().to_string());
        let overridden_reason = std::mem::replace(
            &mut self.reason,
            if rationale.is_empty() {
                "approved by the automated reviewer".to_string()
            } else {
                rationale.to_string()
            },
        );
        // The gate no longer requires a person, so leaving a required approval
        // shape behind would leave a downstream reader waiting for a prompt
        // that is never coming.
        self.required_approval = None;
        if let Some(map) = self.receipt.as_object_mut() {
            map.insert(
                "auto_review".to_string(),
                serde_json::json!({
                    "overridden_action": overridden_action,
                    "overridden_reason": overridden_reason,
                    "rationale": rationale,
                    "decider": "auto_reviewer",
                }),
            );
        }
    }

    pub fn has_audit_signal(&self) -> bool {
        self.matched_rule.is_some() || !self.risk_labels.is_empty()
    }
}

#[derive(Clone, Debug)]
struct EvaluationContext {
    invocation_sha256: Option<String>,
    tool_name: String,
    tool_kind: Option<String>,
    side_effect: Option<String>,
    capabilities: Vec<String>,
    path_entries: Vec<WorkspacePathInfo>,
    path_candidates: Vec<String>,
    command_candidates: Vec<String>,
    literal_command: Option<String>,
    literal_identity: Option<LiteralResourceIdentity>,
    command_identities: Vec<String>,
    urls: Vec<String>,
    domains: Vec<String>,
    http_methods: Vec<String>,
    mcp_servers: Vec<String>,
    mcp_tools: Vec<String>,
    agent: Option<String>,
    persona: Option<String>,
    mode: Option<String>,
    env_modes: Vec<String>,
    repeat_count: Option<u64>,
    /// The external roots governing this call's declared paths, with their
    /// modes, so the receipt states what access each root granted.
    external_roots: Vec<super::ExternalRoot>,
}

impl EvaluationContext {
    fn new(tool_name: &str, args: &JsonValue, repeat_count: Option<u64>) -> Self {
        let annotations = super::current_tool_annotations(tool_name);
        let path_entries = path_inputs::classify(
            args,
            annotations.as_ref(),
            &crate::orchestration::execution_root_path(),
        );
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
        let env_modes = string_values(args, &["env_mode", "envMode"]);
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

        let mut context = Self {
            invocation_sha256: None,
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
            command_candidates,
            literal_command: identity_match::literal_command(args),
            literal_identity: None,
            command_identities,
            urls,
            domains,
            http_methods,
            mcp_servers,
            mcp_tools,
            agent,
            persona,
            mode,
            env_modes,
            repeat_count,
            external_roots: Vec::new(),
        };
        context.literal_identity = Some(LiteralResourceIdentity::capture(&context, args, None));
        context.invocation_sha256 =
            invocation_memory::digest(&context, args, &crate::orchestration::execution_root_path());
        context
    }

    fn from_request(request: &ToolApprovalRequest) -> Self {
        host_request::request_context(request)
    }

    fn absorb_host_value(&mut self, value: &JsonValue) {
        self.capabilities
            .extend(string_values(value, &["capability", "capabilities"]));
        self.path_candidates.extend(path_values(value));

        let commands = string_values(
            value,
            &[
                "command",
                "command_identity",
                "command_identities",
                "operation",
            ],
        );
        for command in &commands {
            if let Some(identity) = shell_command_identity(command) {
                self.command_identities.push(identity);
            }
        }
        self.command_candidates.extend(commands);
        self.command_identities.extend(string_values(
            value,
            &["command_identity", "command_identities"],
        ));
        if let Some(argv) = value.get("argv").and_then(JsonValue::as_array) {
            let parts = argv
                .iter()
                .filter_map(JsonValue::as_str)
                .collect::<Vec<_>>();
            if !parts.is_empty() {
                self.command_candidates.push(parts.join(" "));
                self.command_identities.push(parts[0].to_string());
            }
        }

        self.urls.extend(string_values(value, &["url", "urls"]));
        self.domains
            .extend(string_values(value, &["domain", "domains"]));
        self.http_methods.extend(
            string_values(value, &["method", "http_method", "http_methods"])
                .into_iter()
                .map(|method| method.to_ascii_uppercase()),
        );
        self.mcp_servers
            .extend(string_values(value, &["mcp_server", "mcp_servers"]));
        self.mcp_tools
            .extend(string_values(value, &["mcp_tool", "mcp_tools"]));
        self.env_modes
            .extend(string_values(value, &["env_mode", "envMode"]));

        if let Some(entries) = value.get("paths").and_then(JsonValue::as_array) {
            self.path_entries.extend(
                entries
                    .iter()
                    .filter_map(|entry| serde_json::from_value(entry.clone()).ok()),
            );
        }
    }

    fn finish_host_normalization(&mut self) {
        for entry in &self.path_entries {
            self.path_candidates.extend(entry.policy_candidates());
        }
        for url in &self.urls {
            if let Ok(parsed) = url::Url::parse(url) {
                if matches!(parsed.scheme(), "http" | "https") {
                    if let Some(host) = parsed.host_str() {
                        self.domains.push(host.to_ascii_lowercase());
                    }
                }
            }
        }
        dedup(&mut self.capabilities);
        dedup(&mut self.path_candidates);
        dedup(&mut self.command_candidates);
        dedup(&mut self.command_identities);
        dedup(&mut self.urls);
        dedup(&mut self.domains);
        dedup(&mut self.http_methods);
        dedup(&mut self.mcp_servers);
        dedup(&mut self.mcp_tools);
        dedup(&mut self.env_modes);
    }

    fn tool_kinds(&self) -> Vec<String> {
        self.tool_kind.iter().cloned().collect()
    }

    fn invocation_constraints(&self) -> PolicyRuleMatch {
        PolicyRuleMatch {
            tool_kind: self.tool_kinds(),
            side_effect: self.side_effects(),
            command_identity: self.command_identities.clone(),
            http_method: self.http_methods.clone(),
            mcp_server: self.mcp_servers.clone(),
            mcp_tool: self.mcp_tools.clone(),
            agent: self.agent.iter().cloned().collect(),
            persona: self.persona.iter().cloned().collect(),
            mode: self.mode.iter().cloned().collect(),
            capability: self.capabilities.clone(),
            env_mode: self.env_modes.clone(),
            ..Default::default()
        }
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
            "env_modes": self.env_modes,
            "repeat_count": self.repeat_count,
            "external_roots": self.external_roots,
        })
    }
}

struct Candidate {
    source: String,
    source_rank: PolicyRuleSource,
    index: Option<usize>,
    id: Option<String>,
    action: PolicyAction,
    reason: String,
    approval: ApprovalShape,
    risk_labels: Vec<String>,
    /// The declared path this candidate refused, for the guards that refuse
    /// ON a path. Empty for every rule that matched on something else, so a
    /// reader can tell "no path was the reason" from "the path is in the
    /// prose somewhere".
    denied_paths: Vec<String>,
    contributing_rules: Vec<PolicyMatchedRule>,
}

impl Candidate {
    fn matched_rule(&self) -> PolicyMatchedRule {
        PolicyMatchedRule {
            source: self.source.clone(),
            action: self.action.as_str().to_string(),
            id: self.id.clone(),
            index: self.index,
            contributing_rules: self.contributing_rules.clone(),
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

pub fn next_approval_unavailable_class_repeat_count(
    session_id: &str,
    risk_labels: &[String],
) -> (String, u64) {
    let class = approval_unavailable_class(risk_labels);
    let key = format!("{session_id}:{class}");
    let repeat_count = APPROVAL_UNAVAILABLE_CLASS_COUNTS.with(|counts| {
        let mut counts = counts.borrow_mut();
        let count = counts.entry(key).or_insert(0);
        *count += 1;
        *count
    });
    (class, repeat_count)
}

pub fn clear_approval_policy_repeat_counts(session_id: &str) {
    let prefix = format!("{session_id}:");
    APPROVAL_CALL_COUNTS.with(|counts| {
        counts
            .borrow_mut()
            .retain(|key, _| !key.starts_with(prefix.as_str()));
    });
    APPROVAL_UNAVAILABLE_CLASS_COUNTS.with(|counts| {
        counts
            .borrow_mut()
            .retain(|key, _| !key.starts_with(prefix.as_str()));
    });
}

pub fn clear_all_approval_policy_repeat_counts() {
    APPROVAL_CALL_COUNTS.with(|counts| counts.borrow_mut().clear());
    APPROVAL_UNAVAILABLE_CLASS_COUNTS.with(|counts| counts.borrow_mut().clear());
}

fn approval_unavailable_class(risk_labels: &[String]) -> String {
    let labels = risk_labels
        .iter()
        .map(String::as_str)
        .filter(|label| !label.trim().is_empty())
        .collect::<std::collections::BTreeSet<_>>();
    if labels.is_empty() {
        "approval_required".to_string()
    } else {
        labels.into_iter().collect::<Vec<_>>().join("+")
    }
}

/// Refuse malformed path inputs before dispatch can request permission. The
/// catalog's explicit annotations take precedence over ambient annotations.
pub(crate) fn validate_tool_approval_path_arguments(
    tool_name: &str,
    args: &JsonValue,
    annotations: Option<&ToolAnnotations>,
) -> Result<(), String> {
    let ambient = annotations
        .is_none()
        .then(|| super::current_tool_annotations(tool_name))
        .flatten();
    let parameters = path_inputs::parameters(annotations.or(ambient.as_ref()));
    path_inputs::validate(args, &parameters)
}

pub fn evaluate_tool_approval_policy(
    policy: &ToolApprovalPolicy,
    tool_name: &str,
    args: &JsonValue,
    repeat_count: Option<u64>,
) -> PolicyEvaluation {
    let context = EvaluationContext::new(tool_name, args, repeat_count);
    if let Err(reason) = validate_tool_approval_path_arguments(tool_name, args, None) {
        return host_request::invalid_context(&context, reason);
    }
    evaluate_context(policy, context)
}

pub fn evaluate_tool_approval_request(
    policy: &ToolApprovalPolicy,
    request: &ToolApprovalRequest,
) -> PolicyEvaluation {
    if let Err(reason) = request.validate() {
        return host_request::invalid_request(request, reason);
    }
    evaluate_context(policy, EvaluationContext::from_request(request))
}

fn evaluate_context(policy: &ToolApprovalPolicy, mut ctx: EvaluationContext) -> PolicyEvaluation {
    ctx.external_roots =
        super::external_roots::governing_roots(&policy.external_roots, &ctx.path_entries);
    if let Some(default) = default_guard(policy, &ctx) {
        return evaluation_from_candidate(default, &ctx);
    }

    let mut candidates = Vec::new();
    candidates.extend(legacy_candidates(policy, &ctx));
    candidates.extend(rule_candidates(policy, &ctx));
    candidates.extend(remembered_paths::candidates(policy, &ctx));
    if let Some(repeat_limit) = policy.repeat_limit {
        if ctx.repeat_count.is_some_and(|count| count > repeat_limit) {
            let action = policy.repeat_action.unwrap_or(PolicyAction::Ask);
            candidates.push(Candidate {
                source: "repeat_limit".to_string(),
                source_rank: PolicyRuleSource::Policy,
                index: None,
                id: Some("repeat_limit".to_string()),
                action,
                reason: format!(
                    "tool '{}' repeated more than {repeat_limit} time(s) with the same arguments",
                    ctx.tool_name
                ),
                approval: ApprovalShape::default(),
                risk_labels: vec!["repeated_call".to_string()],
                denied_paths: Vec::new(),
                contributing_rules: Vec::new(),
            });
        }
    }

    if let Some(candidate) = strongest_candidate(candidates) {
        return evaluation_from_candidate(candidate, &ctx);
    }

    default_allow(&ctx)
}

fn legacy_candidates(policy: &ToolApprovalPolicy, ctx: &EvaluationContext) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for (index, pattern) in policy.auto_deny.iter().enumerate() {
        if super::super::glob_match(pattern, &ctx.tool_name) {
            candidates.push(Candidate {
                source: "auto_deny".to_string(),
                source_rank: PolicyRuleSource::Policy,
                index: Some(index),
                id: Some(pattern.clone()),
                action: PolicyAction::Deny,
                reason: format!("tool '{}' matches deny pattern '{pattern}'", ctx.tool_name),
                approval: ApprovalShape::default(),
                risk_labels: vec!["matched_deny_rule".to_string()],
                denied_paths: Vec::new(),
                contributing_rules: Vec::new(),
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
                    source_rank: PolicyRuleSource::Policy,
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
                    denied_paths: Vec::new(),
                    contributing_rules: Vec::new(),
                });
            }
        }
    }

    for (index, pattern) in policy.require_approval.iter().enumerate() {
        if super::super::glob_match(pattern, &ctx.tool_name) {
            candidates.push(Candidate {
                source: "require_approval".to_string(),
                source_rank: PolicyRuleSource::Policy,
                index: Some(index),
                id: Some(pattern.clone()),
                action: PolicyAction::Ask,
                reason: format!(
                    "tool '{}' matches approval pattern '{pattern}'",
                    ctx.tool_name
                ),
                approval: ApprovalShape::default(),
                risk_labels: vec!["approval_required".to_string()],
                denied_paths: Vec::new(),
                contributing_rules: Vec::new(),
            });
        }
    }

    for (index, pattern) in policy.auto_approve.iter().enumerate() {
        if super::super::glob_match(pattern, &ctx.tool_name) {
            candidates.push(Candidate {
                source: "auto_approve".to_string(),
                source_rank: PolicyRuleSource::Policy,
                index: Some(index),
                id: Some(pattern.clone()),
                action: PolicyAction::Allow,
                reason: format!("tool '{}' matches allow pattern '{pattern}'", ctx.tool_name),
                approval: ApprovalShape::default(),
                risk_labels: Vec::new(),
                denied_paths: Vec::new(),
                contributing_rules: Vec::new(),
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
        .filter(|(_, rule)| {
            (rule.matches.is_empty() || rule.matches.matches(ctx, rule.identity_match, rule.action))
                && host_request::exact_write_env_allow(rule, ctx)
        })
        .map(|(index, rule)| Candidate {
            source: rule.source.receipt_source().to_string(),
            source_rank: rule.source,
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
            denied_paths: Vec::new(),
            contributing_rules: Vec::new(),
        })
        .collect()
}

fn strongest_candidate(candidates: Vec<Candidate>) -> Option<Candidate> {
    // Source and action jointly express authority. An authored deny wins over
    // other configured candidates. A remembered choice beats a mode default.
    // Action strength breaks equal-priority conflicts. Strict comparison keeps the first rule
    // on a complete tie, so policy composition remains deterministic.
    let mut best: Option<Candidate> = None;
    for candidate in candidates {
        if best
            .as_ref()
            .map(|best| {
                (
                    candidate.source_rank.rank(candidate.action),
                    candidate.action.rank(),
                ) > (best.source_rank.rank(best.action), best.action.rank())
            })
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
        denied_paths: candidate.denied_paths,
        denied_network_targets: Vec::new(),
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
        denied_paths: Vec::new(),
        denied_network_targets: Vec::new(),
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

pub(crate) fn command_candidates(args: &JsonValue) -> (Vec<String>, Vec<String>) {
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
    if let Some((server, tool)) = tool_name
        .strip_prefix("mcp.")
        .and_then(|name| name.split_once('.'))
    {
        if !server.is_empty() && !tool.is_empty() {
            servers.push(server.to_string());
            tools.push(tool.to_string());
        }
    }
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

fn first_string(value: &JsonValue, keys: &[&str]) -> Option<String> {
    string_values(value, keys).into_iter().next()
}

fn string_values(value: &JsonValue, keys: &[&str]) -> Vec<String> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for key in keys {
        match object.get(*key) {
            Some(JsonValue::String(value)) if !value.trim().is_empty() => {
                values.push(value.clone());
            }
            Some(JsonValue::Array(items)) => {
                values.extend(
                    items
                        .iter()
                        .filter_map(JsonValue::as_str)
                        .filter(|value| !value.trim().is_empty())
                        .map(ToOwned::to_owned),
                );
            }
            _ => {}
        }
    }
    values
}

fn path_values(value: &JsonValue) -> Vec<String> {
    let mut paths = string_values(value, path_inputs::CONVENTIONAL_PATH_PARAMETERS);
    if let Some(entries) = value.get("paths").and_then(JsonValue::as_array) {
        for entry in entries {
            if let Some(path) = first_string(
                entry,
                &["workspace_path", "path", "host_absolute_path", "host_path"],
            ) {
                paths.push(path);
            }
        }
    }
    paths
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
            let Some((_, after)) = rest.split_once(part) else {
                return false;
            };
            rest = after;
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
    let canonical = crate::canonical_json::to_vec(value);
    let digest = Sha256::digest(&canonical);
    hex::encode(digest)
}

#[cfg(test)]
mod tests;
