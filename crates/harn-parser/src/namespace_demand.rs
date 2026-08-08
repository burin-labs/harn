//! Conservative export demand for namespace imports.
//!
//! This analysis is shared by bytecode compilation and module artifact
//! construction. It only selects individual members when every use of an
//! imported namespace is a statically named member access. Any use whose
//! meaning could depend on the complete namespace widens the demand to
//! [`NamespaceDemand::Whole`].

use std::collections::{BTreeMap, BTreeSet};

use crate::{lexical::binding_pattern_names, visit::immediate_children, Node, SNode};

/// The public exports an importer can observe through a namespace binding.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NamespaceDemand {
    /// Preserve the complete public namespace.
    Whole,
    /// Project only these statically named members.
    Members(BTreeSet<String>),
}

impl NamespaceDemand {
    fn add_member(&mut self, member: &str) {
        if let Self::Members(members) = self {
            members.insert(member.to_string());
        }
    }

    fn widen(&mut self) {
        *self = Self::Whole;
    }
}

/// Compute conservative member demand for every namespace import in `program`.
///
/// An unused private namespace has an empty member set: importing it still
/// loads and initializes its target module, but consumers may omit all public
/// exports from the namespace projection. Public namespace imports, alias
/// escapes, dynamic access, duplicate/conflicting bindings, and future syntax
/// not recognized as a direct member access all require the whole namespace.
pub fn namespace_import_demands(program: &[SNode]) -> BTreeMap<String, NamespaceDemand> {
    let mut demands = BTreeMap::new();
    let mut aliases = BTreeSet::new();

    for node in program {
        collect_imports(node, &mut aliases, &mut demands);
    }

    for alias in aliases {
        let demand = demands
            .get_mut(&alias)
            .expect("namespace alias and demand are recorded together");
        if matches!(demand, NamespaceDemand::Whole) {
            continue;
        }
        if program
            .iter()
            .any(|node| conflicts_with_alias(node, &alias))
        {
            demand.widen();
            continue;
        }
        for node in program {
            analyze_node(node, &alias, demand);
            if matches!(demand, NamespaceDemand::Whole) {
                break;
            }
        }
    }

    demands
}

fn collect_imports(
    node: &SNode,
    aliases: &mut BTreeSet<String>,
    demands: &mut BTreeMap<String, NamespaceDemand>,
) {
    if let Node::NamespaceImport { alias, is_pub, .. } = &node.node {
        let first = aliases.insert(alias.clone());
        let demand = if *is_pub || !first {
            NamespaceDemand::Whole
        } else {
            NamespaceDemand::Members(BTreeSet::new())
        };
        demands
            .entry(alias.clone())
            .and_modify(NamespaceDemand::widen)
            .or_insert(demand);
    }
    for child in immediate_children(node) {
        collect_imports(child, aliases, demands);
    }
}

fn conflicts_with_alias(node: &SNode, alias: &str) -> bool {
    let conflicts = match &node.node {
        // A wildcard can introduce any public name, so it prevents proving
        // which binding a bare identifier denotes.
        Node::ImportDecl { .. } => true,
        Node::SelectiveImport { names, .. } => names.iter().any(|name| name == alias),
        Node::NamespaceImport { .. } => false,
        Node::LetBinding { pattern, .. } | Node::ConstBinding { pattern, .. } => {
            binding_pattern_names(pattern)
                .iter()
                .any(|name| name == alias)
        }
        Node::Pipeline { name, params, .. }
        | Node::FnDecl { name, params, .. }
        | Node::ToolDecl { name, params, .. } => {
            name == alias || params.iter().any(|param| param.name == alias)
        }
        Node::OverrideDecl { name, params, .. } => {
            name == alias || params.iter().any(|param| param == alias)
        }
        Node::Closure { params, .. } => params.iter().any(|param| param.name == alias),
        Node::ForIn { pattern, .. } => binding_pattern_names(pattern)
            .iter()
            .any(|name| name == alias),
        Node::TryCatch { error_var, .. } => error_var.as_deref() == Some(alias),
        Node::Parallel { variable, .. } => variable.as_deref() == Some(alias),
        Node::SelectExpr { cases, .. } => cases.iter().any(|case| case.variable == alias),
        Node::EnumDecl { name, .. }
        | Node::StructDecl { name, .. }
        | Node::TypeDecl { name, .. }
        | Node::InterfaceDecl { name, .. }
        | Node::SkillDecl { name, .. } => name == alias,
        Node::EvalPackDecl { binding_name, .. } => binding_name == alias,
        _ => false,
    };
    conflicts
        || immediate_children(node)
            .into_iter()
            .any(|child| conflicts_with_alias(child, alias))
}

fn analyze_node(node: &SNode, alias: &str, demand: &mut NamespaceDemand) {
    if matches!(demand, NamespaceDemand::Whole) {
        return;
    }

    match &node.node {
        Node::NamespaceImport { .. } => {}
        Node::PropertyAccess { object, property }
        | Node::OptionalPropertyAccess { object, property }
            if is_identifier(object, alias) =>
        {
            demand.add_member(property);
        }
        Node::MethodCall {
            object,
            method,
            args,
        }
        | Node::OptionalMethodCall {
            object,
            method,
            args,
        } if is_identifier(object, alias) => {
            demand.add_member(method);
            for arg in args {
                analyze_node(arg, alias, demand);
            }
        }
        Node::Assignment { target, value, .. } => {
            if contains_alias(target, alias) {
                demand.widen();
            } else {
                analyze_node(value, alias, demand);
            }
        }
        Node::Identifier(name) if name == alias => demand.widen(),
        Node::FunctionCall { name, .. } if name == alias => demand.widen(),
        Node::EnumConstruct { enum_name, .. } if enum_name == alias => demand.widen(),
        Node::StructConstruct { struct_name, .. } if struct_name == alias => demand.widen(),
        _ => {
            for child in immediate_children(node) {
                analyze_node(child, alias, demand);
                if matches!(demand, NamespaceDemand::Whole) {
                    break;
                }
            }
        }
    }
}

fn is_identifier(node: &SNode, name: &str) -> bool {
    matches!(&node.node, Node::Identifier(candidate) if candidate == name)
}

fn contains_alias(node: &SNode, alias: &str) -> bool {
    match &node.node {
        Node::Identifier(name) => name == alias,
        Node::FunctionCall { name, .. } => {
            name == alias
                || immediate_children(node)
                    .into_iter()
                    .any(|child| contains_alias(child, alias))
        }
        Node::EnumConstruct { enum_name, .. } => {
            enum_name == alias
                || immediate_children(node)
                    .into_iter()
                    .any(|child| contains_alias(child, alias))
        }
        Node::StructConstruct { struct_name, .. } => {
            struct_name == alias
                || immediate_children(node)
                    .into_iter()
                    .any(|child| contains_alias(child, alias))
        }
        _ => immediate_children(node)
            .into_iter()
            .any(|child| contains_alias(child, alias)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_source;

    fn demand(source: &str, alias: &str) -> NamespaceDemand {
        namespace_import_demands(&parse_source(source).expect("source parses"))[alias].clone()
    }

    fn members(names: &[&str]) -> NamespaceDemand {
        NamespaceDemand::Members(names.iter().map(|name| (*name).to_string()).collect())
    }

    #[test]
    fn collects_static_property_and_method_members() {
        assert_eq!(
            demand(
                r#"
                import * as ui from "./ui.harn"
                const page = ui.page
                ui.render(page)
                ui?.close()
                "#,
                "ui",
            ),
            members(&["close", "page", "render"])
        );
    }

    #[test]
    fn unused_private_namespace_has_empty_member_demand() {
        assert_eq!(
            demand(r#"import * as ui from "./ui.harn""#, "ui"),
            members(&[])
        );
    }

    #[test]
    fn alias_escape_and_dynamic_access_require_whole_namespace() {
        for source in [
            r#"import * as ui from "./ui.harn"
               return ui"#,
            r#"import * as ui from "./ui.harn"
               const name = "page"
               return ui[name]"#,
        ] {
            assert_eq!(demand(source, "ui"), NamespaceDemand::Whole);
        }
    }

    #[test]
    fn public_import_and_shadowing_ambiguity_require_whole_namespace() {
        for source in [
            r#"pub import * as ui from "./ui.harn""#,
            r#"import * as ui from "./ui.harn"
               fn render(ui) { return ui.page }
               return ui.page"#,
            r#"import * as ui from "./ui.harn"
               import "./other.harn"
               return ui.page"#,
        ] {
            assert_eq!(demand(source, "ui"), NamespaceDemand::Whole);
        }
    }

    #[test]
    fn assignment_through_namespace_requires_whole_namespace() {
        assert_eq!(
            demand(
                r#"import * as ui from "./ui.harn"
                   ui.page = "replacement""#,
                "ui",
            ),
            NamespaceDemand::Whole
        );
    }
}
