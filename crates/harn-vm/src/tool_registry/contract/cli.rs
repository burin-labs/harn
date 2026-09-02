use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Deterministic command-line presentation for one tool.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ToolCliSpec {
    /// Non-empty command path below `harn tool run <script>`.
    #[schemars(length(min = 1), inner(pattern(r"^[A-Za-z0-9_][A-Za-z0-9_-]*$")))]
    pub command: Vec<String>,
    /// Alternate spellings for the final command component.
    #[serde(default)]
    #[schemars(inner(pattern(r"^[A-Za-z0-9_][A-Za-z0-9_-]*$")), extend("uniqueItems" = true))]
    pub aliases: Vec<String>,
    /// Hide the command from help while retaining explicit invocation.
    pub hidden: bool,
    /// Token-to-property projections. Omitted properties retain Harn's
    /// zero-configuration `--property-name` projection.
    #[serde(default)]
    pub arguments: BTreeMap<String, ToolCliArgumentSpec>,
}

/// Portable command metadata owned once at the catalog level.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ToolCliTreeSpec {
    #[serde(default)]
    pub commands: Vec<ToolCliCommandSpec>,
}

/// Presentation for one non-runnable command path in the generated CLI tree.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ToolCliCommandSpec {
    #[schemars(length(min = 1), inner(pattern(r"^[A-Za-z0-9_][A-Za-z0-9_-]*$")))]
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[schemars(length(min = 1), pattern(r".*\S.*"))]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[schemars(length(min = 1), pattern(r".*\S.*"))]
    pub description: Option<String>,
    #[serde(default)]
    #[schemars(inner(pattern(r"^[A-Za-z0-9_][A-Za-z0-9_-]*$")), extend("uniqueItems" = true))]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub display_order: Option<u32>,
}

/// Closed, shell-portable completion hint for an argument value.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ToolCliValueHint {
    File,
    Directory,
    Path,
    Url,
    Email,
    Username,
    Hostname,
    Command,
    Other,
}

/// How one boolean property maps from command-line tokens.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ToolCliBooleanStyle {
    /// Read an explicit `true` or `false` value.
    #[default]
    Value,
    /// Insert `true` when the option is present and omit the property otherwise.
    SetTrue,
    /// Insert `false` when the option is present and omit the property otherwise.
    SetFalse,
}

/// Presentation and token mapping for one input-schema property.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ToolCliArgumentSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[schemars(pattern(r"^[A-Za-z0-9][A-Za-z0-9-]*$"))]
    pub long: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub short: Option<char>,
    #[serde(default)]
    #[schemars(inner(pattern(r"^[A-Za-z0-9][A-Za-z0-9-]*$")), extend("uniqueItems" = true))]
    pub aliases: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub position: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[schemars(length(min = 1), pattern(r".*\S.*"))]
    pub value_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[schemars(length(min = 1), pattern(r".*\S.*"))]
    pub help: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub value_hint: Option<ToolCliValueHint>,
    /// Boolean token behavior. Presence styles never synthesize a value when
    /// the option is absent.
    #[serde(default)]
    pub boolean_style: ToolCliBooleanStyle,
    #[serde(default)]
    pub repeatable: bool,
    /// Hide the argument from generated help while retaining explicit use.
    #[serde(default)]
    pub hidden: bool,
    /// Ordered advisory completion candidates. These do not constrain input.
    #[serde(default)]
    #[schemars(extend("uniqueItems" = true))]
    pub completions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub display_order: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[schemars(length(min = 1), pattern(r".*\S.*"))]
    pub help_group: Option<String>,
}
