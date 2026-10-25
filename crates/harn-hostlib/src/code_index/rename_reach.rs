//! Leftover mentions a renamed declaration cannot reach, by language rule.
//!
//! After a language server's rename, every same-name mention the edit left
//! must resolve to something else, or the rename is refused. A mention the
//! server cannot resolve normally sits in code it did not analyze, which may
//! well reference the declaration. Two declaration shapes rule some
//! mentions out without the server:
//!
//! - A TypeScript or JavaScript binding declared inside a function, or at the
//!   top level of an ES module that never exports it, is visible only in its
//!   own file and only as a bare identifier. A mention in another file, or a
//!   member name such as `args.length`, is something else.
//! - A Rust associated function of an inherent impl is reached only through
//!   its type, and cannot be imported. A macro-body path `$crate::a::name`
//!   whose `a` is an unconditional module declaration names a module item.

use std::path::Path;

use tree_sitter::{Node, Tree};

use crate::ast::{api as ast_api, Language};
use crate::error::HostlibError;

use super::move_symbol::{Family, Lang};
use super::refactor_core::read_source;
use super::rename_rust_residue::qualifier;
use super::state::IndexState;
use super::symbol_graph::NodeKind;

/// Which mentions the declaration provably cannot reach.
pub(super) enum Reach {
    /// No rule applies; every leftover goes to the server.
    Anywhere,
    /// Only bare identifiers in the declaration's own file can reach it.
    FileLocal { path: String },
    /// A Rust inherent associated function.
    InherentAssociated,
}

impl Reach {
    /// Classify the declaration whose name starts at byte `offset`.
    pub(super) fn of(
        path: &str,
        language: Language,
        source: &str,
        offset: usize,
        name: &str,
        kind: NodeKind,
    ) -> Self {
        let Ok(tree) = ast_api::parse_tree(source, language) else {
            return Self::Anywhere;
        };
        let Some(identifier) = tree
            .root_node()
            .named_descendant_for_byte_range(offset, offset + name.len())
        else {
            return Self::Anywhere;
        };
        let local = match language {
            // A declaration file exports every top-level declaration unless it
            // has an export statement of its own, so leave it to the server.
            Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx
                if is_declaration_file(path) =>
            {
                false
            }
            Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx => {
                script_file_local(&tree, source, identifier, name)
            }
            Language::Rust if kind == NodeKind::Function && inherent_associated(identifier) => {
                return Self::InherentAssociated
            }
            _ => false,
        };
        if local {
            Self::FileLocal {
                path: path.to_string(),
            }
        } else {
            Self::Anywhere
        }
    }
}

/// Answers, per leftover mention, why the declaration cannot reach it.
pub(super) struct Exclusions<'a> {
    reach: Reach,
    state: &'a IndexState,
    builtin: &'static str,
    session_id: Option<&'a str>,
    /// Built on first use: only the Rust rule needs the crate layout.
    lang: Option<Lang>,
}

impl<'a> Exclusions<'a> {
    pub(super) fn new(
        reach: Reach,
        state: &'a IndexState,
        builtin: &'static str,
        session_id: Option<&'a str>,
    ) -> Self {
        Self {
            reach,
            state,
            builtin,
            session_id,
            lang: None,
        }
    }

    /// Why the mention at byte `offset` of `path` cannot be the declaration
    /// or a reference to it; `None` when only the server can tell.
    pub(super) fn reason(
        &mut self,
        path: &str,
        source: &str,
        tree: &Tree,
        offset: usize,
    ) -> Result<Option<&'static str>, HostlibError> {
        match &self.reach {
            Reach::Anywhere => Ok(None),
            Reach::FileLocal { path: own } => {
                if !maybe_same_file(&self.state.root, path, own) {
                    return Ok(Some(
                        "the declaration is local to its own file, so another file cannot reference it",
                    ));
                }
                let member = tree
                    .root_node()
                    .descendant_for_byte_range(offset, offset)
                    .is_some_and(|node| node.kind() == "property_identifier");
                Ok(member.then_some("a member name cannot reference a file-local binding"))
            }
            Reach::InherentAssociated => {
                let in_macro = std::iter::successors(
                    tree.root_node().descendant_for_byte_range(offset, offset),
                    Node::parent,
                )
                .any(|node| node.kind() == "macro_definition");
                if !in_macro {
                    return Ok(None);
                }
                let segments = qualifier(source, offset);
                let Some((head, modules)) = segments.split_first() else {
                    return Ok(None);
                };
                if head != "$crate" || modules.is_empty() {
                    return Ok(None);
                }
                let state = self.state;
                let lang = self
                    .lang
                    .get_or_insert_with(|| Lang::new(Family::Rust, state, &[]));
                let Some((crate_dir, roots)) = lang.crate_roots(path) else {
                    return Ok(None);
                };
                // Either root may be the crate a macro in this file belongs to.
                for root in &roots {
                    if !self.declares_modules(&crate_dir, root, modules)? {
                        return Ok(None);
                    }
                }
                Ok(Some(
                    "a module path names a module item, never an inherent associated function",
                ))
            }
        }
    }

    /// Whether `root` declares the module chain `modules` with plain,
    /// unconditional `mod` items, so every build resolves the path to it.
    /// Neither the `mod` item nor the module's own file or body may carry a
    /// `cfg` or `path` attribute.
    fn declares_modules(
        &self,
        crate_dir: &str,
        root: &str,
        modules: &[String],
    ) -> Result<bool, HostlibError> {
        let mut parent = root.to_string();
        for (depth, module) in modules.iter().enumerate() {
            let Some((source, tree)) = self.parse_rust(&parent)? else {
                return Ok(false);
            };
            let Some(item) = unconditional_mod(tree.root_node(), &source, module) else {
                return Ok(false);
            };
            let last = depth + 1 == modules.len();
            if let Some(body) = item.child_by_field_name("body") {
                // An inline module's children are not followed further.
                return Ok(last && !inner_conditional(body, &source));
            }
            let dir = Path::new(crate_dir)
                .join("src")
                .join(modules[..=depth].join("/"));
            let candidates: Vec<String> = [dir.with_extension("rs"), dir.join("mod.rs")]
                .into_iter()
                .filter(|file| self.state.root.join(file).is_file())
                .map(|file| file.to_string_lossy().replace('\\', "/"))
                .collect();
            let [file] = candidates.as_slice() else {
                return Ok(false);
            };
            let Some((module_source, module_tree)) = self.parse_rust(file)? else {
                return Ok(false);
            };
            if inner_conditional(module_tree.root_node(), &module_source) {
                return Ok(false);
            }
            if last {
                return Ok(true);
            }
            parent = file.clone();
        }
        Ok(false)
    }

    /// The source and syntax tree of the Rust file `rel`, if it exists and parses.
    fn parse_rust(&self, rel: &str) -> Result<Option<(String, Tree)>, HostlibError> {
        if !self.state.root.join(rel).is_file() {
            return Ok(None);
        }
        let source = read_source(self.builtin, &self.state.root, rel, self.session_id)?;
        Ok(ast_api::parse_tree(&source, Language::Rust)
            .ok()
            .map(|tree| (source, tree)))
    }
}

/// A TypeScript declaration file. TypeScript's `getDeclarationFileExtension`
/// takes `.d.ts`, `.d.mts`, `.d.cts`, and any `.ts` file whose base name
/// contains `.d.` (`styles.d.css.ts`); this also takes `.mts` and `.cts`
/// and ignores case, which only widens the back-off.
fn is_declaration_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let base = lower.rsplit('/').next().unwrap_or(&lower);
    [".ts", ".mts", ".cts"]
        .iter()
        .any(|extension| base.ends_with(extension))
        && base.contains(".d.")
}

/// Whether index-relative paths `a` and `b` may name one file. Index paths
/// and paths mapped back from a server's edit can spell one file
/// differently (case, a symlinked directory, a hard link), and treating the
/// declaration's own file as another would skip a real reference, so any
/// doubt answers yes.
fn maybe_same_file(root: &Path, a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    let (Ok(left), Ok(right)) = (
        std::fs::metadata(root.join(a)),
        std::fs::metadata(root.join(b)),
    ) else {
        return true;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        left.dev() == right.dev() && left.ino() == right.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (left, right);
        match (root.join(a).canonicalize(), root.join(b).canonicalize()) {
            (Ok(left), Ok(right)) => left == right,
            _ => true,
        }
    }
}

/// Whether a module's own file or inline body opens with an inner `cfg` or
/// `path` attribute (`#![cfg(...)]`), which makes the module conditional.
fn inner_conditional(container: Node<'_>, source: &str) -> bool {
    let mut cursor = container.walk();
    for node in container.named_children(&mut cursor) {
        match node.kind() {
            "inner_attribute_item" => {
                let text = node.utf8_text(source.as_bytes()).unwrap_or_default();
                if text.contains("cfg") || text.contains("path") {
                    return true;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => break,
        }
    }
    false
}

/// A top-level `mod name` item with no `cfg` or `path` attribute.
fn unconditional_mod<'t>(root: Node<'t>, source: &str, name: &str) -> Option<Node<'t>> {
    let mut cursor = root.walk();
    let found = root.named_children(&mut cursor).find(|item| {
        item.kind() == "mod_item"
            && item
                .child_by_field_name("name")
                .is_some_and(|ident| ident.utf8_text(source.as_bytes()) == Ok(name))
    })?;
    let mut previous = found.prev_named_sibling();
    while let Some(node) = previous {
        match node.kind() {
            "attribute_item" => {
                let text = node.utf8_text(source.as_bytes()).unwrap_or_default();
                if text.contains("cfg") || text.contains("path") {
                    return None;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        previous = node.prev_named_sibling();
    }
    Some(found)
}

/// A function item directly in an `impl Type { ... }` with no trait.
fn inherent_associated(identifier: Node<'_>) -> bool {
    let Some(function) = identifier
        .parent()
        .filter(|node| node.kind() == "function_item")
    else {
        return false;
    };
    function
        .parent()
        .filter(|node| node.kind() == "declaration_list")
        .and_then(|list| list.parent())
        .is_some_and(|owner| {
            owner.kind() == "impl_item" && owner.child_by_field_name("trait").is_none()
        })
}

const BINDING_DECLARATIONS: &[&str] = &[
    "variable_declarator",
    "function_declaration",
    "generator_function_declaration",
    "class_declaration",
    "abstract_class_declaration",
    "type_alias_declaration",
    "interface_declaration",
    "enum_declaration",
];

const FUNCTION_SCOPES: &[&str] = &[
    "function_declaration",
    "generator_function_declaration",
    "function_expression",
    "function",
    "generator_function",
    "arrow_function",
    "method_definition",
];

/// Whether the binding named by `identifier` is visible only in its own file:
/// declared inside a function, or at the top level of an ES module that does
/// not export it. A shorthand property (`{ name }`) ties a property to the
/// binding, so its presence anywhere in the file defeats the rule.
fn script_file_local(tree: &Tree, source: &str, identifier: Node<'_>, name: &str) -> bool {
    // An export swallowed by an error or missing node is invisible below.
    if tree.root_node().has_error() {
        return false;
    }
    let Some(declaration) = identifier.parent().filter(|node| {
        BINDING_DECLARATIONS.contains(&node.kind())
            && node.child_by_field_name("name") == Some(identifier)
    }) else {
        return false;
    };
    let root = tree.root_node();
    let text = |node: Node<'_>| node.utf8_text(source.as_bytes()).unwrap_or_default();
    let mentions = |root: Node<'_>, kinds: &[&str]| {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if kinds.contains(&node.kind()) && text(node) == name {
                return true;
            }
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
        false
    };
    if mentions(root, &["shorthand_property_identifier"]) {
        return false;
    }
    let mut node = declaration.parent();
    while let Some(current) = node {
        match current.kind() {
            kind if FUNCTION_SCOPES.contains(&kind) => return true,
            "export_statement" | "internal_module" | "module" | "ambient_declaration" => {
                return false
            }
            "program" => {
                let mut cursor = current.walk();
                let children: Vec<Node<'_>> = current.named_children(&mut cursor).collect();
                let is_module = children
                    .iter()
                    .any(|child| matches!(child.kind(), "import_statement" | "export_statement"));
                // `export const other = name` exports `other` and only uses
                // `name`; an export list, `export default name` or
                // `export = name` exports the binding itself.
                let exported = children.iter().any(|child| {
                    child.kind() == "export_statement"
                        && child.child_by_field_name("declaration").is_none()
                        && mentions(
                            *child,
                            &[
                                "identifier",
                                "type_identifier",
                                "property_identifier",
                                "shorthand_property_identifier",
                            ],
                        )
                });
                return is_module && !exported;
            }
            _ => node = current.parent(),
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reach(path: &str, language: Language, source: &str, name: &str, kind: NodeKind) -> bool {
        let offset = source.find(&format!(" {name}")).unwrap() + 1;
        matches!(
            Reach::of(path, language, source, offset, name, kind),
            Reach::FileLocal { .. } | Reach::InherentAssociated
        )
    }

    #[test]
    fn script_bindings_are_file_local_only_when_no_export_or_shorthand_reaches_them() {
        let ts = Language::TypeScript;
        let f = NodeKind::Function;
        let module = "import { a } from './a';\n";
        for (body, name, local) in [
            ("const length = 1;\n", "length", true),
            ("export function f() {\n  const map = 1;\n}\n", "map", true),
            (
                "it('x', () => {\n  type Input = number;\n});\n",
                "Input",
                true,
            ),
            ("export const length = 1;\n", "length", false),
            ("const length = 1;\nexport { length };\n", "length", false),
            (
                "const length = 1;\nexport default length;\n",
                "length",
                false,
            ),
            ("const length = 1;\nexport = length;\n", "length", false),
            (
                "const length = 1;\nexport const twice = length * 2;\n",
                "length",
                true,
            ),
            (
                "const length = 1;\nconst o = { length };\n",
                "length",
                false,
            ),
            (
                "declare global {\n  const length: number;\n}\n",
                "length",
                false,
            ),
            ("namespace N {\n  const length = 1;\n}\n", "length", false),
            (
                "declare module \"x\" {\n  const length: number;\n}\n",
                "length",
                false,
            ),
            // A syntax error can hide an export from the top-level check.
            (
                "const length = 1;\nfunction f( {\nexport { length };\n",
                "length",
                false,
            ),
            ("const length = 1;\nexport { length\n", "length", false),
        ] {
            let source = format!("{module}{body}");
            assert_eq!(reach("a.ts", ts, &source, name, f), local, "{source}");
        }
        // A script's top-level bindings are global.
        assert!(!reach("a.ts", ts, "const length = 1;\n", "length", f));
        // A declaration file exports its top-level declarations implicitly.
        let declarations = "import type { Base } from \"./base\";\ninterface Options extends Base {}\nexport interface Config { options: Options }\n";
        assert!(reach(
            "types.ts",
            ts,
            declarations,
            "Options",
            NodeKind::Type
        ));
        for path in [
            "types.d.ts",
            "types.d.mts",
            "types.d.cts",
            "Types.D.TS",
            "src/styles.d.css.ts",
            "src/page.d.html.mts",
        ] {
            assert!(
                !reach(path, ts, declarations, "Options", NodeKind::Type),
                "{path}"
            );
        }
    }

    #[test]
    fn file_identity_doubts_answer_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.ts"), "").unwrap();
        std::fs::write(root.join("src/b.ts"), "").unwrap();
        assert!(!maybe_same_file(root, "src/a.ts", "src/b.ts"));
        assert!(maybe_same_file(root, "src/a.ts", "SRC/A.ts"));
        assert!(maybe_same_file(root, "src/a.ts", "src/missing.ts"));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("src"), root.join("linked")).unwrap();
            std::fs::hard_link(root.join("src/a.ts"), root.join("src/hard.ts")).unwrap();
            assert!(maybe_same_file(root, "src/a.ts", "linked/a.ts"));
            assert!(maybe_same_file(root, "src/a.ts", "src/hard.ts"));
        }
    }

    #[test]
    fn only_inherent_rust_functions_are_unreachable_by_module_paths() {
        let rust = Language::Rust;
        let f = NodeKind::Function;
        assert!(reach(
            "a.rs",
            rust,
            "impl Value {\n    fn capture() {}\n}\n",
            "capture",
            f
        ));
        assert!(!reach(
            "a.rs",
            rust,
            "impl Visit for Value {\n    fn capture() {}\n}\n",
            "capture",
            f
        ));
        assert!(!reach(
            "a.rs",
            rust,
            "trait Visit {\n    fn capture();\n}\n",
            "capture",
            f
        ));
        assert!(!reach("a.rs", rust, "fn capture() {}\n", "capture", f));
    }
}
