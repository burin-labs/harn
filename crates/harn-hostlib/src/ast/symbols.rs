//! Per-language tree-sitter symbol extractors.
//!
//! Each extractor walks a tree-sitter parse tree depth-first, emitting
//! [`Symbol`] entries. Container kinds (class, struct, enum, ...) are
//! stamped onto the `container` field of nested symbols so the outline
//! fold in [`super::outline`] can rebuild a tree.
//!
//! The shape of every extractor is identical:
//!
//! 1. Walk all named descendants of the root.
//! 2. For each interesting node kind, emit a [`Symbol`] and (if it's a
//!    container) return a new container name to stamp on its descendants.
//!
//! Common idioms — name extraction by field, `function f(args)` signature
//! shaping, container tracking — live in the [`helpers`] sub-module so
//! each per-language extractor stays small.

use tree_sitter::{Node, Tree};

use super::language::Language;
use super::types::{Symbol, SymbolKind};

mod c_family;
mod ecmascript;
mod harn;
pub(super) mod helpers;

use helpers::{
    explicit_access_level, field_text, has_anonymous_child, named_decl_with_keyword, point_pos,
    push_func, truncate_end, walk_named, NamedDeclArgs, NodePos, PushFuncArgs,
};

/// Extract a flat symbol list from a parsed tree.
pub(super) fn extract(tree: &Tree, source: &str, language: Language) -> Vec<Symbol> {
    let root = tree.root_node();
    let mut out: Vec<Symbol> = Vec::new();

    match language {
        Language::Harn => harn::extract(root, source, &mut out),
        Language::TypeScript | Language::Tsx => {
            ecmascript::extract_typescript(root, source, &mut out);
        }
        Language::JavaScript | Language::Jsx => {
            ecmascript::extract_javascript(root, source, &mut out);
        }
        Language::Go => extract_go(root, source, &mut out),
        Language::Rust => extract_rust(root, source, &mut out),
        Language::Python => extract_python(root, source, &mut out),
        Language::Java => extract_java(root, source, &mut out),
        Language::C => c_family::extract_c(root, source, &mut out),
        Language::Cpp => c_family::extract_cpp(root, source, &mut out),
        Language::CSharp => extract_csharp(root, source, &mut out),
        Language::Kotlin => extract_kotlin(root, source, &mut out),
        Language::Ruby => extract_ruby(root, source, &mut out),
        Language::Php => extract_php(root, source, &mut out),
        Language::Scala => extract_scala(root, source, &mut out),
        Language::Bash => extract_bash(root, source, &mut out),
        Language::Swift => extract_swift(root, source, &mut out),
        Language::Zig => extract_zig(root, source, &mut out),
        Language::Elixir => extract_elixir(root, source, &mut out),
        Language::Lua => extract_lua(root, source, &mut out),
        Language::Haskell => extract_haskell(root, source, &mut out),
        Language::R => extract_r(root, source, &mut out),
        // Data/markup/config grammars carry no nameable symbols; they
        // support the edit primitives but expose an empty symbol/outline
        // projection (see `Language::supports_symbol_extraction`).
        Language::Json
        | Language::Yaml
        | Language::Toml
        | Language::Css
        | Language::Html
        | Language::Sql
        | Language::Markdown => {}
    }

    out
}

fn child_text_by_kind(node: Node<'_>, source: &str, kinds: &[&str]) -> Option<String> {
    helpers::children(node)
        .find(|child| child.is_named() && kinds.contains(&child.kind()))
        .map(|child| helpers::node_text(child, source))
}

fn descendant_text_by_kind(node: Node<'_>, source: &str, kinds: &[&str]) -> Option<String> {
    for child in helpers::children(node) {
        if child.is_named() && kinds.contains(&child.kind()) {
            if let Some(name) = field_text(child, "name", source) {
                return Some(name);
            }
            return Some(helpers::node_text(child, source));
        }
        if let Some(found) = descendant_text_by_kind(child, source, kinds) {
            return Some(found);
        }
    }
    None
}

fn enum_case_symbol(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    out: &mut Vec<Symbol>,
) {
    let name = if matches!(
        node.kind(),
        "identifier" | "simple_identifier" | "property_identifier" | "field_identifier"
    ) {
        Some(helpers::node_text(node, source))
    } else {
        field_text(node, "name", source).or_else(|| {
            child_text_by_kind(
                node,
                source,
                &[
                    "identifier",
                    "simple_identifier",
                    "property_identifier",
                    "field_identifier",
                ],
            )
        })
    };
    let Some(name) = name else {
        return;
    };
    if name.is_empty() {
        return;
    }
    out.push(helpers::sym(
        &name,
        SymbolKind::EnumCase,
        container,
        format!("case {name}"),
        pos,
    ));
}

fn field_symbol(
    name: &str,
    signature: String,
    container: Option<&str>,
    access_level: Option<&str>,
    pos: NodePos,
    out: &mut Vec<Symbol>,
) {
    if name.is_empty() {
        return;
    }
    out.push(helpers::sym_with_access_level(
        name,
        SymbolKind::Field,
        container,
        access_level,
        signature,
        pos,
    ));
}

// ---------------------------------------------------------------------------
// Go
// ---------------------------------------------------------------------------

fn extract_go(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    extract_go_package(root, source, out);

    walk_named(root, None, &mut |node, container| {
        let pos = point_pos(node);
        match node.kind() {
            "function_declaration" => {
                let name_node = node.child_by_field_name("name")?;
                let name = helpers::node_text(name_node, source);
                let params = field_text(node, "parameters", source).unwrap_or_else(|| "()".into());
                let prefix = if name.starts_with("Test")
                    || name.starts_with("Benchmark")
                    || name.starts_with("Example")
                {
                    "[test] "
                } else {
                    ""
                };
                let sig = format!("{prefix}func {name}{}", truncate_end(&params, 80));
                out.push(helpers::sym(
                    &name,
                    SymbolKind::Function,
                    container,
                    sig,
                    pos,
                ));
                None
            }
            "method_declaration" => {
                let name_node = node.child_by_field_name("name")?;
                let name = helpers::node_text(name_node, source);
                let receiver = field_text(node, "receiver", source).unwrap_or_default();
                let params = field_text(node, "parameters", source).unwrap_or_else(|| "()".into());
                let sig = format!("func {receiver} {name}{}", truncate_end(&params, 80));
                out.push(helpers::sym(&name, SymbolKind::Method, container, sig, pos));
                None
            }
            "type_declaration" => extract_go_type_decl(node, source, container, pos, out),
            "field_declaration" => {
                push_go_field(node, source, container, pos, out);
                None
            }
            _ => None,
        }
    });
}

fn push_go_field(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    out: &mut Vec<Symbol>,
) {
    let Some(name) = field_text(node, "name", source)
        .or_else(|| child_text_by_kind(node, source, &["field_identifier", "identifier"]))
    else {
        return;
    };
    let access = if name.chars().next().is_some_and(char::is_uppercase) {
        Some("public")
    } else {
        Some("private")
    };
    let signature = field_text(node, "type", source)
        .map(|ty| format!("{name} {ty}"))
        .unwrap_or_else(|| name.clone());
    field_symbol(&name, signature, container, access, pos, out);
}

fn extract_go_package(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    for child in helpers::children(root) {
        if child.kind() != "package_clause" {
            continue;
        }
        let name_node = child
            .child_by_field_name("name")
            .or_else(|| child.child(1u32));
        if let Some(n) = name_node {
            let name = helpers::node_text(n, source);
            let pos = point_pos(child);
            out.push(helpers::sym(
                &name,
                SymbolKind::Module,
                None,
                format!("package {name}"),
                pos,
            ));
        }
        break;
    }
}

fn extract_go_type_decl(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    out: &mut Vec<Symbol>,
) -> Option<String> {
    for spec in helpers::children(node) {
        if spec.kind() != "type_spec" {
            continue;
        }
        let Some(name_node) = spec.child_by_field_name("name") else {
            continue;
        };
        let Some(type_node) = spec.child_by_field_name("type") else {
            continue;
        };
        let name = helpers::node_text(name_node, source);
        match type_node.kind() {
            "struct_type" => {
                out.push(helpers::sym(
                    &name,
                    SymbolKind::Struct,
                    container,
                    format!("type {name} struct"),
                    pos,
                ));
                return Some(name);
            }
            "interface_type" => {
                out.push(helpers::sym(
                    &name,
                    SymbolKind::Interface,
                    container,
                    format!("type {name} interface"),
                    pos,
                ));
                return Some(name);
            }
            _ => {
                out.push(helpers::sym(
                    &name,
                    SymbolKind::Type,
                    container,
                    format!("type {name}"),
                    pos,
                ));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Rust
// ---------------------------------------------------------------------------

fn extract_rust(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        let pos = point_pos(node);
        match node.kind() {
            "function_item" => {
                push_func(PushFuncArgs {
                    node,
                    source,
                    container,
                    pos,
                    kind: SymbolKind::Function,
                    prefix: "fn",
                    out,
                });
                None
            }
            "struct_item" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Struct,
                keyword: "struct",
                out,
            }),
            "enum_item" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Enum,
                keyword: "enum",
                out,
            }),
            "enum_variant" => {
                enum_case_symbol(node, source, container, pos, out);
                None
            }
            "field_declaration" => {
                push_rust_field(node, source, container, pos, out);
                None
            }
            "trait_item" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Protocol,
                keyword: "trait",
                out,
            }),
            "impl_item" => node
                .child_by_field_name("type")
                .map(|n| helpers::node_text(n, source)),
            "type_item" => {
                named_decl_with_keyword(NamedDeclArgs {
                    node,
                    source,
                    container,
                    pos,
                    kind: SymbolKind::Type,
                    keyword: "type",
                    out,
                });
                None
            }
            _ => None,
        }
    });
}

fn push_rust_field(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    out: &mut Vec<Symbol>,
) {
    let Some(name) = field_text(node, "name", source)
        .or_else(|| child_text_by_kind(node, source, &["field_identifier", "identifier"]))
    else {
        return;
    };
    let signature = field_text(node, "type", source)
        .map(|ty| format!("{name}: {ty}"))
        .unwrap_or_else(|| name.clone());
    let access = explicit_access_level(node, source).or(Some("private"));
    field_symbol(&name, signature, container, access, pos, out);
}

// ---------------------------------------------------------------------------
// Python
// ---------------------------------------------------------------------------

fn extract_python(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        let pos = point_pos(node);
        match node.kind() {
            "class_definition" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Class,
                keyword: "class",
                out,
            }),
            "function_definition" => {
                let name_node = node.child_by_field_name("name")?;
                let name = helpers::node_text(name_node, source);
                let params = field_text(node, "parameters", source).unwrap_or_else(|| "()".into());
                let cleaned = strip_self_param(&params);
                let kind = if container.is_some() {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                let sig = format!("def {name}{}", truncate_end(&cleaned, 80));
                out.push(helpers::sym(&name, kind, container, sig, pos));
                None
            }
            _ => None,
        }
    });
}

fn strip_self_param(params: &str) -> String {
    if let Some(rest) = params.strip_prefix("(self,") {
        return format!("({}", rest.trim_start());
    }
    if params.starts_with("(self)") {
        return "()".into();
    }
    if let Some(rest) = params.strip_prefix("(cls,") {
        return format!("({}", rest.trim_start());
    }
    if params.starts_with("(cls)") {
        return "()".into();
    }
    params.to_string()
}

// ---------------------------------------------------------------------------
// Java
// ---------------------------------------------------------------------------

fn extract_java(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        let pos = point_pos(node);
        match node.kind() {
            "class_declaration" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Class,
                keyword: "class",
                out,
            }),
            "interface_declaration" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Interface,
                keyword: "interface",
                out,
            }),
            "enum_declaration" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Enum,
                keyword: "enum",
                out,
            }),
            "enum_constant" => {
                enum_case_symbol(node, source, container, pos, out);
                None
            }
            "field_declaration" => {
                push_jvm_field(node, source, container, pos, "type", Some("package"), out);
                None
            }
            "method_declaration" => {
                push_jvm_method(node, source, container, pos, out);
                None
            }
            "constructor_declaration" => {
                push_func(PushFuncArgs {
                    node,
                    source,
                    container,
                    pos,
                    kind: SymbolKind::Method,
                    prefix: "",
                    out,
                });
                None
            }
            _ => None,
        }
    });
}

fn push_jvm_field(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    type_field: &str,
    default_access: Option<&str>,
    out: &mut Vec<Symbol>,
) {
    let name = field_text(node, "name", source)
        .or_else(|| descendant_text_by_kind(node, source, &["variable_declarator", "identifier"]));
    let Some(name) = name else {
        return;
    };
    let field_type = field_text(node, type_field, source).unwrap_or_default();
    let signature = format!("{field_type} {name}").trim().to_string();
    let access = explicit_access_level(node, source).or(default_access);
    field_symbol(&name, signature, container, access, pos, out);
}

fn push_jvm_method(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    out: &mut Vec<Symbol>,
) {
    push_typed_method(node, source, container, pos, "type", out);
}

fn push_csharp_method(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    out: &mut Vec<Symbol>,
) {
    push_typed_method(node, source, container, pos, "returns", out);
}

/// Shared shape for languages that expose `name` + `parameters` +
/// "return-type" fields on their method nodes (Java, C#).
fn push_typed_method(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    return_field: &str,
    out: &mut Vec<Symbol>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let name = helpers::node_text(name_node, source);
    let params = field_text(node, "parameters", source).unwrap_or_else(|| "()".into());
    let return_type = field_text(node, return_field, source).unwrap_or_else(|| "void".into());
    let kind = if container.is_some() {
        SymbolKind::Method
    } else {
        SymbolKind::Function
    };
    let sig = format!("{return_type} {name}{}", truncate_end(&params, 80));
    out.push(helpers::sym(&name, kind, container, sig, pos));
}

// ---------------------------------------------------------------------------
// C#
// ---------------------------------------------------------------------------

fn extract_csharp(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        let pos = point_pos(node);
        match node.kind() {
            "namespace_declaration" | "file_scoped_namespace_declaration" => {
                named_decl_with_keyword(NamedDeclArgs {
                    node,
                    source,
                    container,
                    pos,
                    kind: SymbolKind::Module,
                    keyword: "namespace",
                    out,
                })
            }
            "class_declaration" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Class,
                keyword: "class",
                out,
            }),
            "struct_declaration" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Struct,
                keyword: "struct",
                out,
            }),
            "interface_declaration" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Interface,
                keyword: "interface",
                out,
            }),
            "enum_declaration" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Enum,
                keyword: "enum",
                out,
            }),
            "enum_member_declaration" => {
                enum_case_symbol(node, source, container, pos, out);
                None
            }
            "method_declaration" => {
                push_csharp_method(node, source, container, pos, out);
                None
            }
            "field_declaration" => {
                push_jvm_field(node, source, container, pos, "type", Some("private"), out);
                None
            }
            "property_declaration" => {
                let name_node = node.child_by_field_name("name")?;
                let name = helpers::node_text(name_node, source);
                let prop_type = field_text(node, "type", source).unwrap_or_default();
                let access = explicit_access_level(node, source).or(Some("private"));
                out.push(helpers::sym_with_access_level(
                    &name,
                    SymbolKind::Field,
                    container,
                    access,
                    format!("{prop_type} {name}").trim().to_string(),
                    pos,
                ));
                None
            }
            _ => None,
        }
    });
}

// ---------------------------------------------------------------------------
// Kotlin
// ---------------------------------------------------------------------------

fn extract_kotlin(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        let pos = point_pos(node);
        match node.kind() {
            "class_declaration" => {
                let name_node = node.child_by_field_name("name")?;
                let name = helpers::node_text(name_node, source);
                let is_interface = has_anonymous_child(node, "interface", source);
                let kind = if is_interface {
                    SymbolKind::Interface
                } else {
                    SymbolKind::Class
                };
                let keyword = if is_interface { "interface" } else { "class" };
                out.push(helpers::sym(
                    &name,
                    kind,
                    container,
                    format!("{keyword} {name}"),
                    pos,
                ));
                Some(name)
            }
            "object_declaration" => {
                let name_node = node.child_by_field_name("name")?;
                let name = helpers::node_text(name_node, source);
                out.push(helpers::sym(
                    &name,
                    SymbolKind::Class,
                    container,
                    format!("object {name}"),
                    pos,
                ));
                Some(name)
            }
            "enum_entry" => {
                enum_case_symbol(node, source, container, pos, out);
                None
            }
            "property_declaration" => {
                push_kotlin_field(node, source, container, pos, out);
                None
            }
            "function_declaration" => {
                let name_node = node.child_by_field_name("name")?;
                let name = helpers::node_text(name_node, source);
                let kind = if container.is_some() {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                out.push(helpers::sym(
                    &name,
                    kind,
                    container,
                    format!("fun {name}"),
                    pos,
                ));
                None
            }
            _ => None,
        }
    });
}

fn push_kotlin_field(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    out: &mut Vec<Symbol>,
) {
    let Some(name) = field_text(node, "name", source)
        .or_else(|| child_text_by_kind(node, source, &["identifier", "simple_identifier"]))
    else {
        return;
    };
    let access = explicit_access_level(node, source).or(Some("public"));
    field_symbol(&name, name.clone(), container, access, pos, out);
}

// ---------------------------------------------------------------------------
// Ruby
// ---------------------------------------------------------------------------

fn extract_ruby(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        let pos = point_pos(node);
        match node.kind() {
            "class" => {
                let name_node = node.child_by_field_name("name")?;
                let name = helpers::node_text(name_node, source);
                out.push(helpers::sym(
                    &name,
                    SymbolKind::Class,
                    container,
                    format!("class {name}"),
                    pos,
                ));
                Some(name)
            }
            "module" => {
                let name_node = node.child_by_field_name("name")?;
                let name = helpers::node_text(name_node, source);
                out.push(helpers::sym(
                    &name,
                    SymbolKind::Module,
                    container,
                    format!("module {name}"),
                    pos,
                ));
                Some(name)
            }
            "method" => {
                let name_node = node.child_by_field_name("name")?;
                let name = helpers::node_text(name_node, source);
                let params = field_text(node, "parameters", source).unwrap_or_default();
                let kind = if container.is_some() {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                let sig = if params.is_empty() {
                    format!("def {name}")
                } else {
                    format!("def {name}{}", truncate_end(&params, 80))
                };
                out.push(helpers::sym(&name, kind, container, sig, pos));
                None
            }
            "singleton_method" => {
                let name_node = node.child_by_field_name("name")?;
                let name = helpers::node_text(name_node, source);
                out.push(helpers::sym(
                    &name,
                    SymbolKind::Method,
                    container,
                    format!("def self.{name}"),
                    pos,
                ));
                None
            }
            _ => None,
        }
    });
}

// ---------------------------------------------------------------------------
// PHP
// ---------------------------------------------------------------------------

fn extract_php(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        let pos = point_pos(node);
        match node.kind() {
            "class_declaration" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Class,
                keyword: "class",
                out,
            }),
            "interface_declaration" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Interface,
                keyword: "interface",
                out,
            }),
            "enum_declaration" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Enum,
                keyword: "enum",
                out,
            }),
            "enum_case" | "case_declaration" => {
                enum_case_symbol(node, source, container, pos, out);
                None
            }
            "function_definition" => {
                push_func(PushFuncArgs {
                    node,
                    source,
                    container,
                    pos,
                    kind: SymbolKind::Function,
                    prefix: "function",
                    out,
                });
                None
            }
            "method_declaration" => {
                push_func(PushFuncArgs {
                    node,
                    source,
                    container,
                    pos,
                    kind: SymbolKind::Method,
                    prefix: "",
                    out,
                });
                None
            }
            _ => None,
        }
    });
}

// ---------------------------------------------------------------------------
// Scala
// ---------------------------------------------------------------------------

fn extract_scala(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        let pos = point_pos(node);
        match node.kind() {
            "class_definition" => {
                let name_node = node.child_by_field_name("name")?;
                let name = helpers::node_text(name_node, source);
                let is_case = has_anonymous_child(node, "case", source);
                let sig = if is_case {
                    format!("case class {name}")
                } else {
                    format!("class {name}")
                };
                out.push(helpers::sym(&name, SymbolKind::Class, container, sig, pos));
                Some(name)
            }
            "trait_definition" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Protocol,
                keyword: "trait",
                out,
            }),
            "object_definition" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Class,
                keyword: "object",
                out,
            }),
            "enum_definition" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Enum,
                keyword: "enum",
                out,
            }),
            "enum_case" | "case_clause" => {
                if container.is_some() {
                    enum_case_symbol(node, source, container, pos, out);
                }
                None
            }
            "function_definition" | "function_declaration" => {
                let name_node = node.child_by_field_name("name")?;
                let name = helpers::node_text(name_node, source);
                let params = field_text(node, "parameters", source).unwrap_or_default();
                let kind = if container.is_some() {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                out.push(helpers::sym(
                    &name,
                    kind,
                    container,
                    format!("def {name}{}", truncate_end(&params, 80)),
                    pos,
                ));
                None
            }
            "type_definition" => {
                named_decl_with_keyword(NamedDeclArgs {
                    node,
                    source,
                    container,
                    pos,
                    kind: SymbolKind::Type,
                    keyword: "type",
                    out,
                });
                None
            }
            "val_definition" | "val_declaration" => {
                push_scala_binding(
                    node,
                    source,
                    container,
                    pos,
                    SymbolKind::Variable,
                    "val",
                    out,
                );
                None
            }
            "var_definition" | "var_declaration" => {
                push_scala_binding(
                    node,
                    source,
                    container,
                    pos,
                    SymbolKind::Variable,
                    "var",
                    out,
                );
                None
            }
            _ => None,
        }
    });
}

fn push_scala_binding(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    kind: SymbolKind,
    keyword: &str,
    out: &mut Vec<Symbol>,
) {
    let pattern = node
        .child_by_field_name("pattern")
        .or_else(|| node.child_by_field_name("name"));
    let Some(node_text_src) = pattern else {
        return;
    };
    let name = helpers::node_text(node_text_src, source);
    if name.len() <= 1 {
        return;
    }
    let symbol_kind = if kind == SymbolKind::Variable && container.is_some() {
        SymbolKind::Field
    } else {
        kind
    };
    let access = (symbol_kind == SymbolKind::Field)
        .then(|| explicit_access_level(node, source).unwrap_or("public"));
    out.push(helpers::sym_with_access_level(
        &name,
        symbol_kind,
        container,
        access,
        format!("{keyword} {name}"),
        pos,
    ));
}

// ---------------------------------------------------------------------------
// Bash
// ---------------------------------------------------------------------------

fn extract_bash(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        if node.kind() != "function_definition" {
            return None;
        }
        let pos = point_pos(node);
        let name_node = node.child_by_field_name("name")?;
        let name = helpers::node_text(name_node, source);
        out.push(helpers::sym(
            &name,
            SymbolKind::Function,
            container,
            format!("function {name}"),
            pos,
        ));
        None
    });
}

// ---------------------------------------------------------------------------
// Swift
// ---------------------------------------------------------------------------

fn extract_swift(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        let pos = point_pos(node);
        match node.kind() {
            "function_declaration" => {
                push_func(PushFuncArgs {
                    node,
                    source,
                    container,
                    pos,
                    kind: SymbolKind::Function,
                    prefix: "func",
                    out,
                });
                None
            }
            "class_declaration" => extract_swift_class(node, source, container, pos, out),
            "enum_entry" => {
                enum_case_symbol(node, source, container, pos, out);
                None
            }
            "protocol_declaration" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Protocol,
                keyword: "protocol",
                out,
            }),
            "property_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = helpers::node_text(name_node, source);
                    let kind = if container.is_some() {
                        SymbolKind::Field
                    } else {
                        SymbolKind::Variable
                    };
                    let access = (kind == SymbolKind::Field)
                        .then(|| explicit_access_level(node, source).unwrap_or("internal"));
                    out.push(helpers::sym_with_access_level(
                        &name,
                        kind,
                        container,
                        access,
                        name.clone(),
                        pos,
                    ));
                }
                None
            }
            _ => None,
        }
    });
}

fn extract_swift_class(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    out: &mut Vec<Symbol>,
) -> Option<String> {
    let (kind, keyword) = match node
        .child_by_field_name("declaration_kind")
        .map(|n| helpers::node_text(n, source))
    {
        Some(t) if t == "struct" => (SymbolKind::Struct, "struct"),
        Some(t) if t == "enum" => (SymbolKind::Enum, "enum"),
        Some(t) if t == "actor" => (SymbolKind::Class, "actor"),
        Some(t) if t == "extension" => (SymbolKind::Other, "extension"),
        _ => (SymbolKind::Class, "class"),
    };
    named_decl_with_keyword(NamedDeclArgs {
        node,
        source,
        container,
        pos,
        kind,
        keyword,
        out,
    })
}

// ---------------------------------------------------------------------------
// Zig
// ---------------------------------------------------------------------------

fn extract_zig(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        let pos = point_pos(node);
        match node.kind() {
            "function_declaration" => {
                push_func(PushFuncArgs {
                    node,
                    source,
                    container,
                    pos,
                    kind: SymbolKind::Function,
                    prefix: "fn",
                    out,
                });
                None
            }
            "container_declaration" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Struct,
                keyword: "struct",
                out,
            }),
            "test_declaration" => {
                extract_zig_test(node, source, container, pos, out);
                None
            }
            _ => None,
        }
    });
}

fn extract_zig_test(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    out: &mut Vec<Symbol>,
) {
    for child in helpers::children(node) {
        if child.kind() != "string" {
            continue;
        }
        let mut name = helpers::node_text(child, source);
        for content in helpers::children(child) {
            if content.kind() == "string_content" {
                name = helpers::node_text(content, source);
                break;
            }
        }
        let trimmed = name.trim_matches('"').to_string();
        out.push(helpers::sym(
            &trimmed,
            SymbolKind::Function,
            container,
            format!("test \"{trimmed}\""),
            pos,
        ));
        return;
    }
}

// ---------------------------------------------------------------------------
// Elixir
// ---------------------------------------------------------------------------

fn extract_elixir(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        if node.kind() != "call" {
            return None;
        }
        let pos = point_pos(node);
        let target = node.child(0u32)?;
        let keyword = helpers::node_text(target, source);
        match keyword.as_str() {
            "defmodule" => {
                let arg = node.child(1u32)?;
                let name = helpers::node_text(arg, source)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                out.push(helpers::sym(
                    &name,
                    SymbolKind::Module,
                    container,
                    format!("defmodule {name}"),
                    pos,
                ));
                Some(name)
            }
            "def" | "defp" => {
                let arg = node.child(1u32)?;
                let sig = helpers::node_text(arg, source)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                let name = sig.split('(').next().unwrap_or(&sig).trim().to_string();
                let kind = if container.is_some() {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                out.push(helpers::sym(
                    &name,
                    kind,
                    container,
                    format!("{keyword} {}", truncate_end(&sig, 80)),
                    pos,
                ));
                None
            }
            _ => None,
        }
    });
}

// ---------------------------------------------------------------------------
// Lua
// ---------------------------------------------------------------------------

fn extract_lua(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        let pos = point_pos(node);
        match node.kind() {
            "function_declaration" => {
                let name_node = node.child_by_field_name("name")?;
                let name = helpers::node_text(name_node, source);
                let params = field_text(node, "parameters", source).unwrap_or_else(|| "()".into());
                let kind = if name.contains(':') {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                out.push(helpers::sym(
                    &name,
                    kind,
                    container,
                    format!("function {name}{}", truncate_end(&params, 80)),
                    pos,
                ));
                None
            }
            "local_function" => {
                let name_node = node.child_by_field_name("name")?;
                let name = helpers::node_text(name_node, source);
                let params = field_text(node, "parameters", source).unwrap_or_else(|| "()".into());
                out.push(helpers::sym(
                    &name,
                    SymbolKind::Function,
                    container,
                    format!("local function {name}{}", truncate_end(&params, 80)),
                    pos,
                ));
                None
            }
            _ => None,
        }
    });
}

// ---------------------------------------------------------------------------
// Haskell
// ---------------------------------------------------------------------------

fn extract_haskell(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        let pos = point_pos(node);
        match node.kind() {
            "function" => {
                if let Some(name_node) = node.child(0u32) {
                    let name = helpers::node_text(name_node, source);
                    if !name.is_empty() {
                        out.push(helpers::sym(
                            &name,
                            SymbolKind::Function,
                            container,
                            name.clone(),
                            pos,
                        ));
                    }
                }
                None
            }
            "adt" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Class,
                keyword: "data",
                out,
            }),
            "newtype" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Type,
                keyword: "newtype",
                out,
            }),
            "class" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Interface,
                keyword: "class",
                out,
            }),
            _ => None,
        }
    });
}

// ---------------------------------------------------------------------------
// R
// ---------------------------------------------------------------------------

fn extract_r(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        if node.kind() != "binary_operator" {
            return None;
        }
        let pos = point_pos(node);
        let name_node = node.child_by_field_name("lhs")?;
        let value_node = node.child_by_field_name("rhs")?;
        if value_node.kind() != "function_definition" {
            return None;
        }
        let name = helpers::node_text(name_node, source);
        let params = value_node
            .child_by_field_name("parameters")
            .map(|n| helpers::node_text(n, source))
            .unwrap_or_else(|| "()".into());
        out.push(helpers::sym(
            &name,
            SymbolKind::Function,
            container,
            format!("{name} <- function{}", truncate_end(&params, 80)),
            pos,
        ));
        None
    });
}
