//! `tools/get_file_outline` — structural outline (functions, types, and
//! their nested members) for a single source file.
//!
//! The language comes from the `harn-languages` registry and the items from
//! the tree-sitter fold behind `ast.outline`, so the two tools agree for
//! every language with a grammar. A file whose language is unknown, has no
//! grammar in this build, or has no symbol projection (data and markup
//! formats) gets `status: "unsupported"` with a typed `reason` instead of a
//! guessed outline.

use std::path::PathBuf;
use std::sync::Arc;

use harn_languages::Language;
use harn_vm::process_sandbox::FsAccess;
use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::args::{build_dict, dict_arg, optional_int, require_string, str_value};
use crate::tools::permissions::enforce_path_scope;

const BUILTIN: &str = "hostlib_tools_get_file_outline";

/// Why a file has no structural outline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unsupported {
    /// The registry does not recognize the path.
    UnknownLanguage,
    /// The language has no tree-sitter grammar in this build.
    NoGrammar,
    /// The grammar parses but exposes no symbols (data, markup, style).
    NoSymbolProjection,
}

impl Unsupported {
    fn as_str(self) -> &'static str {
        match self {
            Unsupported::UnknownLanguage => "unknown_language",
            Unsupported::NoGrammar => "no_grammar",
            Unsupported::NoSymbolProjection => "no_symbol_projection",
        }
    }
}

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();

    let path_str = require_string(BUILTIN, dict, "path")?;
    let max_depth = optional_int(BUILTIN, dict, "max_depth", 0)?;
    let session_id = crate::tools::args::optional_string(BUILTIN, dict, "session_id")?;
    if max_depth < 0 {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "max_depth",
            message: "must be >= 0".to_string(),
        });
    }

    let path = PathBuf::from(&path_str);
    enforce_path_scope(BUILTIN, &path, FsAccess::Read)?;
    let language = Language::from_path(&path);

    let content = match crate::fs::read_to_string(&path, session_id.as_deref()) {
        Some(result) => result,
        None => std::fs::read_to_string(&path),
    }
    .map_err(|err| HostlibError::Backend {
        builtin: BUILTIN,
        message: format!("read `{path_str}`: {err}"),
    })?;

    let (items, unsupported) = match outline(&content, language, max_depth as usize)? {
        Ok(items) => (items, None),
        Err(reason) => (Vec::new(), Some(reason)),
    };

    Ok(build_dict([
        ("path", str_value(&path_str)),
        (
            "language",
            language.map_or(VmValue::Nil, |language| str_value(language.name())),
        ),
        (
            "status",
            str_value(if unsupported.is_some() {
                "unsupported"
            } else {
                "ok"
            }),
        ),
        (
            "reason",
            unsupported.map_or(VmValue::Nil, |reason| str_value(reason.as_str())),
        ),
        ("items", VmValue::List(Arc::new(items))),
    ]))
}

#[cfg(feature = "ast")]
fn outline(
    content: &str,
    language: Option<Language>,
    max_depth: usize,
) -> Result<Result<Vec<VmValue>, Unsupported>, HostlibError> {
    use crate::ast::Language as Grammar;

    let language = match language {
        Some(language) => language,
        None => return Ok(Err(Unsupported::UnknownLanguage)),
    };
    let grammar = match Grammar::from_registry(language).filter(|g| g.ts_language().is_some()) {
        Some(grammar) => grammar,
        None => return Ok(Err(Unsupported::NoGrammar)),
    };
    if !grammar.supports_symbol_extraction() {
        return Ok(Err(Unsupported::NoSymbolProjection));
    }
    let items = crate::ast::api::outline_from_source(content, grammar, max_depth)?;
    Ok(Ok(items
        .iter()
        .map(crate::ast::OutlineItem::to_vm_value)
        .collect()))
}

#[cfg(not(feature = "ast"))]
fn outline(
    _content: &str,
    language: Option<Language>,
    _max_depth: usize,
) -> Result<Result<Vec<VmValue>, Unsupported>, HostlibError> {
    Ok(Err(match language {
        None => Unsupported::UnknownLanguage,
        Some(language) if language.class() != harn_languages::LanguageClass::Code => {
            Unsupported::NoSymbolProjection
        }
        Some(_) => Unsupported::NoGrammar,
    }))
}
