//! TypeScript/JavaScript arm of `move_symbol`: ES imports with relative
//! specifiers.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;

use tree_sitter::{Node, Tree};

use super::text::{self, Edit};
use super::{
    Binding, FileModel, ImportRequest, ListStatement, ModuleRef, QualifiedRewrite, Shape, TopItem,
};

const EXTENSIONS: &[&str] = &[".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"];

const DECLARATION_KINDS: &[&str] = &[
    "function_declaration",
    "generator_function_declaration",
    "class_declaration",
    "abstract_class_declaration",
    "interface_declaration",
    "type_alias_declaration",
    "enum_declaration",
    "lexical_declaration",
    "variable_declaration",
];

/// How a file writes its imports, so added ones match.
#[derive(Clone, Debug)]
struct Style {
    quote: char,
    semicolon: bool,
    /// Extension relative specifiers carry: `""`, `".js"` (nodenext), or
    /// `".ts"` (allowImportingTsExtensions).
    extension: &'static str,
}

pub(crate) struct Workspace {
    files: HashSet<String>,
    styles: HashMap<String, Style>,
}

impl Workspace {
    pub(crate) fn new(files: HashSet<String>) -> Self {
        Self {
            files,
            styles: HashMap::new(),
        }
    }

    fn resolve(&self, from: &str, spec: &str) -> ModuleRef {
        if !spec.starts_with('.') {
            return ModuleRef::External(spec.to_string());
        }
        let joined = normalize(&format!("{}/{spec}", parent_dir(from)));
        let mut candidates = Vec::new();
        if EXTENSIONS.iter().any(|ext| joined.ends_with(ext)) {
            candidates.push(joined.clone());
            let base = strip_extension(&joined);
            for ext in EXTENSIONS {
                candidates.push(format!("{base}{ext}"));
            }
        } else {
            for ext in EXTENSIONS {
                candidates.push(format!("{joined}{ext}"));
            }
            for ext in EXTENSIONS {
                candidates.push(format!("{joined}/index{ext}"));
            }
        }
        candidates
            .into_iter()
            .find(|path| self.files.contains(path))
            .map(ModuleRef::Script)
            .unwrap_or_else(|| ModuleRef::External(spec.to_string()))
    }

    pub(crate) fn model(&mut self, rel: &str, tree: &Tree, source: &str) -> FileModel {
        let mut model = FileModel::default();
        let root = tree.root_node();
        let mut last_import_end = None;
        let mut local_exports: Vec<String> = Vec::new();
        let mut quote = None;
        let mut semicolon = None;
        let mut extension = "";
        for child in text::members(root) {
            match child.kind() {
                "import_statement" => {
                    last_import_end = Some(child.end_byte());
                    model.import_ranges.push(child.byte_range());
                    semicolon.get_or_insert(text::text(child, source).trim_end().ends_with(';'));
                    if let Some(spec) = child.child_by_field_name("source") {
                        let raw = text::text(spec, source);
                        quote.get_or_insert(raw.chars().next().unwrap_or('"'));
                        let inner = raw.trim_matches(['"', '\'']);
                        if inner.starts_with('.') {
                            if inner.ends_with(".js") {
                                extension = ".js";
                            } else if inner.ends_with(".ts") {
                                extension = ".ts";
                            }
                        }
                    }
                    self.add_import(rel, child, source, &mut model);
                }
                "export_statement" => {
                    if child.child_by_field_name("source").is_some() {
                        model.import_ranges.push(child.byte_range());
                        self.add_reexport(rel, child, source, &mut model);
                    } else if let Some(declaration) = child.child_by_field_name("declaration") {
                        let is_default = has_token(child, "default");
                        push_items(
                            &mut model,
                            declaration,
                            child.byte_range(),
                            true,
                            !is_default,
                            source,
                        );
                    } else {
                        for clause in text::members(child) {
                            if clause.kind() == "export_clause" {
                                for spec in text::members(clause) {
                                    if let Some(name) = spec.child_by_field_name("name") {
                                        local_exports.push(text::text(name, source).to_string());
                                    }
                                }
                            }
                        }
                    }
                }
                kind if DECLARATION_KINDS.contains(&kind) => {
                    push_items(&mut model, child, child.byte_range(), false, true, source);
                }
                _ => {}
            }
        }
        for item in &mut model.items {
            if local_exports.contains(&item.name) {
                // Exported through a separate `export { name }` clause, which
                // a move would leave dangling.
                item.exported = true;
                item.movable = false;
            }
        }
        model.has_imports = last_import_end.is_some();
        model.insert_at = match last_import_end {
            Some(end) => text::next_line_start(source, end),
            None => text::members(root)
                .first()
                .filter(|n| n.kind() == "hash_bang_line")
                .map(|n| text::next_line_start(source, n.end_byte()))
                .unwrap_or(0),
        };
        let semicolon =
            semicolon.unwrap_or_else(|| source.lines().any(|l| l.trim_end().ends_with(';')));
        self.styles.insert(
            rel.to_string(),
            Style {
                quote: quote.unwrap_or('"'),
                semicolon,
                extension,
            },
        );
        model
    }

    fn add_import(&self, rel: &str, statement: Node<'_>, source: &str, model: &mut FileModel) {
        let Some(spec) = statement.child_by_field_name("source") else {
            return;
        };
        let target = self.resolve(rel, unquote(text::text(spec, source)));
        let type_only = has_token(statement, "type");
        let Some(clause) = text::members(statement)
            .into_iter()
            .find(|n| n.kind() == "import_clause")
        else {
            return;
        };
        let parts = text::members(clause);
        for (index, part) in parts.iter().enumerate() {
            match part.kind() {
                "identifier" => model.bindings.push(Binding {
                    local: text::text(*part, source).to_string(),
                    imported: "default".into(),
                    target: target.clone(),
                    shape: Shape::Default,
                    type_only,
                    reexport: false,
                    prefix: String::new(),
                    top_level: true,
                    removal: clause_part_removal(&parts, index, statement),
                    statement: statement.byte_range(),
                    sole: parts.len() == 1,
                }),
                "namespace_import" => {
                    if let Some(local) = text::members(*part).first() {
                        model.bindings.push(Binding {
                            local: text::text(*local, source).to_string(),
                            imported: String::new(),
                            target: target.clone(),
                            shape: Shape::Namespace,
                            type_only,
                            reexport: false,
                            prefix: String::new(),
                            top_level: true,
                            removal: clause_part_removal(&parts, index, statement),
                            statement: statement.byte_range(),
                            sole: parts.len() == 1,
                        });
                    }
                }
                "named_imports" => {
                    let specs = text::members(*part);
                    let ranges: Vec<Range<usize>> = specs.iter().map(|s| s.byte_range()).collect();
                    for (i, spec) in specs.iter().enumerate() {
                        let Some(name) = spec.child_by_field_name("name") else {
                            continue;
                        };
                        let imported = unquote(text::text(name, source)).to_string();
                        let local = spec
                            .child_by_field_name("alias")
                            .map(|a| text::text(a, source).to_string())
                            .unwrap_or_else(|| imported.clone());
                        let (removal, sole) = if specs.len() > 1 {
                            (text::list_element_removal(&ranges, i), false)
                        } else {
                            let removal = clause_part_removal(&parts, index, statement);
                            let sole = removal == statement.byte_range();
                            (removal, sole)
                        };
                        model.bindings.push(Binding {
                            local,
                            imported,
                            target: target.clone(),
                            shape: Shape::Named,
                            type_only: type_only || has_token(*spec, "type"),
                            reexport: false,
                            prefix: String::new(),
                            top_level: true,
                            removal,
                            statement: statement.byte_range(),
                            sole,
                        });
                    }
                    if let Some(last) = specs.last() {
                        model.lists.push(ListStatement {
                            target: target.clone(),
                            type_only,
                            reexport: false,
                            prefix: String::new(),
                            append_at: last.end_byte(),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    fn add_reexport(&self, rel: &str, statement: Node<'_>, source: &str, model: &mut FileModel) {
        let Some(spec) = statement.child_by_field_name("source") else {
            return;
        };
        let target = self.resolve(rel, unquote(text::text(spec, source)));
        let type_only = has_token(statement, "type");
        let clause = text::members(statement)
            .into_iter()
            .find(|n| n.kind() == "export_clause");
        let Some(clause) = clause else {
            if has_token(statement, "*")
                && !text::members(statement)
                    .iter()
                    .any(|n| n.kind() == "namespace_export")
            {
                model.bindings.push(Binding {
                    local: String::new(),
                    imported: String::new(),
                    target,
                    shape: Shape::Glob,
                    type_only,
                    reexport: true,
                    prefix: String::new(),
                    top_level: true,
                    removal: statement.byte_range(),
                    statement: statement.byte_range(),
                    sole: true,
                });
            }
            return;
        };
        let specs = text::members(clause);
        let ranges: Vec<Range<usize>> = specs.iter().map(|s| s.byte_range()).collect();
        for (i, spec) in specs.iter().enumerate() {
            let Some(name) = spec.child_by_field_name("name") else {
                continue;
            };
            let imported = unquote(text::text(name, source)).to_string();
            let local = spec
                .child_by_field_name("alias")
                .map(|a| text::text(a, source).to_string())
                .unwrap_or_else(|| imported.clone());
            let sole = specs.len() == 1;
            model.bindings.push(Binding {
                local,
                imported,
                target: target.clone(),
                shape: Shape::Named,
                type_only: type_only || has_token(*spec, "type"),
                reexport: true,
                prefix: String::new(),
                top_level: true,
                removal: if sole {
                    statement.byte_range()
                } else {
                    text::list_element_removal(&ranges, i)
                },
                statement: statement.byte_range(),
                sole,
            });
        }
        if let Some(last) = specs.last() {
            model.lists.push(ListStatement {
                target,
                type_only,
                reexport: true,
                prefix: String::new(),
                append_at: last.end_byte(),
            });
        }
    }

    pub(crate) fn render(&self, rel: &str, source: &str, req: &ImportRequest) -> String {
        let style = self.styles.get(rel).cloned().unwrap_or(Style {
            quote: '"',
            semicolon: source.lines().any(|l| l.trim_end().ends_with(';')),
            extension: "",
        });
        let spec = match &req.target {
            ModuleRef::Script(path) => relative_spec(rel, path, style.extension),
            ModuleRef::External(spec) => spec.clone(),
            other => format!("{other:?}"),
        };
        let q = style.quote;
        let semi = if style.semicolon { ";" } else { "" };
        let alias = if req.local != req.imported {
            format!(" as {}", req.local)
        } else {
            String::new()
        };
        let kind = if req.type_only { "type " } else { "" };
        let keyword = if req.reexport { "export" } else { "import" };
        match req.shape {
            Shape::Named => format!(
                "{keyword} {kind}{{ {}{alias} }} from {q}{spec}{q}{semi}",
                req.imported
            ),
            Shape::Default => format!("import {kind}{} from {q}{spec}{q}{semi}", req.local),
            Shape::Namespace => format!("import {kind}* as {} from {q}{spec}{q}{semi}", req.local),
            Shape::Glob => format!("export * from {q}{spec}{q}{semi}"),
        }
    }
}

/// `ns.name` (or the type `ns.Name`) where `ns` is a namespace import of
/// `from`.
pub(crate) fn qualified(
    source: &str,
    model: &FileModel,
    node: Node<'_>,
    from: &ModuleRef,
    to: &ModuleRef,
    bare: bool,
) -> Option<QualifiedRewrite> {
    let parent = node.parent()?;
    let object = match parent.kind() {
        "member_expression" if text::is_field_child(parent, "property", node) => {
            parent.child_by_field_name("object")?
        }
        "nested_type_identifier" if text::is_field_child(parent, "name", node) => {
            parent.child_by_field_name("module")?
        }
        _ => return None,
    };
    if object.kind() != "identifier" {
        return None;
    }
    let qualifier = text::text(object, source);
    model
        .bindings
        .iter()
        .find(|b| b.local == qualifier && b.shape == Shape::Namespace && &b.target == from)?;
    let name = text::text(node, source);
    let strip = Edit::replace(object.start_byte()..node.start_byte(), "");
    if bare {
        return Some(QualifiedRewrite {
            edit: strip,
            import: None,
        });
    }
    // A binding of the moved name from either module is retargeted to the
    // destination, so it does not compete with the bare name.
    let named_from_to = |b: &Binding| {
        b.shape == Shape::Named && (&b.target == to || &b.target == from) && b.imported == name
    };
    let taken = model.item(name).is_some()
        || model
            .bindings
            .iter()
            .any(|b| b.local == name && !named_from_to(b));
    if !taken {
        return Some(QualifiedRewrite {
            edit: strip,
            import: Some(ImportRequest::named(to.clone(), name)),
        });
    }
    // The bare name is bound to something else here: qualify through a
    // namespace import of the destination instead.
    if let Some(existing) = model
        .bindings
        .iter()
        .find(|b| b.shape == Shape::Namespace && &b.target == to)
    {
        return Some(QualifiedRewrite {
            edit: Edit::replace(object.byte_range(), existing.local.clone()),
            import: None,
        });
    }
    let stem = match to {
        ModuleRef::Script(path) => {
            strip_extension(path.rsplit('/').next().unwrap_or(path)).to_string()
        }
        _ => "moved".to_string(),
    };
    let mut alias: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    while model.item(&alias).is_some() || model.bindings.iter().any(|b| b.local == alias) {
        alias.push('_');
    }
    Some(QualifiedRewrite {
        edit: Edit::replace(object.byte_range(), alias.clone()),
        import: Some(ImportRequest {
            target: to.clone(),
            shape: Shape::Namespace,
            imported: String::new(),
            local: alias,
            type_only: false,
            reexport: false,
            prefix: String::new(),
            anchor: None,
        }),
    })
}

/// Identifiers the moved item uses, minus property names and its own name.
pub(crate) fn free_names(
    item: Node<'_>,
    source: &str,
    own: &str,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut names = BTreeSet::new();
    text::walk(item, |node| {
        let kind = node.kind();
        if matches!(
            kind,
            "identifier" | "type_identifier" | "shorthand_property_identifier"
        ) {
            let is_tail = node.parent().is_some_and(|p| {
                p.kind() == "nested_type_identifier" && text::is_field_child(p, "name", node)
            });
            if !is_tail {
                names.insert(text::text(node, source).to_string());
            }
        }
        true
    });
    names.remove(own);
    (names, BTreeSet::new())
}

fn push_items(
    model: &mut FileModel,
    declaration: Node<'_>,
    range: Range<usize>,
    exported: bool,
    movable: bool,
    source: &str,
) {
    let type_like = matches!(
        declaration.kind(),
        "interface_declaration" | "type_alias_declaration"
    );
    if matches!(
        declaration.kind(),
        "lexical_declaration" | "variable_declaration"
    ) {
        let declarators: Vec<Node<'_>> = text::members(declaration)
            .into_iter()
            .filter(|n| n.kind() == "variable_declarator")
            .collect();
        let single = declarators.len() == 1;
        for declarator in declarators {
            if let Some(name) = declarator
                .child_by_field_name("name")
                .filter(|n| n.kind() == "identifier")
            {
                model.items.push(TopItem {
                    name: text::text(name, source).to_string(),
                    range: range.clone(),
                    exported,
                    type_like: false,
                    movable: movable && single,
                });
            }
        }
        return;
    }
    if let Some(name) = declaration.child_by_field_name("name") {
        model.items.push(TopItem {
            name: text::text(name, source).to_string(),
            range,
            exported,
            type_like,
            movable,
        });
    }
}

/// Removal of one part of an import clause (`Foo`, `* as ns`, `{...}`).
fn clause_part_removal(parts: &[Node<'_>], index: usize, statement: Node<'_>) -> Range<usize> {
    if parts.len() == 1 {
        return statement.byte_range();
    }
    if index > 0 {
        parts[index - 1].end_byte()..parts[index].end_byte()
    } else {
        parts[0].start_byte()..parts[1].start_byte()
    }
}

fn has_token(node: Node<'_>, token: &str) -> bool {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .any(|c| !c.is_named() && c.kind() == token);
    found
}

fn unquote(raw: &str) -> &str {
    raw.trim_matches(['"', '\'', '`'])
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
}

fn strip_extension(path: &str) -> &str {
    if let Some(stripped) = path.strip_suffix(".d.ts") {
        return stripped;
    }
    for ext in EXTENSIONS {
        if let Some(stripped) = path.strip_suffix(ext) {
            return stripped;
        }
    }
    path
}

fn normalize(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out.join("/")
}

/// The specifier `from` uses to import the file `to`.
fn relative_spec(from: &str, to: &str, extension: &str) -> String {
    let from_dir: Vec<&str> = parent_dir(from)
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let to_parts: Vec<&str> = to.split('/').collect();
    let common = from_dir
        .iter()
        .zip(to_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts: Vec<String> = vec!["..".to_string(); from_dir.len() - common];
    parts.extend(to_parts[common..].iter().map(|s| s.to_string()));
    let joined = parts.join("/");
    let base = strip_extension(&joined).to_string();
    let suffix = match extension {
        ".js" if to.ends_with(".mts") => ".mjs",
        ".js" if to.ends_with(".cts") => ".cjs",
        ".js" => ".js",
        ".ts" => text::slice(to, strip_extension(to).len()..),
        _ => "",
    };
    let spec = format!("{base}{suffix}");
    if spec.starts_with("..") {
        spec
    } else {
        format!("./{spec}")
    }
}
