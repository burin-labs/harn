use super::*;

/// Three-way resolution of `tool_search.mode` against the provider's
/// native capability. Kept as a private enum so the option-parse path
/// reads linearly; the `Client` variant leaves the fallback to the Harn
/// agent loop, and the `Native` variant feeds provider-native injection.
pub(super) enum ToolSearchResolution {
    Native,
    Client,
}

/// Parse the `tool_search` option into a ToolSearchConfig.
///
/// Accepts:
///   - `nil` / absent / `false` → None (no tool_search engaged)
///   - `true` → default (bm25 + auto)
///   - `"bm25"` | `"regex"` | `"hybrid"` → that variant + auto
///   - `{ variant?, mode?, strategy?, always_loaded?, name? }` → explicit.
///     Non-string strategies are Harn-side custom scorers, so they force
///     client-mode resolution.
pub(super) fn parse_tool_search_option(
    options: Option<&crate::value::DictMap>,
) -> Result<Option<crate::llm::api::ToolSearchConfig>, VmError> {
    use crate::llm::api::{ToolSearchConfig, ToolSearchMode, ToolSearchVariant};

    let raw = match options.and_then(|o| o.get("tool_search")) {
        Some(v) => v,
        None => return Ok(None),
    };

    let variant_from_short = |s: &str| -> Result<ToolSearchVariant, VmError> {
        match s {
            "bm25" => Ok(ToolSearchVariant::Bm25),
            "regex" => Ok(ToolSearchVariant::Regex),
            "hybrid" => Ok(ToolSearchVariant::Hybrid),
            other => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!(
                    "tool_search.variant: expected \"bm25\", \"regex\", or \"hybrid\", got \"{other}\""
                ),
            )))),
        }
    };
    let mode_from_short = |s: &str| -> Result<ToolSearchMode, VmError> {
        match s {
            "auto" => Ok(ToolSearchMode::Auto),
            "native" => Ok(ToolSearchMode::Native),
            "client" => Ok(ToolSearchMode::Client),
            other => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!(
                "tool_search.mode: expected \"auto\" | \"native\" | \"client\", got \"{other}\""
            ),
            )))),
        }
    };
    let validate_strategy = |s: &str| -> Result<(), VmError> {
        match s {
            "bm25" | "regex" | "hybrid" => Ok(()),
            other => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!(
                    "tool_search.strategy: expected \"bm25\" | \"regex\" | \"hybrid\", got \"{other}\""
                ),
            )))),
        }
    };

    match raw {
        VmValue::Nil => Ok(None),
        VmValue::Bool(false) => Ok(None),
        VmValue::Bool(true) => Ok(Some(ToolSearchConfig::default_bm25_auto())),
        VmValue::String(s) => Ok(Some(ToolSearchConfig {
            variant: variant_from_short(s.as_str())?,
            mode: ToolSearchMode::Auto,
        })),
        VmValue::Dict(d) => {
            let variant = match d.get("variant") {
                Some(VmValue::String(s)) => variant_from_short(s.as_str())?,
                Some(_) => {
                    return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                        "tool_search.variant: expected a string",
                    ))));
                }
                None => ToolSearchVariant::Bm25,
            };
            let mode = match d.get("mode") {
                Some(VmValue::String(s)) => mode_from_short(s.as_str())?,
                Some(_) => {
                    return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                        "tool_search.mode: expected a string",
                    ))));
                }
                None => ToolSearchMode::Auto,
            };
            match d.get("always_loaded") {
                Some(VmValue::List(_)) | None => {}
                Some(_) => {
                    return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                        "tool_search.always_loaded: expected a list of tool names",
                    ))));
                }
            }
            let custom_strategy = match d.get("strategy") {
                Some(VmValue::String(s)) => {
                    validate_strategy(s.as_str())?;
                    false
                }
                Some(VmValue::Closure(_)) => true,
                Some(VmValue::Dict(strategy)) => {
                    if matches!(strategy.get("handler"), Some(VmValue::Closure(_))) {
                        true
                    } else {
                        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                            "tool_search.strategy: expected \"bm25\" | \"regex\" | \"hybrid\", a scorer closure, or {handler: closure}",
                        ))));
                    }
                }
                Some(_) => {
                    return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                        "tool_search.strategy: expected \"bm25\" | \"regex\" | \"hybrid\", a scorer closure, or {handler: closure}",
                    ))));
                }
                None => false,
            };
            if custom_strategy && matches!(mode, ToolSearchMode::Native) {
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    "tool_search.strategy: custom scorers are client-only; set mode: \"client\" or \"auto\"",
                ))));
            }
            match d.get("name") {
                Some(VmValue::String(s)) => {
                    let s = s.as_str().trim();
                    if s.is_empty() {
                        None
                    } else {
                        Some(s.to_string())
                    }
                }
                Some(VmValue::Nil) | None => None,
                Some(_) => {
                    return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                        "tool_search.name: expected a string",
                    ))));
                }
            };
            Ok(Some(ToolSearchConfig {
                variant: if custom_strategy {
                    ToolSearchVariant::Hybrid
                } else {
                    variant
                },
                mode,
            }))
        }
        _ => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "tool_search: expected bool, string (\"bm25\"/\"regex\"/\"hybrid\"), or dict \
             ({variant, mode, strategy, always_loaded, name})",
        )))),
    }
}
