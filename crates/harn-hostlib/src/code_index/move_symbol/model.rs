//! The language-neutral model every `move_symbol` arm fills in: module
//! identity, import bindings, top-level items, and import requests.

use std::collections::{BTreeSet, HashSet};
use std::ops::Range;

use tree_sitter::{Node, Tree};

use crate::ast::Language;
use crate::code_index::state::IndexState;

use super::text::Edit;
use super::{python, rust, script};

/// The language families `move_symbol` rewrites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Family {
    Rust,
    Script,
    Python,
}

impl Family {
    pub(crate) fn of(language: Language) -> Option<Self> {
        match language {
            Language::Rust => Some(Self::Rust),
            Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx => {
                Some(Self::Script)
            }
            Language::Python => Some(Self::Python),
            _ => None,
        }
    }

    /// Newlines between top-level items: one blank line, two for Python.
    pub(crate) fn item_gap(self) -> usize {
        match self {
            Self::Python => 3,
            _ => 2,
        }
    }
}

/// What an import or qualifier resolves to.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum ModuleRef {
    /// A library module: the crate directory and the path from its root.
    Rust {
        crate_dir: String,
        path: Vec<String>,
    },
    /// A TS/JS module, by workspace-relative file path.
    Script(String),
    /// A Python module, by dotted name.
    Python(String),
    /// A module outside the workspace, spelled as the import wrote it.
    External(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Shape {
    /// Binds `imported` from the module (Rust leaves, TS named, Python from-import).
    Named,
    /// TS default import.
    Default,
    /// Binds the module itself (TS `* as`, Python `import a.b [as x]`).
    Namespace,
    /// Binds everything (`*`), or re-exports everything.
    Glob,
}

/// One name an import statement binds.
#[derive(Clone, Debug)]
pub(crate) struct Binding {
    /// Local name; empty for globs.
    pub local: String,
    /// Name in the target for `Named`; empty otherwise.
    pub imported: String,
    pub target: ModuleRef,
    pub shape: Shape,
    pub type_only: bool,
    /// Rust `pub use` or TS `export ... from`.
    pub reexport: bool,
    /// Rust visibility text including its trailing space (`pub `).
    pub prefix: String,
    pub top_level: bool,
    /// Removes this binding alone (the whole statement when `sole`).
    pub removal: Range<usize>,
    pub statement: Range<usize>,
    pub sole: bool,
}

/// An import statement with a named list another binding can join.
#[derive(Clone, Debug)]
pub(crate) struct ListStatement {
    pub target: ModuleRef,
    pub type_only: bool,
    pub reexport: bool,
    pub prefix: String,
    pub append_at: usize,
}

/// A top-level declaration.
#[derive(Clone, Debug)]
pub(crate) struct TopItem {
    pub name: String,
    /// The declaration including an `export` wrapper or decorators.
    pub range: Range<usize>,
    pub exported: bool,
    /// TS interfaces and type aliases import with `type`.
    pub type_like: bool,
    /// Whether the item can be moved (functions, types, constants).
    pub movable: bool,
}

/// What one file declares and imports, in byte ranges.
#[derive(Clone, Debug, Default)]
pub(crate) struct FileModel {
    pub bindings: Vec<Binding>,
    pub lists: Vec<ListStatement>,
    pub items: Vec<TopItem>,
    /// Where a new import block goes: a line start.
    pub insert_at: usize,
    pub has_imports: bool,
    /// Byte ranges of import statements, for "is this use an import" tests.
    pub import_ranges: Vec<Range<usize>>,
}

impl FileModel {
    pub(crate) fn item(&self, name: &str) -> Option<&TopItem> {
        self.items.iter().find(|item| item.name == name)
    }

    pub(crate) fn binding_of(&self, local: &str) -> Option<&Binding> {
        self.bindings
            .iter()
            .find(|b| b.local == local && b.shape != Shape::Glob)
    }

    pub(crate) fn in_import(&self, at: usize) -> bool {
        self.import_ranges.iter().any(|r| r.contains(&at))
    }
}

/// An import a file needs after the move.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ImportRequest {
    pub target: ModuleRef,
    pub shape: Shape,
    pub imported: String,
    pub local: String,
    pub type_only: bool,
    pub reexport: bool,
    pub prefix: String,
    /// The statement this request replaces in place, when the binding it
    /// retargets was the statement's only one.
    pub anchor: Option<(usize, usize)>,
}

impl ImportRequest {
    pub(crate) fn named(target: ModuleRef, name: &str) -> Self {
        Self {
            target,
            shape: Shape::Named,
            imported: name.to_string(),
            local: name.to_string(),
            type_only: false,
            reexport: false,
            prefix: String::new(),
            anchor: None,
        }
    }

    pub(crate) fn satisfied_by(&self, binding: &Binding) -> bool {
        binding.target == self.target
            && binding.shape == self.shape
            && binding.imported == self.imported
            && binding.local == self.local
            && binding.reexport == self.reexport
    }
}

/// How a qualified use is rewritten.
pub(crate) struct QualifiedRewrite {
    pub edit: Edit,
    pub import: Option<ImportRequest>,
}

/// Language arm: module identity, import model, rendering and resolution.
pub(crate) enum Lang {
    Rust(rust::Workspace),
    Script(script::Workspace),
    Python(python::Workspace),
}

impl Lang {
    pub(crate) fn new(family: Family, state: &IndexState, extra: &[&str]) -> Self {
        let mut files: HashSet<String> = state
            .files
            .values()
            .map(|f| f.relative_path.clone())
            .collect();
        files.extend(extra.iter().map(|p| p.to_string()));
        match family {
            Family::Rust => Self::Rust(rust::Workspace::new(state.root.clone())),
            Family::Script => Self::Script(script::Workspace::new(files)),
            Family::Python => Self::Python(python::Workspace::new(files)),
        }
    }

    pub(crate) fn module_of(&mut self, path: &str) -> Option<ModuleRef> {
        match self {
            Self::Rust(ws) => ws.module_of(path),
            Self::Script(_) => Some(ModuleRef::Script(path.to_string())),
            Self::Python(ws) => Some(ModuleRef::Python(ws.module_of(path))),
        }
    }

    pub(crate) fn model(&mut self, path: &str, tree: &Tree, source: &str) -> FileModel {
        match self {
            Self::Rust(ws) => ws.model(path, tree, source),
            Self::Script(ws) => ws.model(path, tree, source),
            Self::Python(ws) => ws.model(path, tree, source),
        }
    }

    pub(crate) fn render(
        &mut self,
        path: &str,
        source: &str,
        req: &ImportRequest,
    ) -> Result<String, String> {
        match self {
            Self::Rust(ws) => ws.render(path, req),
            Self::Script(ws) => Ok(ws.render(path, source, req)),
            Self::Python(_) => Ok(python::render(req)),
        }
    }

    pub(crate) fn merge_text(&self, list: &ListStatement, req: &ImportRequest) -> String {
        let alias = if req.local != req.imported {
            format!(" as {}", req.local)
        } else {
            String::new()
        };
        let kind = if req.type_only && !list.type_only && matches!(self, Self::Script(_)) {
            "type "
        } else {
            ""
        };
        format!(", {kind}{}{alias}", req.imported)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn qualified(
        &mut self,
        path: &str,
        source: &str,
        model: &FileModel,
        node: Node<'_>,
        from: &ModuleRef,
        to: &ModuleRef,
        bare: bool,
    ) -> Result<Option<QualifiedRewrite>, String> {
        match self {
            Self::Rust(ws) => ws.qualified(path, source, model, node, from, to, bare),
            Self::Script(_) => Ok(script::qualified(source, model, node, from, to, bare)),
            Self::Python(_) => Ok(python::qualified(source, model, node, from, to, bare)),
        }
    }

    /// Names the moved item uses, and member names it reads (Rust fields
    /// and methods), from its syntax tree.
    pub(crate) fn free_names(
        &self,
        item: Node<'_>,
        source: &str,
        name: &str,
    ) -> (BTreeSet<String>, BTreeSet<String>) {
        match self {
            Self::Rust(_) => rust::free_names(item, source, name),
            Self::Script(_) => script::free_names(item, source, name),
            Self::Python(_) => python::free_names(item, source, name),
        }
    }

    pub(crate) fn insert_separator(&self) -> usize {
        match self {
            Self::Python(_) => 2,
            _ => 1,
        }
    }
}
