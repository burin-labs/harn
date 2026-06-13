//! `schema_recover(text, schema, opts?)` — best-effort recovery of
//! malformed LLM output against a target schema. Implements the
//! three-tier fallback that scripts (notably `burin-code`'s
//! `grade-lora-corpus.harn` `normalize_grader_output()`) used to
//! hand-roll: parse → extract → regex → optional LLM repair.
//!
//! Pipeline (each stage runs only if the previous failed):
//!
//! 1. **Direct parse** (`stage: "parsed"`) — `serde_json::from_str` on
//!    the raw text, then validate against the schema.
//! 2. **Extracted** (`stage: "extracted"`) — `extract_json_from_text`
//!    lifts JSON from code fences or balanced braces, then parses +
//!    validates. Recovers responses where the model wrapped JSON in
//!    prose or markdown.
//! 3. **Regex** (`stage: "regex"`) — For each top-level scalar field
//!    in `schema.properties`, scan for `"key": value` / `key: value` /
//!    YAML-shaped patterns and assemble a partial dict. Useful when
//!    the model dropped quotes or produced YAML-ish output. Only top-
//!    level scalars (string / integer / number / boolean) are
//!    recovered this way — nested objects are too unreliable.
//! 4. **LLM repair** (`stage: "llm_repair"`, optional) — Single-shot
//!    `llm_call` with the malformed text and schema in the prompt,
//!    asking for valid JSON. Disabled with `{llm_repair: false}` or
//!    when no LLM provider is configured. Repair runs with
//!    `schema_retries: 0` to fail fast; cost amplification is the
//!    caller's problem if they want more.
//!
//! Returns a diagnostic envelope dict so callers can dispatch on
//! `ok` / `error_category` / `stage` without try/catch:
//!
//! ```harn
//! let r = schema_recover(raw_text, schema)
//! if r.ok {
//!   process(r.data)
//! } else {
//!   log("recovery failed:", r.error_category, "stage:", r.stage)
//! }
//! ```
//!
//! Unlike `llm_call_structured_result`, this helper takes already-
//! produced text rather than running a fresh structured call. The
//! intended use is downstream of an `llm_call(...)` that returned
//! prose or that used `output_validation: "off"`, when the caller
//! wants to recover the schema-shaped payload after the fact.

use crate::value::VmDictExt;
use std::sync::Arc;

use serde_json::Value as JsonValue;

use crate::stdlib::{json_to_vm_value, schema_result_value};
use crate::value::{VmError, VmValue};

use super::helpers::extract_llm_options;
use super::{execute_schema_retry_loop, structured_output_errors};

const STAGE_PARSED: &str = "parsed";
const STAGE_EXTRACTED: &str = "extracted";
const STAGE_REGEX: &str = "regex";
const STAGE_LLM_REPAIR: &str = "llm_repair";
const STAGE_FAILED: &str = "failed";

const ERR_SCHEMA_VALIDATION: &str = "schema_validation";
const ERR_REPAIR_FAILED: &str = "repair_failed";
const ERR_TRANSPORT: &str = "transport";

/// Public entry point used by `register_llm_builtins`. The bridge
/// argument is forwarded to the optional LLM repair pass so worker /
/// agent contexts get the same provider routing as the surrounding
/// agent loop. Pass `None` from non-bridge call sites.
pub(crate) async fn schema_recover_impl(
    args: Vec<VmValue>,
    bridge: Option<&Arc<crate::bridge::HostBridge>>,
) -> Result<VmValue, VmError> {
    if args.len() < 2 {
        return Err(VmError::Runtime(
            "schema_recover: expected (text: string, schema: dict, opts?: dict)".to_string(),
        ));
    }
    let text = args[0].display();
    let schema_value = match &args[1] {
        VmValue::Dict(_) => args[1].clone(),
        other => {
            return Err(VmError::Runtime(format!(
                "schema_recover: schema must be a dict, got {}",
                other.type_name(),
            )));
        }
    };
    let opts = args.get(2).and_then(|a| a.as_dict()).cloned();
    let apply_defaults = opt_bool_field(&opts, "apply_defaults");

    let mut last_errors: Vec<String>;
    let mut attempts: usize = 0;

    // Stage 1: direct parse.
    attempts += 1;
    match try_parse_and_validate(&text, &schema_value, apply_defaults) {
        Ok(data) => {
            return Ok(envelope_success(data, &text, STAGE_PARSED, attempts, false));
        }
        Err(errs) => {
            last_errors = errs;
        }
    }

    // Stage 2: extract JSON from prose / fences and try again.
    let extracted = crate::stdlib::json::extract_json_from_text(&text);
    if extracted.trim() != text.trim() {
        attempts += 1;
        match try_parse_and_validate(&extracted, &schema_value, apply_defaults) {
            Ok(data) => {
                return Ok(envelope_success(
                    data,
                    &text,
                    STAGE_EXTRACTED,
                    attempts,
                    false,
                ));
            }
            Err(errs) => {
                last_errors = errs;
            }
        }
    }

    // Stage 3: regex-based field extraction for top-level scalars.
    attempts += 1;
    match try_regex_recover(&text, &schema_value, apply_defaults) {
        Ok(Some(data)) => {
            return Ok(envelope_success(data, &text, STAGE_REGEX, attempts, false));
        }
        Ok(None) => {
            // No fields recoverable via regex — keep last_errors.
        }
        Err(errs) => {
            last_errors = errs;
        }
    }

    // Stage 4: LLM repair pass (optional, opt-out).
    let repair = parse_llm_repair_config(&opts);
    if repair.enabled {
        attempts += 1;
        match run_llm_repair(&text, &schema_value, &repair, &opts, bridge).await {
            Ok(Some(data)) => {
                return Ok(envelope_success(
                    data,
                    &text,
                    STAGE_LLM_REPAIR,
                    attempts,
                    true,
                ));
            }
            Ok(None) => {
                return Ok(envelope_failure(
                    &text,
                    STAGE_LLM_REPAIR,
                    ERR_REPAIR_FAILED,
                    "LLM repair pass returned invalid JSON",
                    attempts,
                ));
            }
            Err(message) => {
                return Ok(envelope_failure(
                    &text,
                    STAGE_LLM_REPAIR,
                    ERR_TRANSPORT,
                    &message,
                    attempts,
                ));
            }
        }
    }

    let message = if last_errors.is_empty() {
        "schema_recover: no recoverable JSON found".to_string()
    } else {
        last_errors.join("; ")
    };
    Ok(envelope_failure(
        &text,
        STAGE_FAILED,
        ERR_SCHEMA_VALIDATION,
        &message,
        attempts,
    ))
}

/// Try `serde_json` parse + schema validation. Returns the validated
/// (and possibly default-applied) value on success, or the list of
/// validation / parse errors on failure.
fn try_parse_and_validate(
    text: &str,
    schema: &VmValue,
    apply_defaults: bool,
) -> Result<VmValue, Vec<String>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(vec!["empty input".to_string()]);
    }
    let parsed = match serde_json::from_str::<JsonValue>(trimmed) {
        Ok(v) => v,
        Err(e) => return Err(vec![format!("JSON parse error: {e}")]),
    };
    let vm_value = json_to_vm_value(&parsed);
    let result = schema_result_value(&vm_value, schema, apply_defaults);
    extract_validation_outcome(&result)
}

/// Pull the validated payload out of a `Result.Ok(value)` or surface
/// the error list from `Result.Err({errors, ...})`.
fn extract_validation_outcome(result: &VmValue) -> Result<VmValue, Vec<String>> {
    match result {
        VmValue::EnumVariant(enum_variant) if enum_variant.has_enum_name("Result") => {
            match enum_variant.variant.as_ref() {
                "Ok" => Ok(enum_variant.fields.first().cloned().unwrap_or(VmValue::Nil)),
                "Err" => {
                    let errors = enum_variant
                        .fields
                        .first()
                        .and_then(|payload| payload.as_dict())
                        .and_then(|payload| payload.get("errors"))
                        .and_then(|errors| match errors {
                            VmValue::List(items) => {
                                Some(items.iter().map(|err| err.display()).collect())
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| vec!["schema validation failed".to_string()]);
                    Err(errors)
                }
                other => Err(vec![format!(
                    "unexpected Result variant from schema validation: {other}"
                )]),
            }
        }
        _ => Err(vec!["schema validation did not return a Result".to_string()]),
    }
}

/// Walk the schema's top-level scalar properties and try to scrape
/// `"key": value` / `key: value` / `key = value` patterns out of free-
/// form text. Returns:
///
/// - `Ok(Some(dict))` — at least one field was scraped AND the
///   resulting dict (filled in with defaults from the schema where
///   possible) validates.
/// - `Ok(None)` — nothing was scraped or what was scraped didn't
///   form a valid object (the caller falls through to the next
///   stage without treating this as a validation error).
/// - `Err(errors)` — the schema is unusable (e.g. not an object
///   schema).
fn try_regex_recover(
    text: &str,
    schema: &VmValue,
    apply_defaults: bool,
) -> Result<Option<VmValue>, Vec<String>> {
    let schema_dict = match schema.as_dict() {
        Some(d) => d,
        None => return Ok(None),
    };
    // We only attempt regex recovery on object-shaped schemas — this
    // matches the canonical grade-lora-corpus.harn pattern where the
    // schema is `{type: "object", properties: {...}}`. Non-object
    // schemas (e.g. raw arrays / scalars) skip this stage cleanly.
    let is_object = matches!(
        schema_dict.get("type"),
        Some(VmValue::String(s)) if s.as_ref() == "object"
    ) || schema_dict.contains_key("properties");
    if !is_object {
        return Ok(None);
    }
    let properties = match schema_dict.get("properties") {
        Some(VmValue::Dict(p)) => p.clone(),
        _ => return Ok(None),
    };

    let mut recovered: crate::value::DictMap = crate::value::DictMap::new();
    let mut any = false;
    for (field, field_schema) in properties.iter() {
        let field_type = field_type_name(field_schema);
        if field_type.is_none() {
            continue;
        }
        let ty = field_type.unwrap();
        if let Some(value) = scrape_field(text, field, ty, field_schema) {
            recovered.insert(field.clone(), value);
            any = true;
        }
    }
    if !any {
        return Ok(None);
    }
    let candidate = VmValue::dict(recovered);
    let result = schema_result_value(&candidate, schema, apply_defaults);
    match extract_validation_outcome(&result) {
        Ok(data) => Ok(Some(data)),
        Err(_) => Ok(None),
    }
}

/// Return the JSON-Schema-style type name for a field schema, treating
/// nested object / array / null / unknown shapes as "skip" — only
/// scalars (string / integer / number / boolean) participate in regex
/// recovery.
fn field_type_name(field_schema: &VmValue) -> Option<&'static str> {
    let dict = field_schema.as_dict()?;
    let ty = dict.get("type")?;
    match ty {
        VmValue::String(s) => match s.as_ref() {
            "string" => Some("string"),
            "integer" => Some("integer"),
            "number" => Some("number"),
            "boolean" => Some("boolean"),
            _ => None,
        },
        // Union types like ["string", "null"] — pick the first scalar.
        VmValue::List(items) => items.iter().find_map(|item| {
            if let VmValue::String(s) = item {
                match s.as_ref() {
                    "string" => Some("string"),
                    "integer" => Some("integer"),
                    "number" => Some("number"),
                    "boolean" => Some("boolean"),
                    _ => None,
                }
            } else {
                None
            }
        }),
        _ => None,
    }
}

/// Try a sequence of progressively-loose patterns to extract a single
/// scalar value for `field` from `text`. Patterns mirror the shapes
/// LLMs commonly produce when they fail JSON validity:
///
/// 1. `"field": "value"`   (quoted JSON-ish)
/// 2. `"field": value`     (quoted key, bare value)
/// 3. `field: "value"`     (YAML-ish quoted value)
/// 4. `field: value`       (bare YAML)
/// 5. `field = value`      (env/ini-ish)
fn scrape_field(
    text: &str,
    field: &str,
    field_type: &'static str,
    field_schema: &VmValue,
) -> Option<VmValue> {
    let escaped = regex::escape(field);
    let patterns = [
        // Quoted key, quoted value.
        format!(r#""{escaped}"\s*:\s*"((?:[^"\\]|\\.)*)""#),
        // Quoted key, bare value (until comma / brace / newline).
        format!(r#""{escaped}"\s*:\s*([^,\n\r}}]+)"#),
        // Bare key with quoted value (YAML-ish).
        format!(r#"\b{escaped}\s*[:=]\s*"((?:[^"\\]|\\.)*)""#),
        // Bare key with bare value (terminated by newline / comma /
        // brace). This is the most permissive pattern and goes last.
        format!(r"\b{escaped}\s*[:=]\s*([^,\n\r}}]+)"),
    ];
    for pat in &patterns {
        let re = match regex::Regex::new(pat) {
            Ok(re) => re,
            Err(_) => continue,
        };
        if let Some(caps) = re.captures(text) {
            if let Some(m) = caps.get(1) {
                let raw = m.as_str().trim();
                if let Some(value) = coerce_scalar(raw, field_type, field_schema) {
                    return Some(value);
                }
            }
        }
    }
    None
}

/// Convert a raw scraped string to the schema-declared scalar type.
/// Unquoted strings get cleaned of trailing punctuation; numerics
/// are parsed; booleans accept the common spellings (true/yes/y/1).
fn coerce_scalar(raw: &str, field_type: &str, field_schema: &VmValue) -> Option<VmValue> {
    let cleaned = raw
        .trim_end_matches([',', ';', '}', ']'])
        .trim()
        .to_string();
    if cleaned.is_empty() {
        return None;
    }
    match field_type {
        "string" => {
            let unquoted = strip_surrounding_quotes(&cleaned);
            let unescaped = unescape_json_string(&unquoted);
            // Reject obvious null tokens for non-nullable string fields.
            if unescaped.eq_ignore_ascii_case("null") && !field_allows_null(field_schema) {
                return None;
            }
            Some(VmValue::String(std::sync::Arc::from(unescaped.as_str())))
        }
        "integer" => {
            let n: i64 = cleaned.parse().ok()?;
            Some(VmValue::Int(n))
        }
        "number" => {
            // Try integer first to preserve numeric precision; fall
            // back to float for fractional values.
            if let Ok(n) = cleaned.parse::<i64>() {
                Some(VmValue::Int(n))
            } else {
                let n: f64 = cleaned.parse().ok()?;
                Some(VmValue::Float(n))
            }
        }
        "boolean" => parse_bool_token(&cleaned).map(VmValue::Bool),
        _ => None,
    }
}

fn parse_bool_token(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "true" | "yes" | "y" | "on" | "1" => Some(true),
        "false" | "no" | "n" | "off" | "0" => Some(false),
        _ => None,
    }
}

fn strip_surrounding_quotes(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

/// Best-effort JSON string escape decode: handles the common escapes
/// (`\"`, `\\`, `\n`, `\t`, `\r`). Unknown escapes pass through.
fn unescape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('/') => out.push('/'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn field_allows_null(field_schema: &VmValue) -> bool {
    let dict = match field_schema.as_dict() {
        Some(d) => d,
        None => return false,
    };
    match dict.get("type") {
        Some(VmValue::String(s)) => s.as_ref() == "null",
        Some(VmValue::List(items)) => items
            .iter()
            .any(|item| matches!(item, VmValue::String(s) if s.as_ref() == "null")),
        _ => false,
    }
}

#[derive(Clone)]
struct LlmRepairConfig {
    enabled: bool,
    overrides: crate::value::DictMap,
}

fn parse_llm_repair_config(opts: &Option<crate::value::DictMap>) -> LlmRepairConfig {
    let Some(opts) = opts.as_ref() else {
        return LlmRepairConfig {
            enabled: true,
            overrides: crate::value::DictMap::new(),
        };
    };
    let raw = opts.get("llm_repair");
    match raw {
        None => LlmRepairConfig {
            enabled: true,
            overrides: crate::value::DictMap::new(),
        },
        Some(VmValue::Nil) => LlmRepairConfig {
            enabled: true,
            overrides: crate::value::DictMap::new(),
        },
        Some(VmValue::Bool(b)) => LlmRepairConfig {
            enabled: *b,
            overrides: crate::value::DictMap::new(),
        },
        Some(VmValue::Dict(d)) => {
            let enabled = match d.get("enabled") {
                None => true,
                Some(VmValue::Bool(false)) => false,
                Some(VmValue::Nil) => true,
                Some(_) => true,
            };
            let mut overrides: crate::value::DictMap = (**d).clone();
            overrides.remove("enabled");
            LlmRepairConfig { enabled, overrides }
        }
        // Tolerant: any other shape disables the repair pass cleanly
        // rather than throwing — recovery is best-effort.
        Some(_) => LlmRepairConfig {
            enabled: false,
            overrides: crate::value::DictMap::new(),
        },
    }
}

fn opt_bool_field(opts: &Option<crate::value::DictMap>, key: &str) -> bool {
    matches!(
        opts.as_ref().and_then(|o| o.get(key)),
        Some(VmValue::Bool(true))
    )
}

/// Run the LLM repair pass: build a corrective prompt and call into
/// the schema-retry loop with `schema_retries: 0` (single shot). The
/// schema is installed on the call so the provider can use native
/// JSON-mode where available.
///
/// Returns:
/// - `Ok(Some(data))` — repair produced valid JSON that schema-validates.
/// - `Ok(None)` — repair completed but the result still failed validation.
/// - `Err(message)` — the LLM transport itself failed (no provider, network, etc).
async fn run_llm_repair(
    text: &str,
    schema: &VmValue,
    repair: &LlmRepairConfig,
    base_opts: &Option<crate::value::DictMap>,
    bridge: Option<&Arc<crate::bridge::HostBridge>>,
) -> Result<Option<VmValue>, String> {
    let prompt = build_repair_prompt(text, schema);
    let merged_options = merge_repair_options(base_opts.as_ref(), &repair.overrides, schema);
    let merged_dict = Some(merged_options.clone());
    let args = vec![
        VmValue::String(std::sync::Arc::from(prompt.as_str())),
        // System slot — the prompt carries instructions inline so the
        // repair pass works regardless of any caller-set system text.
        VmValue::Nil,
        VmValue::dict(merged_options),
    ];
    let opts = extract_llm_options(&args).map_err(|e| e.to_string())?;
    let outcome = execute_schema_retry_loop(None, opts.clone(), merged_dict, bridge)
        .await
        .map_err(|e| e.to_string())?;
    if !outcome.errors.is_empty() {
        return Ok(None);
    }
    // Re-validate against the schema using the same path the rest of
    // schema_recover uses so the success criterion is identical (and
    // we get back the validated, default-applied value).
    let errors = structured_output_errors(&outcome.vm_result, &opts);
    if !errors.is_empty() {
        return Ok(None);
    }
    let data = outcome
        .vm_result
        .as_dict()
        .and_then(|d| d.get("data").cloned())
        .unwrap_or(VmValue::Nil);
    Ok(Some(data))
}

fn build_repair_prompt(raw_text: &str, schema: &VmValue) -> String {
    let schema_text = schema_to_compact_json(schema);
    let mut bindings = crate::value::DictMap::new();
    bindings.put_str("schema_text", schema_text);
    bindings.put_str("raw_text", raw_text);
    crate::stdlib::template::render_stdlib_prompt_asset(
        "llm/prompts/schema_recover_repair.harn.prompt",
        Some(&bindings),
    )
    .expect("schema_recover_repair.harn.prompt is embedded and must render")
}

fn schema_to_compact_json(schema: &VmValue) -> String {
    let json = super::helpers::vm_value_to_json(schema);
    serde_json::to_string(&json).unwrap_or_else(|_| "{}".to_string())
}

fn merge_repair_options(
    base: Option<&crate::value::DictMap>,
    overrides: &crate::value::DictMap,
    schema: &VmValue,
) -> crate::value::DictMap {
    let mut merged: crate::value::DictMap = base.cloned().unwrap_or_default();
    // The repair pass is always single-shot inside schema_recover —
    // burning the caller's `schema_retries` budget here would amplify
    // cost and the outer recovery cascade has already done what it
    // can. Set explicitly rather than relying on defaults.
    merged.insert("schema_retries".to_string(), VmValue::Int(0));
    // Strip schema_recover-specific keys so they don't leak into
    // `extract_llm_options` as unknown provider params.
    merged.remove("llm_repair");
    merged.remove("apply_defaults");
    // Install the schema on the call so providers can use their
    // native JSON-mode and so the schema-retry loop's validation runs.
    merged.insert("output_schema".to_string(), schema.clone());
    merged.insert("json_schema".to_string(), schema.clone());
    merged
        .entry("output_format".to_string())
        .or_insert_with(|| {
            let mut fmt = crate::value::DictMap::new();
            fmt.put_str("kind", "json_schema");
            fmt.insert("schema".to_string(), schema.clone());
            fmt.insert("strict".to_string(), VmValue::Bool(true));
            VmValue::dict(fmt)
        });
    merged
        .entry("output_validation".to_string())
        .or_insert(VmValue::String(std::sync::Arc::from("error")));
    merged
        .entry("response_format".to_string())
        .or_insert(VmValue::String(std::sync::Arc::from("json")));
    for (k, v) in overrides {
        merged.insert(k.clone(), v.clone());
    }
    merged
}

fn envelope_success(
    data: VmValue,
    raw_text: &str,
    stage: &str,
    attempts: usize,
    repaired: bool,
) -> VmValue {
    let mut env = crate::value::DictMap::new();
    env.insert("ok".to_string(), VmValue::Bool(true));
    env.insert("data".to_string(), data);
    env.put_str("raw_text", raw_text);
    env.put_str("error", "");
    env.insert("error_category".to_string(), VmValue::Nil);
    env.insert("attempts".to_string(), VmValue::Int(attempts as i64));
    env.put_str("stage", stage);
    env.insert("repaired".to_string(), VmValue::Bool(repaired));
    VmValue::dict(env)
}

fn envelope_failure(
    raw_text: &str,
    stage: &str,
    error_category: &str,
    error_message: &str,
    attempts: usize,
) -> VmValue {
    let mut env = crate::value::DictMap::new();
    env.insert("ok".to_string(), VmValue::Bool(false));
    env.insert("data".to_string(), VmValue::Nil);
    env.put_str("raw_text", raw_text);
    env.put_str("error", error_message);
    env.put_str("error_category", error_category);
    env.insert("attempts".to_string(), VmValue::Int(attempts as i64));
    env.put_str("stage", stage);
    env.insert("repaired".to_string(), VmValue::Bool(false));
    VmValue::dict(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::VmDictExt;

    fn person_schema() -> VmValue {
        let mut name = crate::value::DictMap::new();
        name.put_str("type", "string");
        let mut age = crate::value::DictMap::new();
        age.put_str("type", "integer");
        let mut active = crate::value::DictMap::new();
        active.put_str("type", "boolean");
        let mut props = crate::value::DictMap::new();
        props.insert("name".to_string(), VmValue::dict(name));
        props.insert("age".to_string(), VmValue::dict(age));
        props.insert("active".to_string(), VmValue::dict(active));
        let required = VmValue::List(std::sync::Arc::new(vec![
            VmValue::String(std::sync::Arc::from("name")),
            VmValue::String(std::sync::Arc::from("age")),
        ]));
        let mut schema = crate::value::DictMap::new();
        schema.put_str("type", "object");
        schema.insert("properties".to_string(), VmValue::dict(props));
        schema.insert("required".to_string(), required);
        VmValue::dict(schema)
    }

    #[test]
    fn parses_clean_json_directly() {
        let schema = person_schema();
        let result = try_parse_and_validate(
            r#"{"name": "Ada", "age": 36, "active": true}"#,
            &schema,
            false,
        )
        .unwrap();
        let dict = result.as_dict().unwrap();
        assert_eq!(dict.get("name").unwrap().display(), "Ada");
        assert_eq!(dict.get("age").unwrap().as_int(), Some(36));
        assert!(matches!(dict.get("active"), Some(VmValue::Bool(true))));
    }

    #[test]
    fn rejects_validation_failure() {
        let schema = person_schema();
        let err = try_parse_and_validate(r#"{"name": 42}"#, &schema, false).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn extracts_field_with_quoted_key_and_value() {
        let schema = person_schema();
        let scraped = scrape_field(
            r#"the result is "name": "Ada Lovelace", others omitted"#,
            "name",
            "string",
            schema
                .as_dict()
                .unwrap()
                .get("properties")
                .unwrap()
                .as_dict()
                .unwrap()
                .get("name")
                .unwrap(),
        );
        assert_eq!(scraped.unwrap().display(), "Ada Lovelace");
    }

    #[test]
    fn extracts_field_with_yaml_shape() {
        let schema = person_schema();
        let scraped = scrape_field(
            "name: Ada\nage: 36\nactive: yes\n",
            "age",
            "integer",
            schema
                .as_dict()
                .unwrap()
                .get("properties")
                .unwrap()
                .as_dict()
                .unwrap()
                .get("age")
                .unwrap(),
        );
        assert_eq!(scraped.unwrap().as_int(), Some(36));
    }

    #[test]
    fn extracts_boolean_via_yes_token() {
        let schema = person_schema();
        let scraped = scrape_field(
            "name: Ada\nage: 36\nactive: yes\n",
            "active",
            "boolean",
            schema
                .as_dict()
                .unwrap()
                .get("properties")
                .unwrap()
                .as_dict()
                .unwrap()
                .get("active")
                .unwrap(),
        );
        assert!(matches!(scraped, Some(VmValue::Bool(true))));
    }

    #[test]
    fn regex_recover_assembles_partial_dict() {
        let schema = person_schema();
        let raw = "Here is the answer: name: \"Grace Hopper\", age: 85, active: false";
        let result = try_regex_recover(raw, &schema, false).unwrap().unwrap();
        let dict = result.as_dict().unwrap();
        assert_eq!(dict.get("name").unwrap().display(), "Grace Hopper");
        assert_eq!(dict.get("age").unwrap().as_int(), Some(85));
        assert!(matches!(dict.get("active"), Some(VmValue::Bool(false))));
    }

    #[test]
    fn regex_recover_returns_none_when_required_field_missing() {
        let schema = person_schema();
        // Only `active` (non-required) is present — required `name`
        // and `age` are missing, so validation fails and the helper
        // returns None to let the next stage run.
        let raw = "active: true";
        let outcome = try_regex_recover(raw, &schema, false).unwrap();
        assert!(outcome.is_none());
    }

    #[test]
    fn regex_recover_skips_when_schema_is_not_object() {
        let mut scalar = crate::value::DictMap::new();
        scalar.put_str("type", "string");
        let schema = VmValue::dict(scalar);
        let outcome = try_regex_recover("hello", &schema, false).unwrap();
        assert!(outcome.is_none());
    }

    #[test]
    fn coerce_handles_unquoted_string_with_trailing_punct() {
        let mut field = crate::value::DictMap::new();
        field.put_str("type", "string");
        let v = coerce_scalar("Ada,", "string", &VmValue::dict(field)).unwrap();
        assert_eq!(v.display(), "Ada");
    }

    #[test]
    fn coerce_handles_escaped_quotes_in_string() {
        let mut field = crate::value::DictMap::new();
        field.put_str("type", "string");
        let v = coerce_scalar("he said \\\"hi\\\"", "string", &VmValue::dict(field)).unwrap();
        assert_eq!(v.display(), "he said \"hi\"");
    }

    #[test]
    fn coerce_rejects_null_for_non_nullable_string() {
        let mut field = crate::value::DictMap::new();
        field.put_str("type", "string");
        let v = coerce_scalar("null", "string", &VmValue::dict(field));
        assert!(v.is_none());
    }

    #[test]
    fn parse_repair_config_disable_via_bool() {
        let mut opts = crate::value::DictMap::new();
        opts.insert("llm_repair".to_string(), VmValue::Bool(false));
        let cfg = parse_llm_repair_config(&Some(opts));
        assert!(!cfg.enabled);
    }

    #[test]
    fn parse_repair_config_enabled_when_unspecified() {
        let cfg = parse_llm_repair_config(&None);
        assert!(cfg.enabled);
    }

    #[test]
    fn parse_repair_config_dict_extracts_overrides() {
        let mut repair = crate::value::DictMap::new();
        repair.put_str("model", "local:fix");
        repair.insert("max_tokens".to_string(), VmValue::Int(400));
        let mut opts = crate::value::DictMap::new();
        opts.insert("llm_repair".to_string(), VmValue::dict(repair));
        let cfg = parse_llm_repair_config(&Some(opts));
        assert!(cfg.enabled);
        assert_eq!(
            cfg.overrides.get("model").map(VmValue::display).as_deref(),
            Some("local:fix")
        );
        assert_eq!(
            cfg.overrides.get("max_tokens").and_then(VmValue::as_int),
            Some(400)
        );
    }

    #[test]
    fn merge_repair_caps_schema_retries_and_installs_schema() {
        let schema = person_schema();
        let mut base = crate::value::DictMap::new();
        base.insert("schema_retries".to_string(), VmValue::Int(7));
        base.insert("llm_repair".to_string(), VmValue::Bool(true));
        base.insert("apply_defaults".to_string(), VmValue::Bool(true));
        let merged = merge_repair_options(Some(&base), &crate::value::DictMap::new(), &schema);
        assert_eq!(
            merged.get("schema_retries").and_then(VmValue::as_int),
            Some(0)
        );
        assert!(merged.contains_key("output_schema"));
        assert!(!merged.contains_key("llm_repair"));
        assert!(!merged.contains_key("apply_defaults"));
        assert_eq!(
            merged
                .get("output_validation")
                .map(VmValue::display)
                .as_deref(),
            Some("error")
        );
    }

    #[test]
    fn merge_repair_overrides_win_over_base() {
        let schema = person_schema();
        let mut base = crate::value::DictMap::new();
        base.put_str("model", "base:big");
        let mut overrides = crate::value::DictMap::new();
        overrides.put_str("model", "override:small");
        let merged = merge_repair_options(Some(&base), &overrides, &schema);
        assert_eq!(
            merged.get("model").map(VmValue::display).as_deref(),
            Some("override:small")
        );
    }

    #[test]
    fn build_repair_prompt_includes_schema_and_text() {
        let schema = person_schema();
        let prompt = build_repair_prompt(r#"{"name": 42}"#, &schema);
        assert!(prompt.contains("Target schema"));
        assert!(prompt.contains("Original text"));
        assert!(prompt.contains(r#"{"name": 42}"#));
        assert!(prompt.contains("Reply with valid JSON only"));
    }

    #[tokio::test]
    async fn schema_recover_stage_parsed_for_clean_json() {
        let schema = person_schema();
        let args = vec![
            VmValue::String(std::sync::Arc::from(r#"{"name": "Ada", "age": 36}"#)),
            schema,
        ];
        let env = schema_recover_impl(args, None).await.unwrap();
        let dict = env.as_dict().unwrap();
        assert!(matches!(dict.get("ok"), Some(VmValue::Bool(true))));
        assert_eq!(
            dict.get("stage").map(VmValue::display).as_deref(),
            Some("parsed"),
        );
        assert_eq!(dict.get("attempts").and_then(VmValue::as_int), Some(1));
        assert!(matches!(dict.get("repaired"), Some(VmValue::Bool(false))));
    }

    #[tokio::test]
    async fn schema_recover_stage_extracted_for_fenced_json() {
        let schema = person_schema();
        let args = vec![
            VmValue::String(std::sync::Arc::from(
                "Sure, here you go:\n```json\n{\"name\": \"Ada\", \"age\": 36}\n```\nDone.",
            )),
            schema,
        ];
        let env = schema_recover_impl(args, None).await.unwrap();
        let dict = env.as_dict().unwrap();
        assert!(matches!(dict.get("ok"), Some(VmValue::Bool(true))));
        assert_eq!(
            dict.get("stage").map(VmValue::display).as_deref(),
            Some("extracted"),
        );
    }

    #[tokio::test]
    async fn schema_recover_stage_regex_for_yaml_shape() {
        let schema = person_schema();
        let args = vec![
            VmValue::String(std::sync::Arc::from("name: Ada\nage: 36\nactive: true\n")),
            schema,
        ];
        let env = schema_recover_impl(args, None).await.unwrap();
        let dict = env.as_dict().unwrap();
        assert!(
            matches!(dict.get("ok"), Some(VmValue::Bool(true))),
            "envelope: {env:?}"
        );
        assert_eq!(
            dict.get("stage").map(VmValue::display).as_deref(),
            Some("regex"),
        );
        let data = dict.get("data").unwrap().as_dict().unwrap();
        assert_eq!(data.get("name").unwrap().display(), "Ada");
        assert_eq!(data.get("age").unwrap().as_int(), Some(36));
    }

    #[tokio::test]
    async fn schema_recover_failure_when_repair_disabled_and_unrecoverable() {
        let schema = person_schema();
        let mut opts = crate::value::DictMap::new();
        opts.insert("llm_repair".to_string(), VmValue::Bool(false));
        let args = vec![
            VmValue::String(std::sync::Arc::from("nothing useful here at all")),
            schema,
            VmValue::dict(opts),
        ];
        let env = schema_recover_impl(args, None).await.unwrap();
        let dict = env.as_dict().unwrap();
        assert!(matches!(dict.get("ok"), Some(VmValue::Bool(false))));
        assert_eq!(
            dict.get("stage").map(VmValue::display).as_deref(),
            Some("failed"),
        );
        assert_eq!(
            dict.get("error_category").map(VmValue::display).as_deref(),
            Some("schema_validation"),
        );
    }

    #[tokio::test]
    async fn schema_recover_rejects_non_dict_schema() {
        let args = vec![
            VmValue::String(std::sync::Arc::from("anything")),
            VmValue::String(std::sync::Arc::from("not a schema")),
        ];
        let err = schema_recover_impl(args, None).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("schema must be a dict"), "got: {msg}");
    }

    #[tokio::test]
    async fn schema_recover_rejects_missing_schema_arg() {
        let args = vec![VmValue::String(std::sync::Arc::from("anything"))];
        let err = schema_recover_impl(args, None).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("expected"), "got: {msg}");
    }
}
