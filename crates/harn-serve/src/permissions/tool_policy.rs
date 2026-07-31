//! Typed tool-permission rule evaluation for native Harn hosts.
//!
//! Hosts own policy persistence and approval presentation, but the meaning of
//! tool metadata and the rule-selection algorithm belong to Harn. This module
//! is the boundary: a host projects its stored rules and an ACP permission
//! request into these small types, then renders the returned decision.

use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value};

/// Stable receipt stamped on every decision so hosts can prove the Harn path
/// fired rather than a legacy local evaluator.
pub const TOOL_PERMISSION_EVALUATOR_ID: &str = "harn.tool_permission.v1";

/// The closed decision vocabulary shared by Harn permission projections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolPermissionEffect {
    Allow,
    Ask,
    Deny,
}

/// One or more anchored glob patterns for a matcher dimension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolPermissionPatterns {
    One(String),
    Many(Vec<String>),
}

impl ToolPermissionPatterns {
    fn iter(&self) -> impl Iterator<Item = &str> {
        match self {
            Self::One(value) => std::slice::from_ref(value).iter().map(String::as_str),
            Self::Many(values) => values.iter().map(String::as_str),
        }
    }
}

/// Typed matcher registry for one rule.
///
/// Unknown dimensions are rejected at insertion time, so hosts cannot silently
/// invent a taxonomy Harn will ignore.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolPermissionMatchers(BTreeMap<String, ToolPermissionPatterns>);

impl ToolPermissionMatchers {
    pub const KEYS: &'static [&'static str] = &[
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

    /// Add a matcher, returning the unsupported key unchanged on failure.
    pub fn insert(
        &mut self,
        key: impl Into<String>,
        patterns: ToolPermissionPatterns,
    ) -> Result<(), String> {
        let key = key.into();
        if !Self::KEYS.contains(&key.as_str()) {
            return Err(key);
        }
        self.0.insert(key, patterns);
        Ok(())
    }

    fn get(&self, key: &str) -> Option<&ToolPermissionPatterns> {
        self.0.get(key)
    }
}

/// One host-persisted rule projected into Harn's evaluator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolPermissionRule {
    pub id: String,
    pub effect: ToolPermissionEffect,
    pub matchers: ToolPermissionMatchers,
    pub reason: Option<String>,
}

/// Raw permission-request fields at the ACP/host boundary.
///
/// Normalization happens once inside Harn because `policy_decision.context`
/// and the compatibility aliases are Harn-owned protocol vocabulary.
#[derive(Clone, Debug, Default)]
pub struct ToolPermissionRequest {
    pub tool_name: String,
    pub arguments: Map<String, Value>,
    pub policy_decision: Option<Map<String, Value>>,
    pub approval_request: Option<Map<String, Value>>,
}

/// Selected rule and effect. The host uses `rule_id` to recover presentation
/// metadata from its persisted document without duplicating evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolPermissionEvaluation {
    pub evaluator: &'static str,
    pub effect: ToolPermissionEffect,
    pub rule_id: String,
    pub reason: String,
}

/// Evaluate all matching rules with the canonical deny > ask > allow
/// precedence. Within an effect, authored order wins.
pub fn evaluate_tool_permission_policy(
    rules: &[ToolPermissionRule],
    request: &ToolPermissionRequest,
) -> Option<ToolPermissionEvaluation> {
    let context = DerivedContext::from_request(request);
    let selected = [
        ToolPermissionEffect::Deny,
        ToolPermissionEffect::Ask,
        ToolPermissionEffect::Allow,
    ]
    .into_iter()
    .find_map(|effect| {
        rules
            .iter()
            .find(|rule| rule.effect == effect && rule.matches(&context))
    })?;
    Some(ToolPermissionEvaluation {
        evaluator: TOOL_PERMISSION_EVALUATOR_ID,
        effect: selected.effect,
        rule_id: selected.id.clone(),
        reason: selected.reason.clone().unwrap_or_else(|| {
            format!(
                "{} rule {} matched",
                effect_name(selected.effect),
                selected.id
            )
        }),
    })
}

fn effect_name(effect: ToolPermissionEffect) -> &'static str {
    match effect {
        ToolPermissionEffect::Allow => "allow",
        ToolPermissionEffect::Ask => "ask",
        ToolPermissionEffect::Deny => "deny",
    }
}

impl ToolPermissionRule {
    fn matches(&self, context: &DerivedContext) -> bool {
        if self.effect == ToolPermissionEffect::Allow
            && requires_exact_env_mode_allow(&context.env_modes)
            && !exactly_matches_env_modes(self.matchers.get("env_mode"), &context.env_modes)
        {
            return false;
        }
        matches_field(self.matchers.get("tool"), &context.tool_name)
            && matches_field(
                self.matchers.get("tool_kind"),
                context.tool_kind.as_deref().unwrap_or(""),
            )
            && matches_any(self.matchers.get("side_effect"), &context.side_effects)
            && matches_any(self.matchers.get("path"), &context.paths)
            && matches_any(self.matchers.get("command"), &context.commands)
            && matches_any(self.matchers.get("command_identity"), &context.commands)
            && matches_any(self.matchers.get("url"), &context.urls)
            && matches_any(self.matchers.get("domain"), &context.domains)
            && matches_any(self.matchers.get("method"), &context.methods)
            && matches_any(self.matchers.get("mcp_server"), &context.mcp_servers)
            && matches_any(self.matchers.get("mcp_tool"), &context.mcp_tools)
            && matches_field(
                self.matchers.get("agent"),
                context.agent.as_deref().unwrap_or(""),
            )
            && matches_field(
                self.matchers.get("persona"),
                context.persona.as_deref().unwrap_or(""),
            )
            && matches_field(
                self.matchers.get("mode"),
                context.mode.as_deref().unwrap_or(""),
            )
            && matches_env_modes(self.matchers.get("env_mode"), &context.env_modes)
            && matches_any(self.matchers.get("capability"), &context.capabilities)
    }
}

#[derive(Debug, Default)]
struct DerivedContext {
    tool_name: String,
    tool_kind: Option<String>,
    side_effects: Vec<String>,
    capabilities: Vec<String>,
    paths: Vec<String>,
    commands: Vec<String>,
    urls: Vec<String>,
    domains: Vec<String>,
    methods: Vec<String>,
    mcp_servers: Vec<String>,
    mcp_tools: Vec<String>,
    agent: Option<String>,
    persona: Option<String>,
    mode: Option<String>,
    env_modes: Vec<String>,
}

impl DerivedContext {
    fn from_request(request: &ToolPermissionRequest) -> Self {
        let policy_context = request
            .policy_decision
            .as_ref()
            .and_then(|decision| decision.get("context"))
            .and_then(Value::as_object)
            .cloned()
            .or_else(|| {
                request
                    .approval_request
                    .as_ref()
                    .and_then(|approval| approval.get("undo_metadata"))
                    .and_then(Value::as_object)
                    .and_then(|metadata| metadata.get("policy_decision"))
                    .and_then(Value::as_object)
                    .and_then(|decision| decision.get("context"))
                    .and_then(Value::as_object)
                    .cloned()
            })
            .unwrap_or_default();
        let nested = policy_context
            .get("policy_context")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut nested_inputs = Vec::new();
        for key in ["rawInput", "raw_input", "input"] {
            if let Some(value) = policy_context.get(key).and_then(Value::as_object) {
                nested_inputs.push(value.clone());
            }
            if let Some(value) = request.arguments.get(key).and_then(Value::as_object) {
                nested_inputs.push(value.clone());
            }
        }
        let mut arguments = vec![&request.arguments];
        arguments.extend(nested_inputs.iter());

        let mut urls = string_list(&policy_context, &["url", "urls"]);
        for argument in &arguments {
            urls.extend(string_list(argument, &["url", "urls"]));
        }
        let urls = unique(urls);
        let (mcp_servers_raw, mcp_tools_raw) = mcp_identity(&request.tool_name, &policy_context);
        let mut domains = string_list(&policy_context, &["domain", "domains"]);
        domains.extend(urls.iter().filter_map(|url| domain_from_url(url)));

        Self {
            tool_name: string_value(policy_context.get("tool_name"))
                .or_else(|| string_value(policy_context.get("toolName")))
                .unwrap_or_else(|| request.tool_name.clone()),
            tool_kind: string_value(policy_context.get("tool_kind"))
                .or_else(|| string_value(policy_context.get("toolKind"))),
            side_effects: unique(concat(&[
                string_list(&policy_context, &["side_effect", "sideEffect"]),
                string_list(
                    &policy_context,
                    &["requested_side_effect_level", "requestedSideEffectLevel"],
                ),
                string_list(&nested, &["side_effect", "sideEffect"]),
                string_list(
                    &nested,
                    &["requested_side_effect_level", "requestedSideEffectLevel"],
                ),
            ])),
            capabilities: unique(concat(&[
                string_list(&policy_context, &["capability", "capabilities"]),
                string_list(&nested, &["capability", "capabilities"]),
            ])),
            paths: request_paths(&policy_context, &arguments),
            commands: request_commands(&policy_context, &arguments),
            urls,
            domains: unique(domains),
            methods: unique({
                let mut values =
                    string_list(&policy_context, &["method", "http_method", "http_methods"]);
                for argument in &arguments {
                    values.extend(string_list(
                        argument,
                        &["method", "http_method", "http_methods"],
                    ));
                }
                values
            }),
            mcp_servers: unique(concat(&[
                string_list(&policy_context, &["mcp_server", "mcp_servers"]),
                string_list(&nested, &["mcp_server", "mcp_servers"]),
                mcp_servers_raw,
            ])),
            mcp_tools: unique(concat(&[
                string_list(&policy_context, &["mcp_tool", "mcp_tools"]),
                string_list(&nested, &["mcp_tool", "mcp_tools"]),
                mcp_tools_raw,
            ])),
            agent: string_value(policy_context.get("agent"))
                .or_else(|| string_value(policy_context.get("agent_id"))),
            persona: string_value(policy_context.get("persona"))
                .or_else(|| string_value(policy_context.get("persona_id"))),
            mode: string_value(policy_context.get("mode"))
                .or_else(|| string_value(policy_context.get("action"))),
            env_modes: unique({
                let mut values = concat(&[
                    string_list(&policy_context, &["env_mode", "envMode"]),
                    string_list(&nested, &["env_mode", "envMode"]),
                ]);
                for argument in &arguments {
                    values.extend(string_list(argument, &["env_mode", "envMode"]));
                }
                values
            }),
        }
    }
}

fn matches_field(patterns: Option<&ToolPermissionPatterns>, value: &str) -> bool {
    patterns.is_none_or(|patterns| patterns.iter().any(|pattern| glob_matches(pattern, value)))
}

fn matches_any(patterns: Option<&ToolPermissionPatterns>, values: &[String]) -> bool {
    patterns.is_none_or(|patterns| {
        values
            .iter()
            .any(|value| patterns.iter().any(|pattern| glob_matches(pattern, value)))
    })
}

fn requires_exact_env_mode_allow(modes: &[String]) -> bool {
    modes
        .iter()
        .any(|mode| mode == "patch" || mode == "replace")
}

fn normalized_env_modes(modes: &[String]) -> Vec<&str> {
    if modes.is_empty() {
        vec!["inherit_clean"]
    } else {
        modes.iter().map(String::as_str).collect()
    }
}

fn exactly_matches_env_modes(patterns: Option<&ToolPermissionPatterns>, modes: &[String]) -> bool {
    patterns.is_some_and(|patterns| {
        normalized_env_modes(modes).iter().all(|mode| {
            !matches!(*mode, "patch" | "replace") || patterns.iter().any(|pattern| pattern == *mode)
        })
    })
}

fn matches_env_modes(patterns: Option<&ToolPermissionPatterns>, modes: &[String]) -> bool {
    patterns.is_none_or(|patterns| {
        normalized_env_modes(modes)
            .iter()
            .all(|mode| patterns.iter().any(|pattern| glob_matches(pattern, mode)))
    })
}

fn request_paths(context: &Map<String, Value>, arguments: &[&Map<String, Value>]) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(entries) = context.get("paths").and_then(Value::as_array) {
        for entry in entries.iter().filter_map(Value::as_object) {
            if let Some(path) = string_value(entry.get("workspace_path"))
                .or_else(|| string_value(entry.get("path")))
                .or_else(|| string_value(entry.get("host_absolute_path")))
            {
                paths.push(path);
            }
        }
    }
    paths.extend(string_list(context, &["path", "paths"]));
    for argument in arguments {
        paths.extend(string_list(
            argument,
            &[
                "path",
                "file",
                "target",
                "source_path",
                "new_path",
                "target_path",
            ],
        ));
    }
    unique(paths)
}

fn request_commands(
    context: &Map<String, Value>,
    arguments: &[&Map<String, Value>],
) -> Vec<String> {
    let mut commands = string_list(
        context,
        &["command", "command_identity", "command_identities"],
    );
    for argument in arguments {
        commands.extend(string_list(
            argument,
            &["command", "command_identity", "operation"],
        ));
        if let Some(argv) = argument.get("argv").and_then(Value::as_array) {
            let command = argv
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ");
            if !command.is_empty() {
                commands.push(command);
            }
        }
    }
    unique(commands)
}

fn mcp_identity(tool_name: &str, context: &Map<String, Value>) -> (Vec<String>, Vec<String>) {
    let mut servers = Vec::new();
    let mut tools = Vec::new();
    let separator = if tool_name.contains("__") {
        Some("__")
    } else if tool_name.starts_with("mcp.") {
        Some(".")
    } else {
        None
    };
    if let Some(separator) = separator {
        let stripped = tool_name.strip_prefix("mcp.").unwrap_or(tool_name);
        let mut parts = stripped.split(separator);
        if let Some(server) = parts.next().filter(|value| !value.is_empty()) {
            servers.push(server.to_string());
        }
        let tool = parts.collect::<Vec<_>>().join(separator);
        if !tool.is_empty() {
            tools.push(tool);
        }
    }
    if let Some(server) =
        string_value(context.get("_mcp_server")).or_else(|| string_value(context.get("server")))
    {
        servers.push(server);
    }
    (unique(servers), unique(tools))
}

fn string_value(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn string_list(object: &Map<String, Value>, keys: &[&str]) -> Vec<String> {
    let mut values = Vec::new();
    for key in keys {
        match object.get(*key) {
            Some(Value::String(value)) if !value.is_empty() => values.push(value.clone()),
            Some(Value::Array(items)) => values.extend(
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
            ),
            _ => {}
        }
    }
    values
}

fn concat(values: &[Vec<String>]) -> Vec<String> {
    values.iter().flatten().cloned().collect()
}

fn unique(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn domain_from_url(value: &str) -> Option<String> {
    let authority = value.split("://").nth(1)?.split(['/', '?', '#']).next()?;
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or(authority)
        .split(':')
        .next()
        .unwrap_or(authority);
    (!host.is_empty()).then(|| host.to_string())
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    pattern == "*" || harn_glob::match_path(pattern, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str, effect: ToolPermissionEffect, fields: &[(&str, &str)]) -> ToolPermissionRule {
        let mut matchers = ToolPermissionMatchers::default();
        for (key, value) in fields {
            matchers
                .insert(*key, ToolPermissionPatterns::One((*value).to_string()))
                .unwrap();
        }
        ToolPermissionRule {
            id: id.to_string(),
            effect,
            matchers,
            reason: None,
        }
    }

    fn request(tool: &str, arguments: Value, context: Value) -> ToolPermissionRequest {
        ToolPermissionRequest {
            tool_name: tool.to_string(),
            arguments: arguments.as_object().cloned().unwrap_or_default(),
            policy_decision: Some(Map::from_iter([("context".to_string(), context)])),
            approval_request: None,
        }
    }

    #[test]
    fn deny_ask_allow_precedence_is_independent_of_authored_order() {
        let request = request("run", Value::Null, Value::Object(Map::new()));
        let rules = [
            rule("allow", ToolPermissionEffect::Allow, &[("tool", "run")]),
            rule("ask", ToolPermissionEffect::Ask, &[("tool", "run")]),
            rule("deny", ToolPermissionEffect::Deny, &[("tool", "run")]),
        ];
        assert_eq!(
            evaluate_tool_permission_policy(&rules, &request)
                .unwrap()
                .rule_id,
            "deny"
        );
    }

    #[test]
    fn normalizes_harn_context_and_raw_arguments_across_matcher_dimensions() {
        let request = request(
            "mcp.github__create_issue",
            serde_json::json!({
                "path": "src/lib.rs",
                "command": "cargo test",
                "url": "https://api.example.com/v1",
                "method": "POST",
            }),
            serde_json::json!({
                "tool_kind": "mcp",
                "side_effect": "external_write",
                "capabilities": ["network"],
                "agent": "worker-1",
                "persona": "reviewer",
                "mode": "code",
                "env_mode": "inherit_clean",
            }),
        );
        let rules = [rule(
            "all-fields",
            ToolPermissionEffect::Ask,
            &[
                ("tool", "mcp.*"),
                ("tool_kind", "mcp"),
                ("side_effect", "external_write"),
                ("path", "src/*.rs"),
                ("command", "cargo *"),
                ("url", "https://api.example.com/**"),
                ("domain", "api.example.com"),
                ("method", "POST"),
                ("mcp_server", "github"),
                ("mcp_tool", "create_issue"),
                ("agent", "worker-*"),
                ("persona", "reviewer"),
                ("mode", "code"),
                ("env_mode", "inherit_clean"),
                ("capability", "network"),
            ],
        )];
        assert_eq!(
            evaluate_tool_permission_policy(&rules, &request)
                .unwrap()
                .rule_id,
            "all-fields"
        );
    }

    #[test]
    fn write_env_modes_require_an_exact_allow() {
        let request = request(
            "run",
            serde_json::json!({"env_mode": "patch"}),
            Value::Object(Map::new()),
        );
        let wildcard = [rule(
            "wildcard",
            ToolPermissionEffect::Allow,
            &[("tool", "run"), ("env_mode", "*")],
        )];
        assert!(evaluate_tool_permission_policy(&wildcard, &request).is_none());
        let exact = [rule(
            "exact",
            ToolPermissionEffect::Allow,
            &[("tool", "run"), ("env_mode", "patch")],
        )];
        assert!(evaluate_tool_permission_policy(&exact, &request).is_some());
    }

    #[test]
    fn glob_contract_is_anchored_and_distinguishes_single_from_double_star() {
        assert!(glob_matches("src/*.rs", "src/lib.rs"));
        assert!(!glob_matches("src/*.rs", "src/nested/lib.rs"));
        assert!(glob_matches("src/**/*.rs", "src/nested/lib.rs"));
        assert!(
            glob_matches("src/**/*.rs", "src/lib.rs"),
            "tool policies use Harn's canonical zero-directory `**/` semantics"
        );
        assert!(!glob_matches("run", "cargo run"));
        assert!(glob_matches("*", "a/b/c"));
    }

    #[test]
    fn unknown_matcher_dimensions_fail_closed_at_the_boundary() {
        let mut matchers = ToolPermissionMatchers::default();
        assert_eq!(
            matchers.insert(
                "host_invented_taxonomy",
                ToolPermissionPatterns::One("*".to_string())
            ),
            Err("host_invented_taxonomy".to_string())
        );
    }
}
