//! Harn symbol projection.

use tree_sitter::Node;

use super::super::types::{Symbol, SymbolKind};
use super::helpers::NodePos;
use super::helpers::{
    self, explicit_access_level, field_text, point_pos, truncate_end, walk_named,
};
use super::{enum_case_symbol, field_symbol};

pub(super) fn extract(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        let pos = point_pos(node);
        match node.kind() {
            "pipeline_declaration" => {
                push_callable(
                    node,
                    source,
                    container,
                    pos,
                    SymbolKind::Function,
                    "pipeline",
                    out,
                );
                None
            }
            "fn_declaration" => {
                let kind = if container.is_some() {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                push_callable(node, source, container, pos, kind, "fn", out);
                None
            }
            "tool_declaration" => {
                push_callable(
                    node,
                    source,
                    container,
                    pos,
                    SymbolKind::Function,
                    "tool",
                    out,
                );
                None
            }
            "override_declaration" | "interface_method" => {
                push_callable(node, source, container, pos, SymbolKind::Method, "fn", out);
                None
            }
            "skill_declaration" => named_decl(
                node,
                source,
                container,
                pos,
                SymbolKind::Other,
                "skill",
                out,
            ),
            "eval_pack_declaration" => {
                push_eval_pack(node, source, container, pos, out);
                None
            }
            "struct_declaration" => named_decl(
                node,
                source,
                container,
                pos,
                SymbolKind::Struct,
                "struct",
                out,
            ),
            "enum_declaration" => {
                named_decl(node, source, container, pos, SymbolKind::Enum, "enum", out)
            }
            "enum_variant" | "enum_case" => {
                enum_case_symbol(node, source, container, pos, out);
                None
            }
            "struct_field" => {
                push_field(node, source, container, pos, out);
                None
            }
            "interface_declaration" => named_decl(
                node,
                source,
                container,
                pos,
                SymbolKind::Interface,
                "interface",
                out,
            ),
            "type_declaration" | "associated_type_declaration" => {
                named_decl(node, source, container, pos, SymbolKind::Type, "type", out)
            }
            "let_binding" | "const_binding" | "var_binding"
                if node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "source_file") =>
            {
                push_module_binding(node, source, pos, out);
                None
            }
            "impl_block" => push_impl(node, source, container, pos, out),
            _ => None,
        }
    });
}

fn push_module_binding(node: Node<'_>, source: &str, pos: NodePos, out: &mut Vec<Symbol>) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    if name_node.kind() != "identifier" {
        return;
    }
    let name = helpers::node_text(name_node, source);
    let keyword = match node.kind() {
        "let_binding" => "let",
        "const_binding" => "const",
        "var_binding" => "var",
        _ => return,
    };
    out.push(helpers::sym(
        &name,
        SymbolKind::Variable,
        None,
        format!("{keyword} {name}"),
        pos,
    ));
}

fn push_field(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    out: &mut Vec<Symbol>,
) {
    let Some(name) = field_text(node, "name", source) else {
        return;
    };
    let signature = field_text(node, "type", source)
        .map(|ty| format!("{name}: {ty}"))
        .unwrap_or_else(|| name.clone());
    field_symbol(
        &name,
        signature,
        container,
        explicit_access_level(node, source),
        pos,
        out,
    );
}

fn push_callable(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    kind: SymbolKind,
    keyword: &'static str,
    out: &mut Vec<Symbol>,
) {
    let Some(name) = field_text(node, "name", source) else {
        return;
    };
    let params = parameter_text(node, source);
    out.push(helpers::sym(
        &name,
        kind,
        container,
        format!("{keyword} {name}{}", truncate_end(&params, 80)),
        pos,
    ));
}

fn named_decl(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    kind: SymbolKind,
    keyword: &'static str,
    out: &mut Vec<Symbol>,
) -> Option<String> {
    let name = field_text(node, "name", source)?;
    out.push(helpers::sym(
        &name,
        kind,
        container,
        format!("{keyword} {name}"),
        pos,
    ));
    kind.is_container().then_some(name)
}

fn push_eval_pack(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    out: &mut Vec<Symbol>,
) {
    let name = field_text(node, "name", source)
        .or_else(|| field_text(node, "id", source).map(|id| id.trim_matches('"').to_string()));
    let Some(name) = name else {
        return;
    };
    out.push(helpers::sym(
        &name,
        SymbolKind::Other,
        container,
        format!("eval_pack {name}"),
        pos,
    ));
}

fn push_impl(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    out: &mut Vec<Symbol>,
) -> Option<String> {
    let name = field_text(node, "type_name", source)?;
    out.push(helpers::sym(
        &name,
        SymbolKind::Module,
        container,
        format!("impl {name}"),
        pos,
    ));
    Some(name)
}

fn parameter_text(node: Node<'_>, source: &str) -> String {
    helpers::children(node)
        .find(|child| child.is_named() && child.kind() == "parameter_list")
        .map(|child| format!("({})", helpers::node_text(child, source)))
        .unwrap_or_else(|| "()".to_string())
}
