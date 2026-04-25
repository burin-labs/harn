//! `ast.symbols` — flat symbol list for a single source file.
//!
//! Thin wrapper around [`super::symbols::extract`] that handles parameter
//! parsing, file IO, and shaping the response into the `VmValue::Dict`
//! the schema expects.

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::args::{build_dict, dict_arg, optional_string, require_string, str_value};

use super::language::Language;
use super::parse::{parse_source, read_source};
use super::symbols::extract;
use super::types::Symbol;

const BUILTIN: &str = "hostlib_ast_symbols";

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();

    let path_str = require_string(BUILTIN, dict, "path")?;
    let language_hint = optional_string(BUILTIN, dict, "language")?;
    let kinds = parse_kinds_filter(dict)?;

    let path = PathBuf::from(&path_str);
    let language = Language::detect(&path, language_hint.as_deref()).ok_or_else(|| {
        HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "language",
            message: format!(
                "could not infer a tree-sitter grammar for `{path_str}` \
                 (extension or `language` field unrecognized)"
            ),
        }
    })?;

    let source = read_source(&path_str, 0)?;
    let tree = parse_source(&source, language)?;
    let mut symbols = extract(&tree, &source, language);
    if let Some(filter) = kinds.as_ref() {
        symbols.retain(|s| filter.contains(s.kind.as_str()));
    }

    let symbols_list: Vec<VmValue> = symbols.iter().map(Symbol::to_vm_value).collect();

    Ok(build_dict([
        ("path", str_value(&path_str)),
        ("language", str_value(language.name())),
        ("symbols", VmValue::List(Rc::new(symbols_list))),
    ]))
}

fn parse_kinds_filter(
    dict: &std::collections::BTreeMap<String, VmValue>,
) -> Result<Option<HashSet<String>>, HostlibError> {
    let Some(raw) = dict.get("kinds") else {
        return Ok(None);
    };
    let VmValue::List(list) = raw else {
        if matches!(raw, VmValue::Nil) {
            return Ok(None);
        }
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "kinds",
            message: format!("expected list of strings, got {}", raw.type_name()),
        });
    };
    let mut out = HashSet::new();
    for item in list.iter() {
        let VmValue::String(s) = item else {
            return Err(HostlibError::InvalidParameter {
                builtin: BUILTIN,
                param: "kinds",
                message: format!("entries must be strings, got {}", item.type_name()),
            });
        };
        out.insert(s.to_string());
    }
    Ok(Some(out))
}
