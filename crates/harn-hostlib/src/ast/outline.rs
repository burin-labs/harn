//! `ast.outline` — nested outline for a single source file.
//!
//! Built on top of [`super::symbols::extract`]: take the flat symbol
//! list and fold it into a tree using each container's row range. The
//! fold is language-agnostic, which keeps the per-language extractor
//! count down — every grammar's "container vs. leaf" semantics flow
//! through [`crate::ast::types::SymbolKind::is_container`].

use std::path::PathBuf;
use std::rc::Rc;

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_int, optional_string, require_string, str_value,
};

use super::language::Language;
use super::parse::{parse_source, read_source};
use super::symbols::extract;
use super::types::{OutlineItem, Symbol};

const BUILTIN: &str = "hostlib_ast_outline";

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();

    let path_str = require_string(BUILTIN, dict, "path")?;
    let language_hint = optional_string(BUILTIN, dict, "language")?;
    let max_depth = optional_int(BUILTIN, dict, "max_depth", 0)?;
    if max_depth < 0 {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "max_depth",
            message: "must be >= 0".into(),
        });
    }

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
    let symbols = extract(&tree, &source, language);
    let mut items = build_outline(symbols);
    if max_depth > 0 {
        truncate_depth(&mut items, max_depth as usize, 1);
    }

    let items_list: Vec<VmValue> = items.iter().map(OutlineItem::to_vm_value).collect();

    Ok(build_dict([
        ("path", str_value(&path_str)),
        ("language", str_value(language.name())),
        ("items", VmValue::List(Rc::new(items_list))),
    ]))
}

/// Fold a flat, document-ordered symbol list into a tree using each
/// container's row range to decide ownership: a non-container symbol
/// belongs to the innermost container whose range still encloses it.
pub(super) fn build_outline(symbols: Vec<Symbol>) -> Vec<OutlineItem> {
    let mut roots: Vec<OutlineItem> = Vec::new();
    let mut stack: Vec<OutlineItem> = Vec::new();

    for sym in symbols {
        // Pop containers whose end row is strictly before this symbol's
        // start row — they can't contain it.
        while let Some(top) = stack.last() {
            if top.end_row < sym.start_row {
                let popped = stack.pop().unwrap();
                attach(&mut stack, &mut roots, popped);
            } else {
                break;
            }
        }

        let item = OutlineItem {
            name: sym.name,
            kind: sym.kind,
            signature: sym.signature,
            start_row: sym.start_row,
            end_row: sym.end_row,
            children: Vec::new(),
        };

        if sym.kind.is_container() {
            stack.push(item);
        } else {
            attach(&mut stack, &mut roots, item);
        }
    }

    while let Some(popped) = stack.pop() {
        attach(&mut stack, &mut roots, popped);
    }

    roots
}

fn attach(stack: &mut [OutlineItem], roots: &mut Vec<OutlineItem>, item: OutlineItem) {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(item);
    } else {
        roots.push(item);
    }
}

fn truncate_depth(items: &mut [OutlineItem], max_depth: usize, current: usize) {
    if current >= max_depth {
        for item in items.iter_mut() {
            item.children.clear();
        }
        return;
    }
    for item in items.iter_mut() {
        truncate_depth(&mut item.children, max_depth, current + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::types::SymbolKind;

    fn sym(name: &str, kind: SymbolKind, start: u32, end: u32) -> Symbol {
        Symbol {
            name: name.into(),
            kind,
            container: None,
            signature: name.into(),
            start_row: start,
            start_col: 0,
            end_row: end,
            end_col: 0,
        }
    }

    #[test]
    fn flat_symbols_become_roots() {
        let symbols = vec![
            sym("a", SymbolKind::Function, 1, 3),
            sym("b", SymbolKind::Function, 5, 7),
        ];
        let items = build_outline(symbols);
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i.children.is_empty()));
    }

    #[test]
    fn methods_attach_to_enclosing_class() {
        let symbols = vec![
            sym("Foo", SymbolKind::Class, 1, 10),
            sym("bar", SymbolKind::Method, 2, 4),
            sym("baz", SymbolKind::Method, 5, 7),
        ];
        let items = build_outline(symbols);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Foo");
        assert_eq!(items[0].children.len(), 2);
        assert_eq!(items[0].children[0].name, "bar");
        assert_eq!(items[0].children[1].name, "baz");
    }

    #[test]
    fn sibling_classes_dont_swallow_each_other() {
        let symbols = vec![
            sym("Foo", SymbolKind::Class, 1, 5),
            sym("foo_m", SymbolKind::Method, 2, 4),
            sym("Bar", SymbolKind::Class, 6, 10),
            sym("bar_m", SymbolKind::Method, 7, 9),
        ];
        let items = build_outline(symbols);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "Foo");
        assert_eq!(items[1].name, "Bar");
        assert_eq!(items[0].children.len(), 1);
        assert_eq!(items[1].children.len(), 1);
    }

    #[test]
    fn nested_classes_chain_correctly() {
        let symbols = vec![
            sym("Outer", SymbolKind::Class, 1, 20),
            sym("Inner", SymbolKind::Class, 2, 10),
            sym("inner_m", SymbolKind::Method, 3, 5),
            sym("outer_m", SymbolKind::Method, 12, 15),
        ];
        let items = build_outline(symbols);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Outer");
        assert_eq!(items[0].children.len(), 2);
        assert_eq!(items[0].children[0].name, "Inner");
        assert_eq!(items[0].children[0].children.len(), 1);
        assert_eq!(items[0].children[0].children[0].name, "inner_m");
        assert_eq!(items[0].children[1].name, "outer_m");
    }

    #[test]
    fn truncate_depth_drops_grandchildren() {
        let mut items = vec![OutlineItem {
            name: "Outer".into(),
            kind: SymbolKind::Class,
            signature: "class Outer".into(),
            start_row: 0,
            end_row: 10,
            children: vec![OutlineItem {
                name: "Inner".into(),
                kind: SymbolKind::Class,
                signature: "class Inner".into(),
                start_row: 1,
                end_row: 5,
                children: vec![OutlineItem {
                    name: "deep".into(),
                    kind: SymbolKind::Method,
                    signature: "fn deep".into(),
                    start_row: 2,
                    end_row: 3,
                    children: vec![],
                }],
            }],
        }];
        truncate_depth(&mut items, 2, 1);
        assert_eq!(items[0].children.len(), 1);
        assert!(items[0].children[0].children.is_empty());
    }
}
