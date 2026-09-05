//! One owner for how a `tool_define` config spells a tool's input schema.
//!
//! A config may write `parameters` for the legacy per-parameter map, or
//! `input_schema` or MCP-style `inputSchema` for a complete object-root JSON
//! Schema. A registry entry has room for two of those spellings, so the
//! resolution happens once, here, and every downstream consumer reads a single
//! key instead of guessing among three.

use crate::value::{DictMap, VmError, VmValue};

/// Config keys that mean "this tool's input schema", in the order a refusal
/// should name them.
const SCHEMA_CONFIG_KEYS: [&str; 3] = ["parameters", "input_schema", "inputSchema"];

/// The single schema spelling a config declared, or `None` when it declared
/// none. Two spellings at once is a refusal that names the keys that collided,
/// because the author wrote one schema and deserves to be told which two keys
/// the runtime saw rather than being told they wrote two.
pub(super) fn declared_schema<'a>(
    config: &'a DictMap,
    name: &str,
) -> Result<Option<(&'static str, &'a VmValue)>, VmError> {
    let declared: Vec<(&'static str, &VmValue)> = SCHEMA_CONFIG_KEYS
        .iter()
        .filter_map(|key| {
            config
                .get(*key)
                .filter(|value| !matches!(value, VmValue::Nil))
                .map(|value| (*key, value))
        })
        .collect();
    if declared.len() > 1 {
        let keys = declared
            .iter()
            .map(|(key, _)| format!("{key:?}"))
            .collect::<Vec<_>>()
            .join(" and ");
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!(
                "tool_define: tool {name:?} declares its input schema twice ({keys}); \
                 use `inputSchema` for a complete JSON Schema or legacy `parameters` \
                 for a per-parameter map, not both"
            ),
        ))));
    }
    Ok(declared.into_iter().next())
}

/// Writes the resolved schema onto the entry contract's own two keys.
///
/// A complete schema lands on `inputSchema` whichever spelling declared it,
/// and the legacy per-parameter map keeps `parameters`. Nothing here inserts a
/// competing default beside a caller's key, which is what used to make an
/// entry that declared one schema look like it declared two.
pub(super) fn insert_resolved_schema(
    entry: &mut DictMap,
    declared: Option<(&'static str, &VmValue)>,
) {
    match declared {
        Some(("parameters", value)) => {
            entry.insert(crate::value::intern_key("parameters"), value.clone());
        }
        Some((_, value)) => {
            entry.insert(crate::value::intern_key("inputSchema"), value.clone());
        }
        None => {
            entry.insert(
                crate::value::intern_key("parameters"),
                VmValue::dict(DictMap::new()),
            );
        }
    }
}
