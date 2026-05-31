use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::tool_annotations::{SideEffectLevel, ToolAnnotations};

pub const BINDING_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Policy disposition for a binding projected into a composition manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingPolicyDisposition {
    Allowed,
    Gated,
    Denied,
}

/// Policy metadata attached to a manifest binding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BindingPolicyStatus {
    pub disposition: BindingPolicyDisposition,
    pub reason: Option<String>,
}

impl Default for BindingPolicyStatus {
    fn default() -> Self {
        Self {
            disposition: BindingPolicyDisposition::Allowed,
            reason: None,
        }
    }
}

/// Prompt-visible description of one callable binding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BindingManifestEntry {
    /// Canonical runtime tool name.
    pub name: String,
    /// Harn identifier injected into the composition snippet.
    pub binding: String,
    pub namespace: Option<String>,
    pub description: Option<String>,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    pub annotations: ToolAnnotations,
    pub side_effect_level: SideEffectLevel,
    pub capabilities: BTreeMap<String, Vec<String>>,
    pub path_args: Vec<String>,
    pub examples: Vec<Value>,
    /// `harn`, `host_bridge`, `mcp_server`, `provider_native`, `deferred`,
    /// or another forward-compatible source label.
    pub source: String,
    pub deferred: bool,
    pub policy: BindingPolicyStatus,
    pub metadata: Value,
}

impl Default for BindingManifestEntry {
    fn default() -> Self {
        Self {
            name: String::new(),
            binding: String::new(),
            namespace: None,
            description: None,
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: None,
            annotations: ToolAnnotations::default(),
            side_effect_level: SideEffectLevel::None,
            capabilities: BTreeMap::new(),
            path_args: Vec::new(),
            examples: Vec::new(),
            source: "harn".to_string(),
            deferred: false,
            policy: BindingPolicyStatus::default(),
            metadata: Value::Object(serde_json::Map::new()),
        }
    }
}

/// Stable prompt-visible manifest for a composition run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BindingManifest {
    pub schema_version: u32,
    pub bindings: Vec<BindingManifestEntry>,
    pub side_effect_ceiling: SideEffectLevel,
    pub metadata: Value,
}

impl Default for BindingManifest {
    fn default() -> Self {
        Self {
            schema_version: BINDING_MANIFEST_SCHEMA_VERSION,
            bindings: Vec::new(),
            side_effect_ceiling: SideEffectLevel::ReadOnly,
            metadata: Value::Object(serde_json::Map::new()),
        }
    }
}

impl BindingManifest {
    pub fn new(mut bindings: Vec<BindingManifestEntry>, ceiling: SideEffectLevel) -> Self {
        bindings.sort_by(|a, b| a.binding.cmp(&b.binding).then(a.name.cmp(&b.name)));
        Self {
            bindings,
            side_effect_ceiling: ceiling,
            ..Self::default()
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({"bindings": []}))
    }

    pub fn to_compact_value(&self) -> Value {
        Value::Object(serde_json::Map::from_iter([
            (
                "schema_version".to_string(),
                Value::Number(self.schema_version.into()),
            ),
            (
                "side_effect_ceiling".to_string(),
                serde_json::json!(self.side_effect_ceiling),
            ),
            (
                "bindings".to_string(),
                Value::Array(
                    self.bindings
                        .iter()
                        .map(|binding| {
                            serde_json::json!({
                                "name": binding.name,
                                "binding": binding.binding,
                                "namespace": binding.namespace,
                                "description": binding.description,
                                "side_effect_level": binding.side_effect_level,
                                "policy": binding.policy,
                                "source": binding.source,
                                "deferred": binding.deferred,
                                "examples": binding.examples,
                            })
                        })
                        .collect(),
                ),
            ),
        ]))
    }

    pub fn hash(&self) -> Result<String, serde_json::Error> {
        binding_manifest_hash(&self.to_value())
    }

    pub fn find_by_binding(&self, binding: &str) -> Option<&BindingManifestEntry> {
        self.bindings.iter().find(|entry| entry.binding == binding)
    }

    pub fn find_by_name(&self, name: &str) -> Option<&BindingManifestEntry> {
        self.bindings.iter().find(|entry| entry.name == name)
    }
}

/// Stable digest for a binding manifest value. Producers should build
/// manifests with deterministic object key order before hashing.
pub fn binding_manifest_hash(manifest: &Value) -> Result<String, serde_json::Error> {
    let canonical = serde_json::to_vec(manifest)?;
    let mut hasher = Sha256::new();
    hasher.update(b"harn.composition.binding_manifest.v1\0");
    hasher.update(&canonical);
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingManifestOptions {
    pub side_effect_ceiling: SideEffectLevel,
    pub include_denied: bool,
    pub denied_tools: BTreeSet<String>,
    pub gated_tools: BTreeSet<String>,
}

impl Default for BindingManifestOptions {
    fn default() -> Self {
        Self {
            side_effect_ceiling: SideEffectLevel::ReadOnly,
            include_denied: false,
            denied_tools: BTreeSet::new(),
            gated_tools: BTreeSet::new(),
        }
    }
}

/// Build a binding manifest from a Harn tool registry, MCP `tools/list`
/// payload, or provider-native tool array.
pub fn binding_manifest_from_tool_surface(
    tools: &Value,
    options: BindingManifestOptions,
) -> BindingManifest {
    let mut used_bindings = BTreeSet::new();
    let annotations_by_name = crate::tool_surface::tool_annotations_from_spec(tools);
    let mut entries = Vec::new();
    for tool in tool_surface_entries(tools) {
        let Some(name) = tool
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let annotations = tool
            .get("annotations")
            .cloned()
            .and_then(|value| serde_json::from_value::<ToolAnnotations>(value).ok())
            .or_else(|| annotations_by_name.get(name).cloned())
            .unwrap_or_default();
        let side_effect_level = annotations.side_effect_level;
        let mut policy = BindingPolicyStatus::default();
        if options.denied_tools.contains(name) {
            policy.disposition = BindingPolicyDisposition::Denied;
            policy.reason = Some("denied by active tool policy".to_string());
        } else if side_effect_level.rank() > options.side_effect_ceiling.rank() {
            policy.disposition = BindingPolicyDisposition::Denied;
            policy.reason = Some(format!(
                "requires side-effect level '{}' above composition ceiling '{}'",
                side_effect_level.as_str(),
                options.side_effect_ceiling.as_str()
            ));
        } else if options.gated_tools.contains(name) {
            policy.disposition = BindingPolicyDisposition::Gated;
            policy.reason = Some("requires host approval before dispatch".to_string());
        }
        if !options.include_denied && policy.disposition == BindingPolicyDisposition::Denied {
            continue;
        }
        let binding = unique_binding_identifier(name, &mut used_bindings);
        let source = binding_source(&tool);
        let deferred = tool
            .get("defer_loading")
            .and_then(Value::as_bool)
            .or_else(|| {
                tool.get("function")
                    .and_then(|function| function.get("defer_loading"))
                    .and_then(Value::as_bool)
            })
            .unwrap_or(source == "deferred");
        let input_schema = tool
            .get("inputSchema")
            .or_else(|| tool.get("input_schema"))
            .or_else(|| tool.get("parameters"))
            .or_else(|| tool.get("function").and_then(|f| f.get("parameters")))
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"type": "object"}));
        let output_schema = tool
            .get("outputSchema")
            .or_else(|| tool.get("output_schema"))
            .or_else(|| tool.get("returns"))
            .or_else(|| {
                tool.get("function")
                    .and_then(|f| f.get("x-harn-output-schema"))
            })
            .cloned();
        let examples = tool
            .get("examples")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        entries.push(BindingManifestEntry {
            name: name.to_string(),
            binding,
            namespace: tool
                .get("namespace")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            description: tool
                .get("description")
                .or_else(|| tool.get("function").and_then(|f| f.get("description")))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned),
            input_schema,
            output_schema,
            side_effect_level,
            capabilities: annotations.capabilities.clone(),
            path_args: annotations.arg_schema.path_params.clone(),
            annotations,
            examples,
            source,
            deferred,
            policy,
            metadata: binding_metadata(&tool),
        });
    }
    BindingManifest::new(entries, options.side_effect_ceiling)
}

fn tool_surface_entries(value: &Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items.clone(),
        Value::Object(map) => {
            if let Some(Value::Array(items)) = map.get("tools") {
                return items.clone();
            }
            if map.get("name").and_then(Value::as_str).is_some() {
                return vec![value.clone()];
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn binding_source(tool: &Value) -> String {
    if let Some(executor) = tool.get("executor").and_then(Value::as_str) {
        return executor.to_string();
    }
    if tool.get("_mcp_server").is_some() || tool.get("mcp_server").is_some() {
        return "mcp_server".to_string();
    }
    if tool.get("function").is_some() {
        return "provider_native".to_string();
    }
    if tool
        .get("defer_loading")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return "deferred".to_string();
    }
    "harn".to_string()
}

fn binding_metadata(tool: &Value) -> Value {
    let mut metadata = tool
        .get("metadata")
        .or_else(|| tool.get("_meta"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for key in ["_mcp_server", "mcp_server", "_mcp_tool_name"] {
        if let Some(value) = tool.get(key) {
            metadata
                .entry(key.to_string())
                .or_insert_with(|| value.clone());
        }
    }
    Value::Object(metadata)
}

fn unique_binding_identifier(name: &str, used: &mut BTreeSet<String>) -> String {
    let base = sanitize_binding_identifier(name);
    if used.insert(base.clone()) {
        return base;
    }
    for index in 2.. {
        let candidate = format!("{base}_{index}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("unbounded identifier suffix search")
}

fn sanitize_binding_identifier(name: &str) -> String {
    let mut out = String::new();
    for (idx, ch) in name.chars().enumerate() {
        if ch == '_' || ch.is_ascii_alphanumeric() {
            if idx == 0 && ch.is_ascii_digit() {
                out.push_str("tool_");
            }
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let out = out.trim_matches('_').to_string();
    let out = if out.is_empty() {
        "tool".to_string()
    } else {
        out
    };
    if HARN_KEYWORDS.contains(&out.as_str()) {
        format!("tool_{out}")
    } else {
        out
    }
}

const HARN_KEYWORDS: &[&str] = &[
    "agent",
    "as",
    "await",
    "break",
    "catch",
    "continue",
    "defer",
    "else",
    "enum",
    "false",
    "fn",
    "for",
    "if",
    "impl",
    "import",
    "in",
    "interface",
    "let",
    "match",
    "nil",
    "pipeline",
    "pub",
    "return",
    "skill",
    "spawn",
    "struct",
    "throw",
    "true",
    "try",
    "type",
    "var",
    "while",
];
