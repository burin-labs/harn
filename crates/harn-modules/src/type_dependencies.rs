//! Type declarations required by imported callable signatures.
//!
//! A callable keeps the type environment of its defining module even when a
//! facade re-exports it. The caller therefore needs the callable's named type
//! dependencies, not every private type reachable from the facade.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use harn_parser::{Node, SNode, TypeExpr};

use super::{normalize_path, type_decl_name, ModuleGraph};

impl ModuleGraph {
    pub(super) fn extend_callable_type_dependencies(
        &self,
        origin: &Path,
        callable: &SNode,
        declarations: &mut Vec<SNode>,
        seen: &mut HashSet<(PathBuf, String)>,
    ) {
        let mut names = HashSet::new();
        collect_callable_type_names(callable, &mut names);
        let mut names: Vec<String> = names.into_iter().collect();
        names.sort();
        for name in names {
            self.extend_named_type_dependency(origin, &name, declarations, seen);
        }
    }

    pub(super) fn extend_type_dependency(
        &self,
        origin: &Path,
        declaration: &SNode,
        declarations: &mut Vec<SNode>,
        seen: &mut HashSet<(PathBuf, String)>,
    ) {
        let Some(name) = type_decl_name(declaration) else {
            return;
        };
        let origin = normalize_path(origin);
        if !seen.insert((origin.clone(), name.to_string())) {
            return;
        }
        declarations.push(declaration.clone());

        let mut names = HashSet::new();
        collect_declaration_type_names(declaration, &mut names);
        let mut names: Vec<String> = names.into_iter().collect();
        names.sort();
        for dependency in names {
            self.extend_named_type_dependency(&origin, &dependency, declarations, seen);
        }
    }

    fn extend_named_type_dependency(
        &self,
        visible_from: &Path,
        name: &str,
        declarations: &mut Vec<SNode>,
        seen: &mut HashSet<(PathBuf, String)>,
    ) {
        let mut visited = HashSet::new();
        let Some((declaration, origin)) =
            self.find_visible_type_declaration(visible_from, name, &mut visited)
        else {
            return;
        };
        self.extend_type_dependency(&origin, &declaration, declarations, seen);
    }

    fn find_visible_type_declaration(
        &self,
        path: &Path,
        name: &str,
        visited: &mut HashSet<PathBuf>,
    ) -> Option<(SNode, PathBuf)> {
        let path = normalize_path(path);
        if !visited.insert(path.clone()) {
            return None;
        }
        let module = self.modules.get(&path)?;
        if let Some(declaration) = module
            .type_declarations
            .iter()
            .find(|declaration| type_decl_name(declaration) == Some(name))
        {
            return Some((declaration.clone(), path));
        }

        for import in &module.imports {
            if import.namespace_alias.is_some()
                || import
                    .selective_names
                    .as_ref()
                    .is_some_and(|names| !names.contains(name))
            {
                continue;
            }
            let Some(import_path) = import.path.as_ref() else {
                continue;
            };
            if let Some(found) = self.find_visible_type_declaration(import_path, name, visited) {
                return Some(found);
            }
        }
        None
    }
}

fn collect_callable_type_names(callable: &SNode, names: &mut HashSet<String>) {
    let callable = match &callable.node {
        Node::AttributedDecl { inner, .. } => inner.as_ref(),
        _ => callable,
    };
    match &callable.node {
        Node::FnDecl {
            params,
            return_type,
            type_predicate,
            throws,
            where_clauses,
            ..
        } => {
            collect_parameter_type_names(params, names);
            collect_optional_type_name(return_type, names);
            if let Some(predicate) = type_predicate {
                collect_type_names(&predicate.type_expr, names);
            }
            collect_optional_type_name(throws, names);
            for clause in where_clauses {
                collect_type_names(&clause.bound, names);
            }
        }
        Node::Pipeline {
            params,
            return_type,
            throws,
            ..
        }
        | Node::ToolDecl {
            params,
            return_type,
            throws,
            ..
        } => {
            collect_parameter_type_names(params, names);
            collect_optional_type_name(return_type, names);
            collect_optional_type_name(throws, names);
        }
        _ => {}
    }
}

fn collect_declaration_type_names(declaration: &SNode, names: &mut HashSet<String>) {
    let declaration = match &declaration.node {
        Node::AttributedDecl { inner, .. } => inner.as_ref(),
        _ => declaration,
    };
    match &declaration.node {
        Node::TypeDecl { type_expr, .. } => collect_type_names(type_expr, names),
        Node::StructDecl { fields, .. } => {
            for field in fields {
                collect_optional_type_name(&field.type_expr, names);
            }
        }
        Node::EnumDecl { variants, .. } => {
            for variant in variants {
                collect_parameter_type_names(&variant.fields, names);
            }
        }
        Node::InterfaceDecl {
            associated_types,
            methods,
            ..
        } => {
            for associated in associated_types {
                collect_optional_type_name(&associated.default, names);
            }
            for method in methods {
                collect_parameter_type_names(&method.params, names);
                collect_optional_type_name(&method.return_type, names);
            }
        }
        _ => {}
    }
}

fn collect_parameter_type_names(params: &[harn_parser::TypedParam], names: &mut HashSet<String>) {
    for param in params {
        collect_optional_type_name(&param.type_expr, names);
    }
}

fn collect_optional_type_name(type_expr: &Option<TypeExpr>, names: &mut HashSet<String>) {
    if let Some(type_expr) = type_expr {
        collect_type_names(type_expr, names);
    }
}

fn collect_type_names(type_expr: &TypeExpr, names: &mut HashSet<String>) {
    match type_expr {
        TypeExpr::Named(name) => {
            names.insert(name.clone());
        }
        TypeExpr::Applied { name, args } => {
            names.insert(name.clone());
            for arg in args {
                collect_type_names(arg, names);
            }
        }
        TypeExpr::Union(members) | TypeExpr::Intersection(members) | TypeExpr::Tuple(members) => {
            for member in members {
                collect_type_names(member, names);
            }
        }
        TypeExpr::Shape(fields) => {
            for field in fields {
                collect_type_names(&field.type_expr, names);
            }
        }
        TypeExpr::OpenShape { fields, rests } => {
            for field in fields {
                collect_type_names(&field.type_expr, names);
            }
            for rest in rests {
                collect_type_names(rest, names);
            }
        }
        TypeExpr::List(inner)
        | TypeExpr::Iter(inner)
        | TypeExpr::Generator(inner)
        | TypeExpr::Stream(inner)
        | TypeExpr::Owned(inner) => collect_type_names(inner, names),
        TypeExpr::DictType(key, value) => {
            collect_type_names(key, names);
            collect_type_names(value, names);
        }
        TypeExpr::FnType {
            params,
            return_type,
        } => {
            for param in params {
                collect_type_names(param, names);
            }
            collect_type_names(return_type, names);
        }
        TypeExpr::Never | TypeExpr::LitString(_) | TypeExpr::LitInt(_) => {}
    }
}
