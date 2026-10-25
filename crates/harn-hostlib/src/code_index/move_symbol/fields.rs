//! Rust struct fields a move would hide from the modules that read them.

use std::ops::Range;
use std::path::Path;

use tree_sitter::{Node, Tree};

use crate::ast::Language;
use crate::code_index::refactor_core::read_source;
use crate::code_index::state::IndexState;
use crate::error::HostlibError;

use super::model::{Lang, ModuleRef};
use super::response::{refuse, Outcome};
use super::widen::{self, Widener};
use super::{parse, rust, BUILTIN};

/// What the check needs to know about the move.
pub(super) struct Context<'a> {
    pub state: &'a IndexState,
    pub session: Option<&'a str>,
    pub name: &'a str,
    pub source_path: &'a str,
    pub source: &'a str,
    pub tree: &'a Tree,
    pub item_node: Node<'a>,
    pub moved: &'a Range<usize>,
    pub source_module: &'a ModuleRef,
    pub dest_module: &'a ModuleRef,
}

/// Code that reads a restricted field of the moved struct and would not see
/// it from the destination. Only modules that saw the field before are
/// scanned; elsewhere a same-named field belongs to another type. Returns the
/// refusal, or `None` when nothing is hidden or the widener accepted the fixes.
pub(super) fn settle(
    ctx: &Context<'_>,
    lang: &mut Lang,
    widener: &mut Widener,
) -> Result<Option<Outcome>, HostlibError> {
    let Context {
        state,
        session,
        name,
        source_path,
        source,
        tree,
        item_node,
        moved,
        ..
    } = *ctx;
    let (
        Lang::Rust(ws),
        ModuleRef::Rust {
            crate_dir: source_crate,
            path: from,
        },
        ModuleRef::Rust { path: to, .. },
    ) = (lang, ctx.source_module, ctx.dest_module)
    else {
        return Ok(None);
    };
    let mut regions = Vec::new();
    let hidden_fields = rust::hidden_fields(item_node, source);
    for hidden in &hidden_fields {
        let (field, path) = (hidden.name.clone(), &hidden.path);
        let before = rust::visible_from(from, path).unwrap_or_default();
        match rust::visible_from(to, path) {
            Some(after) if to.starts_with(&after) => regions.push((field, before, after)),
            _ => {
                return Ok(Some(refuse(
                    "visibility_required",
                    format!("field `{field}` of `{name}` is visible in `{path}`, which does not contain the destination; widen it first"),
                )));
            }
        }
    }
    let fields: Vec<String> = regions.iter().map(|(field, ..)| field.clone()).collect();
    let mut paths: Vec<&str> = state
        .files
        .values()
        .map(|file| file.relative_path.as_str())
        .filter(|path| Language::detect(Path::new(path), None) == Some(Language::Rust))
        .collect();
    paths.sort_unstable();
    let mut readers = Vec::new();
    for path in paths {
        let Some(ModuleRef::Rust {
            crate_dir,
            path: module,
        }) = ws.module_of(path)
        else {
            continue;
        };
        let breaks = |field: &str| {
            regions.iter().any(|(f, before, after)| {
                f == field && module.starts_with(before) && !module.starts_with(after)
            })
        };
        if crate_dir != *source_crate || !fields.iter().any(|field| breaks(field)) {
            continue;
        }
        if path == source_path {
            readers.extend(rust::field_reads(
                path,
                source,
                tree,
                name,
                &fields,
                &breaks,
                Some(moved),
            ));
            continue;
        }
        let text = read_source(BUILTIN, &state.root, path, session)?;
        let mentions = text.contains(name)
            || fields.iter().any(|field| {
                breaks(field)
                    && if field.parse::<usize>().is_ok() {
                        text.contains(&format!(".{field}"))
                    } else {
                        text.contains(field.as_str())
                    }
            });
        if mentions {
            let parsed = parse(&text, Language::Rust)?;
            readers.extend(rust::field_reads(
                path, &text, &parsed, name, &fields, &breaks, None,
            ));
        }
    }
    if readers.is_empty() {
        return Ok(None);
    }
    let fixes = widen::reader_fixes(
        ws,
        &readers,
        &hidden_fields,
        &regions,
        (source_crate.as_str(), source_path, source),
        ctx.dest_module,
    );
    let sites = readers.iter().map(|(site, _)| site.clone()).collect();
    Ok(widener.settle(
        format!("code outside the destination reads fields of `{name}` it would no longer see; make them `pub(crate)` first"),
        sites,
        fixes,
    ))
}
