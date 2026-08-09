//! The scan vocabulary, supplied as DATA by `std/llm/dialects`.
//!
//! Delimiting a response needs vocabulary — you cannot find the end of a
//! `<tool_call>` block without knowing the tag — but *owning* that vocabulary
//! would put dialect policy back in Rust. So every tag, marker, and opener the
//! scanner matches arrives through [`ScanSpec`], and nothing here has a
//! default: an absent field is an error, never a fallback to a hardcoded list.
//! If adding a dialect requires editing this file, the cut line has been
//! violated.
//!
//! `strip_thinking` is the single exception the port plan grants a default
//! (true), because it selects a *behavior* rather than naming a dialect.

use std::collections::BTreeSet;

/// A chat-template function-markup opener and the close tag it pairs with.
#[derive(Debug, Clone)]
pub(crate) struct MarkupOpener {
    pub(crate) opener: String,
    pub(crate) close: String,
}

/// OpenAI Harmony frame vocabulary. Header markers introduce a role/channel
/// header whose tail runs to the next frame, call-tag open, or newline;
/// standalone markers consume only themselves. Keeping the two apart is what
/// stops a later literal `<|message|>` in prose or in a tool argument from
/// skipping intervening call blocks.
#[derive(Debug, Clone)]
pub(crate) struct HarmonyVocab {
    pub(crate) header_markers: Vec<String>,
    pub(crate) standalone_markers: Vec<String>,
    pub(crate) frame_prefix: String,
    pub(crate) frame_suffix: String,
    pub(crate) message_marker: String,
    pub(crate) tool_call_header_prefix: String,
    /// Wrapper opens a frame token corrupted mid-tag, e.g. `<tool_call<|`.
    pub(crate) corrupted_openers: Vec<String>,
}

/// Everything the structural scanner needs to know about a dialect.
#[derive(Debug, Clone)]
pub(crate) struct ScanSpec {
    /// Tags whose body may carry a call. Heredoc-aware close scan.
    pub(crate) call_tags: Vec<String>,
    /// Plain `<tag>body</tag>` blocks. Cheap `find` close, no heredoc scan.
    pub(crate) block_tags: Vec<String>,
    /// Contentless wrappers to emit and swallow.
    pub(crate) wrapper_tags: Vec<String>,
    pub(crate) markup_openers: Vec<MarkupOpener>,
    /// Wire openers standing in for a call tag, e.g. `[[CALL]`.
    pub(crate) reserved_openers: Vec<String>,
    pub(crate) harmony: HarmonyVocab,
    /// Registry names plus the runtime's implicit tools. Needed for
    /// delimiting only: an angle-wrapped `<name(...)>` is a unit when `name`
    /// is known, and an unknown tag otherwise.
    pub(crate) known_tools: BTreeSet<String>,
    pub(crate) strip_thinking: bool,
}

impl ScanSpec {
    /// Read a spec from the dict `std/llm/dialects` hands the scanner.
    ///
    /// Every field except `strip_thinking` is required. A missing field is a
    /// hard error rather than a silent fallback, because a fallback would let
    /// Rust answer a vocabulary question it must not own.
    pub(crate) fn from_json(spec: &serde_json::Value) -> Result<Self, String> {
        let Some(obj) = spec.as_object() else {
            return Err(format!(
                "spec must be a dict of dialect vocabulary; got {}",
                json_type_name(spec)
            ));
        };
        // Read in the order the port plan documents the fields, so the first
        // error a caller sees names the first field they left out.
        let call_tags = string_list(obj, "call_tags")?;
        let block_tags = string_list(obj, "block_tags")?;
        let wrapper_tags = string_list(obj, "wrapper_tags")?;
        let openers = markup_openers(obj)?;
        let reserved_openers = string_list(obj, "reserved_openers")?;
        let harmony_value = required(obj, "harmony")?;
        let harmony_obj = harmony_value.as_object().ok_or_else(|| {
            format!(
                "spec.harmony must be a dict; got {}",
                json_type_name(harmony_value)
            )
        })?;
        Ok(Self {
            call_tags,
            block_tags,
            wrapper_tags,
            markup_openers: openers,
            reserved_openers,
            harmony: HarmonyVocab {
                header_markers: string_list_at(harmony_obj, "harmony", "header_markers")?,
                standalone_markers: string_list_at(harmony_obj, "harmony", "standalone_markers")?,
                frame_prefix: string_at(harmony_obj, "harmony", "frame_prefix")?,
                frame_suffix: string_at(harmony_obj, "harmony", "frame_suffix")?,
                message_marker: string_at(harmony_obj, "harmony", "message_marker")?,
                tool_call_header_prefix: string_at(
                    harmony_obj,
                    "harmony",
                    "tool_call_header_prefix",
                )?,
                corrupted_openers: string_list_at(harmony_obj, "harmony", "corrupted_openers")?,
            },
            known_tools: string_list(obj, "known_tools")?.into_iter().collect(),
            strip_thinking: match obj.get("strip_thinking") {
                None | Some(serde_json::Value::Null) => true,
                Some(serde_json::Value::Bool(flag)) => *flag,
                Some(other) => {
                    return Err(format!(
                        "spec.strip_thinking must be a bool; got {}",
                        json_type_name(other)
                    ))
                }
            },
        })
    }
}

type JsonObject = serde_json::Map<String, serde_json::Value>;

fn required<'a>(obj: &'a JsonObject, field: &str) -> Result<&'a serde_json::Value, String> {
    match obj.get(field) {
        Some(serde_json::Value::Null) | None => Err(format!(
            "spec.{field} is required — the scanner holds no dialect vocabulary of its own, \
             so an absent field is an error rather than a fallback"
        )),
        Some(value) => Ok(value),
    }
}

fn string_list(obj: &JsonObject, field: &str) -> Result<Vec<String>, String> {
    let value = required(obj, field)?;
    let items = value
        .as_array()
        .ok_or_else(|| format!("spec.{field} must be a list; got {}", json_type_name(value)))?;
    items
        .iter()
        .map(|item| {
            item.as_str().map(str::to_string).ok_or_else(|| {
                format!(
                    "spec.{field} entries must be strings; got {}",
                    json_type_name(item)
                )
            })
        })
        .collect()
}

fn string_list_at(obj: &JsonObject, parent: &str, field: &str) -> Result<Vec<String>, String> {
    string_list(obj, field).map_err(|error| error.replacen("spec.", &format!("spec.{parent}."), 1))
}

fn string_at(obj: &JsonObject, parent: &str, field: &str) -> Result<String, String> {
    let value = required(obj, field)
        .map_err(|error| error.replacen("spec.", &format!("spec.{parent}."), 1))?;
    value.as_str().map(str::to_string).ok_or_else(|| {
        format!(
            "spec.{parent}.{field} must be a string; got {}",
            json_type_name(value)
        )
    })
}

fn markup_openers(obj: &JsonObject) -> Result<Vec<MarkupOpener>, String> {
    let value = required(obj, "markup_openers")?;
    let items = value.as_array().ok_or_else(|| {
        format!(
            "spec.markup_openers must be a list; got {}",
            json_type_name(value)
        )
    })?;
    items
        .iter()
        .map(|item| {
            let entry = item.as_object().ok_or_else(|| {
                format!(
                    "spec.markup_openers entries must be dicts with `opener` and `close`; got {}",
                    json_type_name(item)
                )
            })?;
            Ok(MarkupOpener {
                opener: string_at(entry, "markup_openers", "opener")?,
                close: string_at(entry, "markup_openers", "close")?,
            })
        })
        .collect()
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "nil",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "list",
        serde_json::Value::Object(_) => "dict",
    }
}
