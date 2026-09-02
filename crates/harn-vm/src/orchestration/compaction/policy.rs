//! The `compaction_policy` value shape: parse it, normalize it, project it.
//!
//! One owner for the whole round trip — the option keys a caller may set, the
//! typed `CompactionPolicy` they parse into, the VM value they project back
//! out, and the receipt metadata fields derived from them. `compaction.rs`
//! consumes the typed struct and never re-reads the raw option dict.

use crate::value::{VmDictExt, VmError, VmValue};
use serde::{Deserialize, Serialize};

const COMPACTION_POLICY_KEYS: &[&str] = &[
    "instructions",
    "mode",
    "scope",
    "preserve",
    "drop",
    "extend_default_instructions",
    "author",
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionPolicy {
    pub instructions: Option<String>,
    pub mode: Option<String>,
    pub scope: Option<String>,
    pub preserve: Vec<String>,
    #[serde(rename = "drop")]
    pub drop_items: Vec<String>,
    pub extend_default_instructions: Option<bool>,
    pub author: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionRequest {
    pub mode: Option<String>,
    pub policy: CompactionPolicy,
}

impl CompactionPolicy {
    pub fn has_metadata(&self) -> bool {
        self.instructions.is_some()
            || self.mode.is_some()
            || self.scope.is_some()
            || !self.preserve.is_empty()
            || !self.drop_items.is_empty()
            || self.extend_default_instructions.is_some()
            || self.author.is_some()
    }

    pub(crate) fn has_prompt_directives(&self) -> bool {
        self.instructions
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            || !self.preserve.is_empty()
            || !self.drop_items.is_empty()
    }

    pub fn instruction_mode(&self) -> &'static str {
        if !self.has_prompt_directives() {
            "default"
        } else if self.extend_default_instructions == Some(false) {
            "replace"
        } else {
            "extend"
        }
    }

    pub fn instruction_source(&self) -> Option<&str> {
        self.author
            .as_deref()
            .filter(|author| !author.trim().is_empty())
    }

    pub fn metadata_json(&self) -> Option<serde_json::Value> {
        if !self.has_metadata() {
            return None;
        }
        let mut map = serde_json::Map::new();
        if let Some(instructions) = self.instructions.as_ref() {
            map.insert(
                "instructions".to_string(),
                serde_json::Value::String(instructions.clone()),
            );
        }
        if let Some(mode) = self.mode.as_ref() {
            map.insert("mode".to_string(), serde_json::Value::String(mode.clone()));
        }
        if let Some(scope) = self.scope.as_ref() {
            map.insert(
                "scope".to_string(),
                serde_json::Value::String(scope.clone()),
            );
        }
        if !self.preserve.is_empty() {
            map.insert(
                "preserve".to_string(),
                serde_json::to_value(&self.preserve).unwrap_or_default(),
            );
        }
        if !self.drop_items.is_empty() {
            map.insert(
                "drop".to_string(),
                serde_json::to_value(&self.drop_items).unwrap_or_default(),
            );
        }
        if let Some(extend_default_instructions) = self.extend_default_instructions {
            map.insert(
                "extend_default_instructions".to_string(),
                serde_json::Value::Bool(extend_default_instructions),
            );
        }
        if let Some(author) = self.author.as_ref() {
            map.insert(
                "author".to_string(),
                serde_json::Value::String(author.clone()),
            );
        }
        map.insert(
            "instruction_mode".to_string(),
            serde_json::Value::String(self.instruction_mode().to_string()),
        );
        if let Some(source) = self.instruction_source() {
            map.insert(
                "instruction_source".to_string(),
                serde_json::Value::String(source.to_string()),
            );
        }
        Some(serde_json::Value::Object(map))
    }

    pub(crate) fn prompt_directives(&self) -> Option<String> {
        if !self.has_prompt_directives() {
            return None;
        }
        let mut parts = Vec::new();
        if let Some(instructions) = self
            .instructions
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            parts.push(instructions.to_string());
        }
        if !self.preserve.is_empty() {
            parts.push(format!("Preserve: {}.", self.preserve.join("; ")));
        }
        if !self.drop_items.is_empty() {
            parts.push(format!("Drop: {}.", self.drop_items.join("; ")));
        }
        Some(parts.join("\n"))
    }

    pub(crate) fn is_model_visible_scope(&self) -> bool {
        matches!(
            self.scope.as_deref(),
            Some("model_visible" | "summary" | "transcript")
        )
    }
}

pub fn compaction_policy_option_keys() -> &'static [&'static str] {
    COMPACTION_POLICY_KEYS
}

pub fn compaction_policy_to_vm_value(policy: &CompactionPolicy) -> VmValue {
    let mut map = crate::value::DictMap::new();
    if let Some(instructions) = policy.instructions.as_ref() {
        map.put_str("instructions", instructions.clone());
    }
    if let Some(mode) = policy.mode.as_ref() {
        map.put_str("mode", mode.clone());
    }
    if let Some(scope) = policy.scope.as_ref() {
        map.put_str("scope", scope.clone());
    }
    map.insert(
        crate::value::intern_key("preserve"),
        VmValue::List(std::sync::Arc::new(
            policy
                .preserve
                .iter()
                .map(|item| VmValue::String(arcstr::ArcStr::from(item.clone())))
                .collect(),
        )),
    );
    map.insert(
        crate::value::intern_key("drop"),
        VmValue::List(std::sync::Arc::new(
            policy
                .drop_items
                .iter()
                .map(|item| VmValue::String(arcstr::ArcStr::from(item.clone())))
                .collect(),
        )),
    );
    if let Some(extend_default_instructions) = policy.extend_default_instructions {
        map.insert(
            crate::value::intern_key("extend_default_instructions"),
            VmValue::Bool(extend_default_instructions),
        );
    }
    if let Some(author) = policy.author.as_ref() {
        map.put_str("author", author.clone());
    }
    VmValue::dict(map)
}

pub fn parse_compaction_policy_options(
    options: Option<&crate::value::DictMap>,
    builtin: &str,
) -> Result<CompactionPolicy, VmError> {
    let mut policy = options
        .and_then(|map| {
            map.get("policy")
                .or_else(|| map.get("compaction_policy"))
                .or_else(|| map.get("compaction_request"))
        })
        .map(|value| parse_compaction_policy_value(value, builtin))
        .transpose()?
        .unwrap_or_default();
    if let Some(options) = options {
        apply_compaction_policy_fields(&mut policy, options, builtin)?;
    }
    Ok(policy)
}

fn parse_compaction_policy_value(
    value: &VmValue,
    builtin: &str,
) -> Result<CompactionPolicy, VmError> {
    match value {
        VmValue::Nil => Ok(CompactionPolicy::default()),
        VmValue::Dict(map) => {
            if let Some(nested) = map
                .get("policy")
                .or_else(|| map.get("compaction_policy"))
                .or_else(|| map.get("compaction_request"))
            {
                let mut policy = parse_compaction_policy_value(nested, builtin)?;
                apply_compaction_policy_fields(&mut policy, map, builtin)?;
                Ok(policy)
            } else {
                let mut policy = CompactionPolicy::default();
                apply_compaction_policy_fields(&mut policy, map, builtin)?;
                Ok(policy)
            }
        }
        other => Err(VmError::Runtime(format!(
            "{builtin}: compaction policy must be a dict or nil, got {}",
            other.type_name()
        ))),
    }
}

fn apply_compaction_policy_fields(
    policy: &mut CompactionPolicy,
    map: &crate::value::DictMap,
    builtin: &str,
) -> Result<(), VmError> {
    if let Some(value) = optional_policy_string(map, "instructions", builtin)? {
        policy.instructions = Some(value);
    }
    if let Some(value) = optional_policy_string(map, "mode", builtin)? {
        policy.mode = Some(value);
    }
    if let Some(value) = optional_policy_string(map, "scope", builtin)? {
        policy.scope = Some(value);
    }
    if map.contains_key("preserve") {
        policy.preserve = policy_string_list(map.get("preserve"), builtin, "preserve")?;
    }
    if map.contains_key("drop") {
        policy.drop_items = policy_string_list(map.get("drop"), builtin, "drop")?;
    }
    if let Some(value) = optional_policy_bool(map, "extend_default_instructions", builtin)? {
        policy.extend_default_instructions = Some(value);
    }
    if let Some(value) = optional_policy_string(map, "author", builtin)? {
        policy.author = Some(value);
    }
    Ok(())
}

fn optional_policy_string(
    map: &crate::value::DictMap,
    key: &str,
    builtin: &str,
) -> Result<Option<String>, VmError> {
    match map.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: compaction policy `{key}` must be a string, got {}",
            other.type_name()
        ))),
    }
}

fn optional_policy_bool(
    map: &crate::value::DictMap,
    key: &str,
    builtin: &str,
) -> Result<Option<bool>, VmError> {
    match map.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Bool(value)) => Ok(Some(*value)),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: compaction policy `{key}` must be a bool, got {}",
            other.type_name()
        ))),
    }
}

fn policy_string_list(
    value: Option<&VmValue>,
    builtin: &str,
    key: &str,
) -> Result<Vec<String>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(Vec::new()),
        Some(VmValue::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Ok(Vec::new())
            } else {
                Ok(vec![trimmed.to_string()])
            }
        }
        Some(VmValue::List(items)) => items
            .iter()
            .map(|item| match item {
                VmValue::String(text) => Ok(text.trim().to_string()),
                other => Err(VmError::Runtime(format!(
                    "{builtin}: compaction policy `{key}` entries must be strings, got {}",
                    other.type_name()
                ))),
            })
            .filter_map(|result| match result {
                Ok(value) if value.is_empty() => None,
                other => Some(other),
            })
            .collect(),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: compaction policy `{key}` must be a string or list, got {}",
            other.type_name()
        ))),
    }
}

pub fn compaction_policy_metadata_fields(
    policy: &CompactionPolicy,
) -> Vec<(&'static str, serde_json::Value)> {
    let mut fields = vec![(
        "instruction_mode",
        serde_json::Value::String(policy.instruction_mode().to_string()),
    )];
    if let Some(source) = policy.instruction_source() {
        fields.push((
            "instruction_source",
            serde_json::Value::String(source.to_string()),
        ));
    }
    if let Some(policy_json) = policy.metadata_json() {
        fields.push(("compaction_policy", policy_json));
    }
    fields
}
