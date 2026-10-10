//! Python arm of `move_symbol`: dotted package paths.

use std::collections::{BTreeSet, HashSet};
use std::ops::Range;

use tree_sitter::{Node, Tree};

use super::text::{self, Edit};
use super::{
    Binding, FileModel, ImportRequest, ListStatement, ModuleRef, QualifiedRewrite, Shape, TopItem,
};

pub(crate) struct Workspace {
    files: HashSet<String>,
}

impl Workspace {
    pub(crate) fn new(files: HashSet<String>) -> Self {
        Self { files }
    }

    /// Dotted module name: the enclosing chain of directories that hold an
    /// `__init__.py`, then the file stem (none for `__init__.py`).
    pub(crate) fn module_of(&self, rel: &str) -> String {
        let parts: Vec<&str> = rel.split('/').collect();
        let (file, dirs) = parts.split_last().expect("split yields one part");
        let stem = file
            .strip_suffix(".pyi")
            .or_else(|| file.strip_suffix(".py"))
            .unwrap_or(file);
        let mut depth = dirs.len();
        while depth > 0
            && self
                .files
                .contains(&format!("{}/__init__.py", dirs[..depth].join("/")))
        {
            depth -= 1;
        }
        let mut module: Vec<&str> = dirs[depth..].to_vec();
        if stem != "__init__" {
            module.push(stem);
        }
        module.join(".")
    }

    fn package_of(&self, rel: &str) -> Vec<String> {
        let module: Vec<String> = self.module_of(rel).split('.').map(str::to_string).collect();
        if rel.ends_with("__init__.py") {
            module
        } else {
            module[..module.len().saturating_sub(1)].to_vec()
        }
    }

    fn resolve_from(&self, rel: &str, module_name: Node<'_>, source: &str) -> ModuleRef {
        if module_name.kind() != "relative_import" {
            return ModuleRef::Python(text::text(module_name, source).to_string());
        }
        let raw = text::text(module_name, source);
        let dots = raw.chars().take_while(|c| *c == '.').count();
        let rest = text::slice(raw, dots..).trim();
        let package = self.package_of(rel);
        if dots == 0 || dots - 1 > package.len() {
            return ModuleRef::External(raw.to_string());
        }
        let mut parts = package[..package.len() - (dots - 1)].to_vec();
        if !rest.is_empty() {
            parts.extend(rest.split('.').map(str::to_string));
        }
        ModuleRef::Python(parts.join("."))
    }

    pub(crate) fn model(&mut self, rel: &str, tree: &Tree, source: &str) -> FileModel {
        let mut model = FileModel::default();
        let root = tree.root_node();
        let mut last_import_end = None;
        let mut docstring_end = None;
        for (index, child) in text::members(root).into_iter().enumerate() {
            match child.kind() {
                "import_statement" | "import_from_statement" | "future_import_statement" => {
                    last_import_end = Some(child.end_byte());
                }
                "function_definition" | "class_definition" => {
                    push_item(&mut model, child, child, source);
                }
                "decorated_definition" => {
                    if let Some(definition) = child.child_by_field_name("definition") {
                        push_item(&mut model, child, definition, source);
                    }
                }
                "expression_statement" => {
                    let first = text::members(child).into_iter().next();
                    if index == 0 && first.is_some_and(|n| n.kind() == "string") {
                        docstring_end = Some(child.end_byte());
                    }
                    if let Some(assignment) = first.filter(|n| n.kind() == "assignment") {
                        if let Some(left) = assignment
                            .child_by_field_name("left")
                            .filter(|n| n.kind() == "identifier")
                        {
                            model.items.push(TopItem {
                                name: text::text(left, source).to_string(),
                                range: child.byte_range(),
                                exported: true,
                                type_like: false,
                                movable: false,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        // Imports anywhere in the file; nested ones are rewritten too.
        text::walk(root, |node| match node.kind() {
            "import_statement" | "import_from_statement" => {
                let top_level = node.parent().is_some_and(|p| p.id() == root.id());
                model.import_ranges.push(node.byte_range());
                self.add_import(rel, node, top_level, source, &mut model);
                false
            }
            "future_import_statement" => {
                model.import_ranges.push(node.byte_range());
                false
            }
            kind => !(kind.contains("comment") || kind == "string"),
        });
        model.has_imports = last_import_end.is_some();
        model.insert_at = match last_import_end.or(docstring_end) {
            Some(end) => text::next_line_start(source, end),
            None => 0,
        };
        model
    }

    fn add_import(
        &self,
        rel: &str,
        statement: Node<'_>,
        top_level: bool,
        source: &str,
        model: &mut FileModel,
    ) {
        let mut cursor = statement.walk();
        let names: Vec<Node<'_>> = statement
            .children_by_field_name("name", &mut cursor)
            .collect();
        let ranges: Vec<Range<usize>> = names.iter().map(|n| n.byte_range()).collect();
        let removal = |i: usize| -> (Range<usize>, bool) {
            if names.len() > 1 {
                (text::list_element_removal(&ranges, i), false)
            } else {
                (statement.byte_range(), true)
            }
        };
        if statement.kind() == "import_statement" {
            for (i, name) in names.iter().enumerate() {
                let (dotted, alias) = split_alias(*name, source);
                let local =
                    alias.unwrap_or_else(|| dotted.split('.').next().unwrap_or("").to_string());
                let (removal, sole) = removal(i);
                model.bindings.push(Binding {
                    local,
                    imported: String::new(),
                    target: ModuleRef::Python(dotted),
                    shape: Shape::Namespace,
                    type_only: false,
                    reexport: false,
                    prefix: String::new(),
                    top_level,
                    removal,
                    statement: statement.byte_range(),
                    sole,
                });
            }
            return;
        }
        let Some(module_name) = statement.child_by_field_name("module_name") else {
            return;
        };
        let target = self.resolve_from(rel, module_name, source);
        let mut cursor = statement.walk();
        if statement
            .children(&mut cursor)
            .any(|c| c.kind() == "wildcard_import")
        {
            model.bindings.push(Binding {
                local: String::new(),
                imported: String::new(),
                target,
                shape: Shape::Glob,
                type_only: false,
                reexport: false,
                prefix: String::new(),
                top_level,
                removal: statement.byte_range(),
                statement: statement.byte_range(),
                sole: true,
            });
            return;
        }
        for (i, name) in names.iter().enumerate() {
            let (imported, alias) = split_alias(*name, source);
            let (removal, sole) = removal(i);
            model.bindings.push(Binding {
                local: alias.unwrap_or_else(|| imported.clone()),
                imported,
                target: target.clone(),
                shape: Shape::Named,
                type_only: false,
                reexport: false,
                prefix: String::new(),
                top_level,
                removal,
                statement: statement.byte_range(),
                sole,
            });
        }
        if let (true, Some(last)) = (top_level, names.last()) {
            model.lists.push(ListStatement {
                target,
                type_only: false,
                reexport: false,
                prefix: String::new(),
                append_at: last.end_byte(),
            });
        }
    }
}

fn split_alias(node: Node<'_>, source: &str) -> (String, Option<String>) {
    if node.kind() == "aliased_import" {
        (
            node.child_by_field_name("name")
                .map(|n| text::text(n, source).to_string())
                .unwrap_or_default(),
            node.child_by_field_name("alias")
                .map(|n| text::text(n, source).to_string()),
        )
    } else {
        (text::text(node, source).to_string(), None)
    }
}

fn push_item(model: &mut FileModel, outer: Node<'_>, definition: Node<'_>, source: &str) {
    if let Some(name) = definition.child_by_field_name("name") {
        model.items.push(TopItem {
            name: text::text(name, source).to_string(),
            range: outer.byte_range(),
            exported: true,
            type_like: false,
            movable: true,
        });
    }
}

fn module_text(target: &ModuleRef) -> String {
    match target {
        ModuleRef::Python(dotted) | ModuleRef::External(dotted) => dotted.clone(),
        other => format!("{other:?}"),
    }
}

pub(crate) fn render(req: &ImportRequest) -> String {
    let module = module_text(&req.target);
    match req.shape {
        Shape::Glob => format!("from {module} import *"),
        Shape::Namespace => {
            if module.split('.').next() == Some(req.local.as_str()) {
                format!("import {module}")
            } else {
                format!("import {module} as {}", req.local)
            }
        }
        Shape::Named | Shape::Default => {
            if req.local != req.imported {
                format!("from {module} import {} as {}", req.imported, req.local)
            } else {
                format!("from {module} import {}", req.imported)
            }
        }
    }
}

/// The module a binding loads when its statement runs.
pub(crate) fn binding_module(binding: &Binding) -> Option<ModuleRef> {
    match &binding.target {
        ModuleRef::Python(_) => Some(binding.target.clone()),
        _ => None,
    }
}

/// The module `qualifier` names through this file's imports.
fn qualifier_module(model: &FileModel, qualifier: &str) -> Option<String> {
    let first = qualifier.split('.').next()?;
    let rest = text::slice(qualifier, first.len()..);
    let binding = model
        .bindings
        .iter()
        .find(|b| b.local == first && b.shape != Shape::Glob)?;
    let ModuleRef::Python(target) = &binding.target else {
        return None;
    };
    match binding.shape {
        Shape::Namespace if target.split('.').next() == Some(first) => Some(qualifier.to_string()),
        Shape::Namespace => Some(format!("{target}{rest}")),
        Shape::Named => Some(format!("{target}.{}{rest}", binding.imported)),
        _ => None,
    }
}

/// `m.name` where `m` resolves through an import to the source module.
pub(crate) fn qualified(
    source: &str,
    model: &FileModel,
    node: Node<'_>,
    from: &ModuleRef,
    to: &ModuleRef,
    bare: bool,
) -> Option<QualifiedRewrite> {
    let parent = node.parent()?;
    if parent.kind() != "attribute" || !text::is_field_child(parent, "attribute", node) {
        return None;
    }
    let object = parent.child_by_field_name("object")?;
    let qualifier = text::text(object, source);
    if !qualifier
        .split('.')
        .all(super::super::refactor_core::is_identifier_token)
    {
        return None;
    }
    let ModuleRef::Python(from_module) = from else {
        return None;
    };
    if qualifier_module(model, qualifier).as_deref() != Some(from_module.as_str()) {
        return None;
    }
    if bare {
        return Some(QualifiedRewrite {
            edit: Edit::replace(object.start_byte()..node.start_byte(), ""),
            import: None,
        });
    }
    let ModuleRef::Python(to_module) = to else {
        return None;
    };
    let existing = model.bindings.iter().find_map(|b| {
        let ModuleRef::Python(target) = &b.target else {
            return None;
        };
        match b.shape {
            Shape::Namespace if target == to_module => {
                Some(if target.split('.').next() == Some(b.local.as_str()) {
                    target.clone()
                } else {
                    b.local.clone()
                })
            }
            Shape::Named if &format!("{target}.{}", b.imported) == to_module => {
                Some(b.local.clone())
            }
            _ => None,
        }
    });
    let (replacement, import) = match existing {
        Some(reference) => (reference, None),
        None => (
            to_module.clone(),
            Some(ImportRequest {
                target: to.clone(),
                shape: Shape::Namespace,
                imported: String::new(),
                local: to_module.split('.').next().unwrap_or_default().to_string(),
                type_only: false,
                reexport: false,
                prefix: String::new(),
                anchor: None,
            }),
        ),
    };
    Some(QualifiedRewrite {
        edit: Edit::replace(object.byte_range(), replacement),
        import,
    })
}

/// Names the moved item reads, excluding attribute members, keyword
/// argument names, parameter names, and its own name.
pub(crate) fn free_names(
    item: Node<'_>,
    source: &str,
    own: &str,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut names = BTreeSet::new();
    text::walk(item, |node| {
        let kind = node.kind();
        if kind != "identifier" {
            return true;
        }
        let Some(parent) = node.parent() else {
            return true;
        };
        let declares = match parent.kind() {
            "attribute" => text::is_field_child(parent, "attribute", node),
            "keyword_argument" => text::is_field_child(parent, "name", node),
            "parameters" | "lambda_parameters" => true,
            "typed_parameter" => text::members(parent).first().map(|n| n.id()) == Some(node.id()),
            "default_parameter" | "typed_default_parameter" => {
                text::is_field_child(parent, "name", node)
            }
            "function_definition" | "class_definition" => {
                text::is_field_child(parent, "name", node)
            }
            _ => false,
        };
        if !declares {
            names.insert(text::text(node, source).to_string());
        }
        true
    });
    names.remove(own);
    (names, BTreeSet::new())
}
