//! Chat-template function-markup recovery for the tagged text-tool grammar.
//!
//! Covers the qwen3 `<function=NAME>` + `<parameter=KEY>` style, the
//! `<invoke name="NAME">` attribute spelling (#3220), and the trailing-JSON
//! dialect `<function=NAME>{json}` (#5252).

use std::collections::BTreeMap;

use super::super::super::type_expr::TypeExpr;
use crate::llm::tools::collect_tool_schemas;
use crate::value::VmValue;

/// Opening markers for the chat-template "function markup" rendering of a
/// tool call. `<function=NAME>` is the qwen3 chat-template style; `<invoke
/// name="NAME">` is the attribute spelling other templates use.
pub(super) const FUNCTION_MARKUP_OPEN: &str = "<function=";
pub(super) const INVOKE_MARKUP_OPEN: &str = "<invoke name=";

/// Parse the chat-template "function markup" rendering of a tool call that
/// native-format models (observed live: qwen3.6 under long context, #3220)
/// sometimes emit as plain assistant text instead of using the provider's
/// structured tool channel:
///
/// ```text
/// <function=edit>
/// <parameter=action>
/// create
/// </parameter>
/// </function>
/// ```
///
/// or the attribute spelling `<invoke name="edit">` +
/// `<parameter name="action">create</parameter>` + `</invoke>`.
///
/// A third dialect some open-weight models emit is a trailing JSON object
/// instead of `<parameter=...>` blocks (#5252):
///
/// ```text
/// <function=echo_marker>{"value": "MK-7Q3Z"}
/// ```
///
/// `body` is the markup WITHOUT the optional `<tool_call>` wrapper. Returns:
/// * `Ok(None)` — body does not open with a plausible function-markup tag
///   (the caller falls through to its other recovery paths);
/// * `Ok(Some(call))` — a complete markup block for a registered tool. A
///   missing `</function>` / `</invoke>` close is tolerated as long as every
///   `<parameter ...>` block is itself properly closed (some emissions close
///   only the outer `</tool_call>`), or the trailing JSON object is balanced;
/// * `Err(msg)` — recognizably function markup but unusable: unknown tool,
///   an unterminated `<parameter ...>` / JSON arguments object (truncation —
///   never dispatch a partial argument value), or a second call sharing the
///   block. The message is model-facing parse feedback.
///
/// Parameter values are typed against the registered tool schema: parameters
/// the schema declares as strings (or whose schema entry is missing) keep
/// their raw bytes verbatim — code in an `edit` `content` value is never
/// JSON-mangled — while non-string parameters are parsed as JSON with a
/// fallback to the raw string.
pub(super) fn parse_function_markup_body(
    body: &str,
    tools_val: Option<&VmValue>,
) -> Result<Option<serde_json::Value>, String> {
    let trimmed = body.trim();
    let (name, after_open, close_tag, style) =
        if let Some(rest) = trimmed.strip_prefix(FUNCTION_MARKUP_OPEN) {
            let Some(gt) = rest.find('>') else {
                return Err(
                    "TOOL CALL TRUNCATED: a `<function=` open tag was never closed with `>` — \
                     the response appears to have been cut off. The call was NOT executed; \
                     re-emit the complete call."
                        .to_string(),
                );
            };
            (
                rest[..gt].trim().trim_matches('"'),
                &rest[gt + 1..],
                "</function>",
                "`<function=...>`",
            )
        } else if let Some(rest) = trimmed.strip_prefix(INVOKE_MARKUP_OPEN) {
            let Some(gt) = rest.find('>') else {
                return Err(
                    "TOOL CALL TRUNCATED: an `<invoke name=` open tag was never closed with \
                     `>` — the response appears to have been cut off. The call was NOT \
                     executed; re-emit the complete call."
                        .to_string(),
                );
            };
            (
                rest[..gt].trim().trim_matches('"'),
                &rest[gt + 1..],
                "</invoke>",
                "`<invoke name=...>`",
            )
        } else {
            return Ok(None);
        };
    // Require a plausible tool identifier; anything else is not tool-call
    // markup (e.g. prose like `<function=a tool of your choice>`), so let the
    // caller's other paths classify it.
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Ok(None);
    }
    let known = super::known_tool_names_with_implicit(tools_val);
    if !known.contains(name) {
        let available: Vec<_> = known.iter().take(20).cloned().collect();
        return Err(format!(
            "Unknown tool '{name}' in chat-template {style} tool-call markup. \
             Available tools: [{}]",
            available.join(", ")
        ));
    }
    // Anything after the close tag (a stray `</tool_call>`, whitespace) is
    // tolerated slop. A missing close tag is tolerated only when every
    // parameter block below is itself closed — see the leftover check.
    let inner = match after_open.find(close_tag) {
        Some(idx) => &after_open[..idx],
        None => after_open,
    };
    let param_types: BTreeMap<String, TypeExpr> = collect_tool_schemas(tools_val, None)
        .into_iter()
        .find(|schema| schema.name == name)
        .map(|schema| {
            schema
                .params
                .into_iter()
                .map(|param| (param.name, param.ty))
                .collect()
        })
        .unwrap_or_default();
    // The attribute spelling tolerates extra attributes after `name="..."`
    // (e.g. `<parameter name="file" string="true">`, the DSML-influenced shape
    // value models emit): requiring `>` right after the name silently failed
    // the match, and the complete call was then misdiagnosed as TRUNCATED —
    // the single largest full-turn parse-kill class in the eval corpus.
    let param_re = regex::Regex::new(
        r#"(?s)<parameter(?:=([A-Za-z0-9_][A-Za-z0-9_.-]*)|\s+name="([^"]+)"(?:\s+[A-Za-z_][A-Za-z0-9_.-]*="[^"]*")*)\s*>(.*?)</parameter>"#,
    )
    .expect("valid function-markup parameter regex");
    let mut args = serde_json::Map::new();
    let mut covered: Vec<(usize, usize)> = Vec::new();
    for captures in param_re.captures_iter(inner) {
        let whole = captures.get(0).expect("whole parameter match");
        covered.push((whole.start(), whole.end()));
        let key = captures
            .get(1)
            .or_else(|| captures.get(2))
            .map(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let raw = captures.get(3).map(|m| m.as_str()).unwrap_or("");
        let value = function_markup_param_value(raw, param_types.get(&key));
        args.insert(key, value);
    }
    let leftover = super::prose_without_ranges(inner, &covered);
    if leftover.contains("<parameter") {
        return Err(format!(
            "TOOL CALL TRUNCATED: a `<parameter ...>` block in the {style} markup for \
             `{name}` was never closed with `</parameter>` — the response appears to have \
             been cut off. The call was NOT executed; re-emit the complete call."
        ));
    }
    if leftover.contains(FUNCTION_MARKUP_OPEN) || leftover.contains(INVOKE_MARKUP_OPEN) {
        return Err(format!(
            "The {style} markup block for `{name}` contained more than one call; \
             emit one call per <tool_call> block."
        ));
    }
    // Dialect: `<function=NAME>{json}` — some open-weight models emit the
    // arguments as a trailing/enclosed JSON object instead of `<parameter=...>`
    // blocks (#5252). Only when no parameter tags were recovered: parameter
    // style wins if both appear, matching the markup grammar that already
    // owns this path. An additive adaptive lane would double-count with the
    // partial call this used to produce (empty args), so the fix stays here.
    let arguments = if args.is_empty() {
        match parse_function_markup_json_args(leftover.trim(), name, style)? {
            Some(json_args) => json_args,
            None => serde_json::Value::Object(args),
        }
    } else {
        serde_json::Value::Object(args)
    };
    Ok(Some(serde_json::json!({
        "id": format!("tc_fnmarkup_{name}"),
        "name": name,
        "arguments": arguments,
    })))
}

/// Parse a trailing JSON-object arguments payload for function markup when the
/// body has no `<parameter=...>` tags. Returns `Ok(None)` when `src` is empty
/// or does not open with `{` (no-arg tool / non-JSON leftover slop). A leading
/// `{` that never closes is truncation; a closed object that is not a JSON
/// object is a hard parse error — never dispatch empty args for those shapes.
fn parse_function_markup_json_args(
    src: &str,
    name: &str,
    style: &str,
) -> Result<Option<serde_json::Value>, String> {
    if src.is_empty() || !src.starts_with('{') {
        return Ok(None);
    }
    let Some(obj_len) = super::balanced_json_object_len(src) else {
        return Err(format!(
            "TOOL CALL TRUNCATED: the JSON arguments object in the {style} markup for \
             `{name}` was never closed — the response appears to have been cut off. The \
             call was NOT executed; re-emit the complete call."
        ));
    };
    let mut arguments: serde_json::Value =
        serde_json::from_str(&src[..obj_len]).map_err(|error| {
            format!(
                "The {style} markup for `{name}` had a JSON arguments object that did not \
                 parse: {error}. The call was NOT executed."
            )
        })?;
    if !arguments.is_object() {
        return Err(format!(
            "JSON arguments for tool '{name}' in {style} markup must be an object, got \
             `{arguments}`."
        ));
    }
    // Same JSON-string channel as `parse_json_tool_call_body` / nested XML:
    // decode escaped markup delimiters exactly once at this boundary.
    super::html_entities::decode_html_entities_in_args(&mut arguments);
    Ok(Some(arguments))
}

/// Type a raw function-markup parameter value against its schema entry.
///
/// The chat-template markup carries no type information — values are raw
/// text framed by the template's own newlines. Strip exactly one leading and
/// one trailing newline (the template's framing, not the value's), then:
/// string-typed (or schema-unknown) parameters keep the framed bytes
/// verbatim; non-string parameters are parsed as JSON, falling back to the
/// raw string when they don't parse.
fn function_markup_param_value(raw: &str, ty: Option<&TypeExpr>) -> serde_json::Value {
    let framed = raw
        .strip_prefix("\r\n")
        .or_else(|| raw.strip_prefix('\n'))
        .unwrap_or(raw);
    let framed = framed
        .strip_suffix("\r\n")
        .or_else(|| framed.strip_suffix('\n'))
        .unwrap_or(framed);
    let wants_string = ty.map(type_expr_wants_string).unwrap_or(true);
    if wants_string {
        return serde_json::Value::String(framed.to_string());
    }
    match serde_json::from_str::<serde_json::Value>(framed.trim()) {
        Ok(value) => value,
        Err(_) => serde_json::Value::String(framed.to_string()),
    }
}

/// True when a schema type only admits string values, so a raw markup value
/// must be kept verbatim rather than JSON-parsed.
fn type_expr_wants_string(ty: &TypeExpr) -> bool {
    match ty {
        TypeExpr::Primitive(name) => name == "string",
        TypeExpr::Literal(value) => value.is_string(),
        TypeExpr::Union(items) => items.iter().all(type_expr_wants_string),
        _ => false,
    }
}

/// Try to parse top-level chat-template function markup (no `<tool_call>`
/// wrapper) at `cursor`. Returns `None` when the cursor is not on a
/// plausible function-markup opener; otherwise the parsed call or the
/// model-facing diagnostic, each paired with the cursor position after the
/// consumed block. Line anchoring and markdown-fence exclusion are enforced
/// by the caller (the main scanner checks both before dispatching tags), so
/// prose mentioning the syntax inline or fenced examples never reach this.
#[allow(clippy::type_complexity)]
pub(super) fn try_parse_top_level_function_markup(
    src: &str,
    cursor: usize,
    tools_val: Option<&VmValue>,
) -> Option<Result<(serde_json::Value, usize), (String, usize)>> {
    let rest = &src[cursor..];
    let close_tag = if rest.starts_with(FUNCTION_MARKUP_OPEN) {
        "</function>"
    } else if rest.starts_with(INVOKE_MARKUP_OPEN) {
        "</invoke>"
    } else {
        return None;
    };
    let end = rest
        .find(close_tag)
        .map(|idx| cursor + idx + close_tag.len())
        .unwrap_or(src.len());
    match parse_function_markup_body(&src[cursor..end], tools_val) {
        Ok(Some(call)) => Some(Ok((call, end))),
        Err(msg) => Some(Err((msg, end))),
        Ok(None) => None,
    }
}
