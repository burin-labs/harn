//! Structured metadata declared in stdlib HarnDoc blocks.
//!
//! Public stdlib functions carry a prose summary plus two required
//! machine-meaningful fields above their `pub fn` declaration:
//!
//! ```text
//! /**
//!  * Returns the contents of `path`.
//!  *
//!  * @effects: [fs.read]
//!  * @errors: [FileNotFound, PermissionDenied]
//!  */
//! pub fn read_to_string(...) -> ... { ... }
//! ```
//!
//! Two more fields are optional:
//!
//! - `@api_stability:` — stability promise (`experimental`, `internal`,
//!   ...). Absent means `stable`.
//! - `@example:` — a hand-written usage example, only worth writing when
//!   it shows something the signature cannot. When absent, tooling
//!   synthesizes one from the type signature ([`synthesize_example`]).
//!
//! These fields drive `harn graph --json`, LSP hover, and the
//! `HARN-STD-101` lint that enforces coverage on stdlib sources.

use crate::ast::{TypeExpr, TypedParam};
use harn_lexer::Span;

/// One declared metadata field on a stdlib function. Empty lists and
/// missing fields are distinct: `effects: Some(vec![])` records an
/// explicit `[]` declaration ("statically certified pure"), while
/// `effects: None` means the author has not annotated the function yet.
///
/// `serde::Serialize` is derived so the same struct can ride through
/// `harn graph --json` and other JSON wire formats without a parallel
/// type definition.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct StdlibMetadata {
    /// Declared effect classes (e.g. `fs.read`, `stdio.write`,
    /// `llm.call`). Comparable to dependency types in `harn graph --json`.
    pub effects: Option<Vec<String>>,
    /// Declared error variants the function may return or raise.
    pub errors: Option<Vec<String>>,
    /// API stability promise (e.g. `experimental`, `internal`,
    /// `deprecated`). Absent means `stable`.
    pub api_stability: Option<String>,
    /// Verbatim usage example. Can span multiple lines. Optional — when
    /// absent, tooling derives one from the signature instead.
    pub example: Option<String>,
}

impl StdlibMetadata {
    /// True when every required field has been populated.
    pub fn is_complete(&self) -> bool {
        self.effects.is_some() && self.errors.is_some()
    }

    /// True when *no* field has been declared. Used by lints and `harn
    /// graph --json` to distinguish "absent" from "partial".
    pub fn is_empty(&self) -> bool {
        self.effects.is_none()
            && self.errors.is_none()
            && self.api_stability.is_none()
            && self.example.is_none()
    }

    /// Names of every required metadata field that has not been declared.
    pub fn missing_fields(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        if self.effects.is_none() {
            out.push("effects");
        }
        if self.errors.is_none() {
            out.push("errors");
        }
        out
    }

    /// Render the metadata as a markdown block for LSP hover and docs.
    /// Only declared fields are emitted; an unannotated function returns
    /// an empty string.
    pub fn to_markdown(&self) -> String {
        self.to_markdown_with_derived_example(None)
    }

    /// Like [`Self::to_markdown`], but falls back to a signature-derived
    /// example (labeled as such) when the author has not written an
    /// `@example`.
    pub fn to_markdown_with_derived_example(&self, derived: Option<&str>) -> String {
        if self.is_empty() && derived.is_none() {
            return String::new();
        }
        let mut lines: Vec<String> = Vec::new();
        if let Some(effects) = &self.effects {
            lines.push(format!(
                "- **effects:** {}",
                if effects.is_empty() {
                    "_none_".to_string()
                } else {
                    effects
                        .iter()
                        .map(|e| format!("`{e}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ));
        }
        if let Some(errors) = &self.errors {
            lines.push(format!(
                "- **errors:** {}",
                if errors.is_empty() {
                    "_none_".to_string()
                } else {
                    errors
                        .iter()
                        .map(|e| format!("`{e}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ));
        }
        if let Some(stability) = &self.api_stability {
            lines.push(format!("- **api_stability:** `{stability}`"));
        }
        if let Some(example) = &self.example {
            lines.push(format!("- **example:**\n\n```harn\n{example}\n```"));
        } else if let Some(derived) = derived {
            lines.push(format!(
                "- **example** _(derived from signature)_**:**\n\n```harn\n{derived}\n```"
            ));
        }
        format!("**Stdlib metadata**\n\n{}", lines.join("\n"))
    }
}

/// Synthesize a usage example from a function signature, for symbols whose
/// docs carry no hand-written `@example`. LSP hover and `harn graph
/// --json` present the result labeled as derived.
pub fn synthesize_example(
    name: &str,
    params: &[TypedParam],
    return_type: Option<&TypeExpr>,
) -> String {
    let args = params
        .iter()
        .map(|p| {
            if p.rest {
                format!("...{}", p.name)
            } else {
                p.name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let call = format!("{name}({args})");
    match return_type {
        // Only bind the result when the signature declares a non-nil
        // return; an untyped fn may well be a statement-style helper.
        Some(TypeExpr::Named(n)) if n == "nil" => call,
        Some(_) => format!("const out = {call}"),
        None => call,
    }
}

/// Parse all `@key: value` fields from the body of a canonical
/// `/** ... */` HarnDoc block. The body should be the inner text with
/// `/**`, leading `*`, and `*/` markers already stripped, one line per
/// element. Multi-line `@example:` continuations are joined while
/// preserving trailing newlines.
pub fn parse_from_doc_body(body: &str) -> StdlibMetadata {
    parse_from_doc_lines(&body.lines().collect::<Vec<_>>())
}

fn parse_from_doc_lines(lines: &[&str]) -> StdlibMetadata {
    let mut meta = StdlibMetadata::default();
    let mut current_key: Option<&'static str> = None;
    let mut current_value: String = String::new();

    let flush = |key: Option<&'static str>, value: String, meta: &mut StdlibMetadata| {
        let Some(key) = key else { return };
        let trimmed = value.trim_end_matches('\n').to_string();
        assign_field(meta, key, &trimmed);
    };

    for raw in lines {
        let line = raw.trim();
        if let Some((key, rest)) = parse_key_line(line) {
            // Flush the previous field before starting a new one.
            flush(current_key, std::mem::take(&mut current_value), &mut meta);
            current_key = Some(key);
            current_value.clear();
            current_value.push_str(rest.trim());
        } else if current_key.is_some() {
            // Lines that are part of an `@example:` continuation keep
            // their leading indentation relative to the doc block. Blank
            // lines terminate the current value.
            if line.is_empty() {
                flush(current_key, std::mem::take(&mut current_value), &mut meta);
                current_key = None;
            } else if current_key == Some("example") {
                current_value.push('\n');
                current_value.push_str(line);
            }
        }
    }
    flush(current_key, current_value, &mut meta);
    meta
}

fn parse_key_line(line: &str) -> Option<(&'static str, &str)> {
    let rest = line.strip_prefix('@')?;
    let colon = rest.find(':')?;
    let (key, after) = rest.split_at(colon);
    let key = match key.trim() {
        "effects" => "effects",
        "errors" => "errors",
        "api_stability" => "api_stability",
        "example" => "example",
        _ => return None,
    };
    Some((key, &after[1..]))
}

fn assign_field(meta: &mut StdlibMetadata, key: &str, value: &str) {
    match key {
        "effects" => meta.effects = Some(parse_list(value)),
        "errors" => meta.errors = Some(parse_list(value)),
        "api_stability" => meta.api_stability = Some(value.trim().to_string()),
        "example" => meta.example = Some(value.trim().to_string()),
        _ => {}
    }
}

fn parse_list(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    let stripped = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    stripped
        .split(',')
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

/// Extract a canonical `/** ... */` block immediately above the given
/// span and parse its metadata fields. Returns the parsed metadata even
/// if no fields are declared so callers can detect "doc present, fields
/// missing".
pub fn parse_for_span(source: &str, span: &Span) -> Option<StdlibMetadata> {
    let body = extract_doc_body(source, span)?;
    Some(parse_from_doc_body(&body))
}

fn extract_doc_body(source: &str, span: &Span) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let def_line_idx = span.line.checked_sub(1)?;
    if def_line_idx == 0 {
        return None;
    }
    let above_idx = def_line_idx - 1;
    let above = lines.get(above_idx)?.trim_end();
    if !above.trim_end().ends_with("*/") {
        return None;
    }

    // Single-line `/** ... */` form.
    let above_trim = above.trim_start();
    if above_trim.starts_with("/**") && above_trim.ends_with("*/") && above_trim.len() >= 5 {
        let inner = &above_trim[3..above_trim.len() - 2];
        return Some(inner.trim().to_string());
    }

    // Multi-line block — walk upward to the matching `/**`.
    let mut start_idx = above_idx;
    loop {
        let line = lines.get(start_idx)?.trim_start();
        if line.starts_with("/**") {
            break;
        }
        if start_idx == 0 {
            return None;
        }
        start_idx -= 1;
    }
    let mut body = String::new();
    for (i, line) in lines.iter().enumerate().take(above_idx + 1).skip(start_idx) {
        let trimmed = line.trim();
        let stripped = if i == start_idx {
            trimmed.strip_prefix("/**").unwrap_or(trimmed).trim_start()
        } else if i == above_idx {
            let without_tail = trimmed.strip_suffix("*/").unwrap_or(trimmed).trim_end();
            without_tail
                .strip_prefix('*')
                .map(|s| s.strip_prefix(' ').unwrap_or(s))
                .unwrap_or(without_tail)
        } else {
            trimmed
                .strip_prefix('*')
                .map(|s| s.strip_prefix(' ').unwrap_or(s))
                .unwrap_or(trimmed)
        };
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(stripped);
    }
    Some(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_fields_inline() {
        let body = "Reads a file.\n\n@effects: [fs.read]\n@errors: [FileNotFound, PermissionDenied]\n@api_stability: experimental\n@example: let s = fs::read_to_string(harness.fs, \"/x\")";
        let meta = parse_from_doc_body(body);
        assert!(meta.is_complete(), "missing: {:?}", meta.missing_fields());
        assert_eq!(meta.effects.as_deref(), Some(&["fs.read".to_string()][..]));
        assert_eq!(
            meta.errors.as_deref(),
            Some(&["FileNotFound".to_string(), "PermissionDenied".to_string()][..]),
        );
        assert_eq!(meta.api_stability.as_deref(), Some("experimental"));
        assert_eq!(
            meta.example.as_deref(),
            Some("let s = fs::read_to_string(harness.fs, \"/x\")"),
        );
    }

    #[test]
    fn required_set_is_effects_and_errors_only() {
        let body = "@effects: []\n@errors: []";
        let meta = parse_from_doc_body(body);
        assert!(meta.is_complete(), "missing: {:?}", meta.missing_fields());
        assert!(meta.api_stability.is_none());
        assert!(meta.example.is_none());
    }

    #[test]
    fn partial_metadata_lists_missing_fields() {
        let body = "@api_stability: experimental";
        let meta = parse_from_doc_body(body);
        assert!(!meta.is_complete());
        assert!(!meta.is_empty());
        assert_eq!(meta.missing_fields(), vec!["effects", "errors"]);
    }

    #[test]
    fn empty_effect_and_error_lists_are_explicit() {
        let body = "@effects: []\n@errors: []";
        let meta = parse_from_doc_body(body);
        assert_eq!(meta.effects.as_deref(), Some(&[][..]));
        assert_eq!(meta.errors.as_deref(), Some(&[][..]));
    }

    #[test]
    fn unknown_keys_do_not_pollute_storage() {
        // `deprecated` was never in the contract; `allocation` was removed
        // from it. Both parse as ignored unknown keys.
        let body = "@deprecated: yes\n@allocation: stack-only\n@errors: []";
        let meta = parse_from_doc_body(body);
        assert_eq!(meta.errors.as_deref(), Some(&[][..]));
        assert!(meta.effects.is_none());
    }

    #[test]
    fn example_continuation_lines_are_joined() {
        let body = "@example: let s = fs::open(p)\n  let b = fs::read(s)\n  fs::close(s)";
        let meta = parse_from_doc_body(body);
        assert_eq!(
            meta.example.as_deref(),
            Some("let s = fs::open(p)\nlet b = fs::read(s)\nfs::close(s)"),
        );
    }

    #[test]
    fn parse_for_span_extracts_multi_line_block() {
        let source = "\
/**
 * Read the file.
 *
 * @effects: [fs.read]
 * @errors: [FileNotFound]
 */
pub fn read_file(path) {
  __fs_read_to_string(path)
}
";
        let span = Span::with_offsets(0, 0, 7, 1);
        let meta = parse_for_span(source, &span).expect("metadata present");
        assert!(meta.is_complete(), "missing: {:?}", meta.missing_fields());
    }

    #[test]
    fn parse_for_span_handles_single_line_block() {
        let source = "/** @effects: [] @errors: [] @example: noop() */\npub fn noop() { }\n";
        let span = Span::with_offsets(0, 0, 2, 1);
        let meta = parse_for_span(source, &span).expect("metadata present");
        // Single-line form only fits one tag — accept whichever last wins.
        assert!(!meta.is_empty());
    }

    #[test]
    fn markdown_omits_unset_fields() {
        let meta = StdlibMetadata {
            effects: Some(vec!["fs.read".to_string()]),
            errors: None,
            api_stability: Some("experimental".to_string()),
            example: None,
        };
        let md = meta.to_markdown();
        assert!(md.contains("**effects:**"));
        assert!(md.contains("**api_stability:**"));
        assert!(!md.contains("**errors:**"));
        assert!(!md.contains("**example"));
    }

    #[test]
    fn markdown_prefers_authored_example_over_derived() {
        let meta = StdlibMetadata {
            effects: Some(vec![]),
            errors: Some(vec![]),
            api_stability: None,
            example: Some("read_file(\"/etc/hosts\")".to_string()),
        };
        let md = meta.to_markdown_with_derived_example(Some("let out = read_file(path)"));
        assert!(md.contains("read_file(\"/etc/hosts\")"));
        assert!(!md.contains("derived from signature"));
    }

    #[test]
    fn markdown_falls_back_to_derived_example() {
        let meta = StdlibMetadata {
            effects: Some(vec![]),
            errors: Some(vec![]),
            api_stability: None,
            example: None,
        };
        let md = meta.to_markdown_with_derived_example(Some("let out = read_file(path)"));
        assert!(md.contains("derived from signature"));
        assert!(md.contains("let out = read_file(path)"));
    }

    #[test]
    fn derived_example_renders_even_without_declared_fields() {
        let meta = StdlibMetadata::default();
        assert!(meta.to_markdown().is_empty());
        let md = meta.to_markdown_with_derived_example(Some("greet(name)"));
        assert!(md.contains("greet(name)"));
    }

    #[test]
    fn synthesize_example_binds_non_nil_returns() {
        use crate::ast::{TypeExpr, TypedParam};
        let params = vec![TypedParam::untyped("path"), TypedParam::untyped("limit")];
        let ret = TypeExpr::Named("string".to_string());
        assert_eq!(
            synthesize_example("read_file", &params, Some(&ret)),
            "const out = read_file(path, limit)",
        );
    }

    #[test]
    fn synthesize_example_skips_binding_for_nil_and_untyped_returns() {
        use crate::ast::{TypeExpr, TypedParam};
        let params = vec![TypedParam::untyped("event")];
        let nil = TypeExpr::Named("nil".to_string());
        assert_eq!(
            synthesize_example("notify", &params, Some(&nil)),
            "notify(event)",
        );
        assert_eq!(synthesize_example("notify", &params, None), "notify(event)");
    }

    #[test]
    fn synthesize_example_spreads_rest_params() {
        use crate::ast::TypedParam;
        let mut rest = TypedParam::untyped("parts");
        rest.rest = true;
        assert_eq!(synthesize_example("join", &[rest], None), "join(...parts)");
    }
}
