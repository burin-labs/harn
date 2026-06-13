//! `ast.capabilities` — report the per-language AST-precise edit matrix.
//!
//! This is the runtime face of the B.7 onboarding contract. The agent
//! loop calls it to decide which edit primitive a file qualifies for
//! (and, when none, what to fall back to) without hard-coding a language
//! list in prompts.
//!
//! ## Wire shape
//!
//! Request (see `schemas/ast/capabilities.request.json`):
//!
//! - `language`: optional canonical name / alias to filter to a single
//!   language. Omit to enumerate every shipped grammar.
//!
//! Response carries `fallback_suggestion` (the text-edit degradation path)
//! and `languages`: a list of `{ language, extension, apply_node,
//! insert_at_anchor, rename_symbol, symbols }` rows. When a `language`
//! filter resolves to no grammar the response is the tagged
//! `unsupported_language` union member instead, carrying the same
//! `fallback_suggestion`.

use std::sync::Arc;

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::args::{build_dict, dict_arg, optional_string, str_value};

use super::language::{EditCapabilities, Language, TEXT_PATCH_FALLBACK};

const BUILTIN: &str = "hostlib_ast_capabilities";

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();
    let language_hint = optional_string(BUILTIN, dict, "language")?;

    if let Some(hint) = language_hint.as_deref().filter(|h| !h.is_empty()) {
        let Some(language) = Language::from_name(hint) else {
            return Ok(unsupported_language_response(hint));
        };
        return Ok(matrix_response(&[language]));
    }

    Ok(matrix_response(Language::all()))
}

fn matrix_response(languages: &[Language]) -> VmValue {
    let rows: Vec<VmValue> = languages.iter().map(|l| row(*l)).collect();
    build_dict([
        ("result", str_value("ok")),
        ("fallback_suggestion", str_value(TEXT_PATCH_FALLBACK)),
        ("languages", VmValue::List(Arc::new(rows))),
    ])
}

fn row(language: Language) -> VmValue {
    let EditCapabilities {
        apply_node,
        insert_at_anchor,
        rename_symbol,
        symbols,
    } = language.edit_capabilities();
    build_dict([
        ("language", str_value(language.name())),
        ("extension", str_value(language.primary_extension())),
        ("apply_node", VmValue::Bool(apply_node)),
        ("insert_at_anchor", VmValue::Bool(insert_at_anchor)),
        ("rename_symbol", VmValue::Bool(rename_symbol)),
        ("symbols", VmValue::Bool(symbols)),
    ])
}

fn unsupported_language_response(hint: &str) -> VmValue {
    build_dict([
        ("result", str_value("unsupported_language")),
        (
            "details",
            str_value(format!("no tree-sitter grammar registered for `{hint}`")),
        ),
        ("fallback_suggestion", str_value(TEXT_PATCH_FALLBACK)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoke(dict: VmValue) -> VmValue {
        run(&[dict]).expect("capabilities runs")
    }

    fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
        let mut map: harn_vm::value::DictMap = Default::default();
        for (k, v) in pairs {
            map.insert((*k).to_string(), v.clone());
        }
        VmValue::dict(map)
    }

    fn field<'a>(value: &'a VmValue, key: &str) -> &'a VmValue {
        match value {
            VmValue::Dict(d) => d.get(key).expect("missing field"),
            _ => panic!("expected dict"),
        }
    }

    fn rows(value: &VmValue) -> Vec<VmValue> {
        match field(value, "languages") {
            VmValue::List(l) => l.as_ref().clone(),
            other => panic!("expected list, got {other:?}"),
        }
    }

    fn s(value: &VmValue) -> String {
        match value {
            VmValue::String(s) => s.to_string(),
            other => panic!("expected string, got {other:?}"),
        }
    }

    fn b(value: &VmValue) -> bool {
        match value {
            VmValue::Bool(b) => *b,
            other => panic!("expected bool, got {other:?}"),
        }
    }

    #[test]
    fn enumerates_every_language() {
        let result = invoke(dict(&[]));
        assert_eq!(s(field(&result, "result")), "ok");
        assert_eq!(rows(&result).len(), Language::all().len());
    }

    #[test]
    fn single_language_filter_reports_capabilities() {
        let result = invoke(dict(&[("language", str_value("rust"))]));
        let rows = rows(&result);
        assert_eq!(rows.len(), 1);
        let rust = &rows[0];
        assert_eq!(s(field(rust, "language")), "rust");
        assert!(b(field(rust, "apply_node")));
        assert!(b(field(rust, "rename_symbol")));
        assert!(b(field(rust, "symbols")));
    }

    #[test]
    fn data_format_reports_edit_only() {
        let result = invoke(dict(&[("language", str_value("json"))]));
        let rows = rows(&result);
        let json = &rows[0];
        assert!(b(field(json, "apply_node")));
        assert!(b(field(json, "insert_at_anchor")));
        assert!(!b(field(json, "rename_symbol")));
        assert!(!b(field(json, "symbols")));
    }

    #[test]
    fn unknown_language_degrades_with_fallback() {
        let result = invoke(dict(&[("language", str_value("brainfuck"))]));
        assert_eq!(s(field(&result, "result")), "unsupported_language");
        assert!(!s(field(&result, "fallback_suggestion")).is_empty());
    }
}
