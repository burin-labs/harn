//! Type declarations required by imported callable signatures.
//!
//! A callable keeps the type environment of its defining module even when a
//! facade re-exports it. The caller therefore needs the callable's named type
//! dependencies, not every private type reachable from the facade.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use harn_parser::{Node, SNode, TypeExpr};

use super::{normalize_path, type_decl_name, LocatedTypeDeclaration, ModuleGraph};

impl ModuleGraph {
    pub(super) fn extend_callable_type_dependencies(
        &self,
        origin: &Path,
        callable: &SNode,
        declarations: &mut Vec<LocatedTypeDeclaration>,
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
        declarations: &mut Vec<LocatedTypeDeclaration>,
        seen: &mut HashSet<(PathBuf, String)>,
    ) {
        let Some(name) = type_decl_name(declaration) else {
            return;
        };
        let origin = normalize_path(origin);
        if !seen.insert((origin.clone(), name.to_string())) {
            return;
        }
        declarations.push(LocatedTypeDeclaration {
            file: origin.clone(),
            declaration: declaration.clone(),
        });

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
        declarations: &mut Vec<LocatedTypeDeclaration>,
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

    /// Resolve a type name in its declaring module's typing context. This
    /// includes private signature dependencies and does not grant import access.
    pub fn type_declaration_visible_from(
        &self,
        file: &Path,
        name: &str,
    ) -> Option<LocatedTypeDeclaration> {
        let (declaration, file) =
            self.find_visible_type_declaration(file, name, &mut HashSet::new())?;
        Some(LocatedTypeDeclaration { file, declaration })
    }

    /// Bind referenced type names to defining modules without flattening the
    /// declaration's private scope into its importing module.
    pub fn type_declaration_bindings(
        &self,
        site: &LocatedTypeDeclaration,
    ) -> BTreeMap<String, PathBuf> {
        let mut names = HashSet::new();
        collect_declaration_type_names(&site.declaration, &mut names);
        let declaration = match &site.declaration.node {
            Node::AttributedDecl { inner, .. } => inner.as_ref(),
            _ => &site.declaration,
        };
        let parameters = match &declaration.node {
            Node::TypeDecl { type_params, .. }
            | Node::StructDecl { type_params, .. }
            | Node::EnumDecl { type_params, .. }
            | Node::InterfaceDecl { type_params, .. } => type_params.as_slice(),
            _ => &[],
        };
        for parameter in parameters {
            names.remove(&parameter.name);
        }
        names
            .into_iter()
            .filter_map(|name| {
                self.type_declaration_visible_from(&site.file, &name)
                    .map(|target| (name, target.file))
            })
            .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(root: &Path, name: &str, source: &str) -> PathBuf {
        let path = root.join(name);
        std::fs::write(&path, source).unwrap();
        path
    }

    #[test]
    fn located_types_retain_recursive_origins_through_callable_facades() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let contracts = write_file(root, "contracts.harn",
            "type Recursive = {next?: Recursive, label: string}\npub type Request = {payload: Recursive}\n");
        write_file(root, "api.harn",
            "import { Request } from \"./contracts\"\npub fn run(request: Request) -> Request { request }\n");
        let facade = write_file(root, "facade.harn", "pub import { run } from \"./api\"\n");
        let entry = write_file(root, "entry.harn", "import { run } from \"./facade\"\n");
        let graph = crate::build(std::slice::from_ref(&entry));
        let sites = graph
            .imported_type_declaration_sites_for_file(&entry)
            .unwrap();
        assert_eq!(sites.len(), 2);
        assert!(sites
            .iter()
            .all(|site| site.file == normalize_path(&contracts)));
        let names: HashSet<_> = sites
            .iter()
            .filter_map(|site| type_decl_name(&site.declaration))
            .collect();
        assert_eq!(names, HashSet::from(["Recursive", "Request"]));
        let recursive = sites
            .iter()
            .find(|site| type_decl_name(&site.declaration) == Some("Recursive"))
            .unwrap();
        assert_eq!(
            graph.type_declaration_bindings(recursive),
            BTreeMap::from([("Recursive".to_string(), normalize_path(&contracts)),])
        );
        let request = sites
            .iter()
            .find(|site| type_decl_name(&site.declaration) == Some("Request"))
            .unwrap();
        assert_eq!(
            request.declaration.span,
            graph
                .export_definition_of(&contracts, "Request")
                .unwrap()
                .span
        );
        assert_eq!(
            graph.imported_type_declarations_for_file(&entry).unwrap(),
            sites
                .into_iter()
                .map(|site| site.declaration)
                .collect::<Vec<_>>()
        );
        assert!(graph.export_definition_of(&facade, "Recursive").is_none());
    }

    #[test]
    fn private_same_named_types_bind_in_each_declaration_scope() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let left = write_file(
            root,
            "left.harn",
            "type Details = {left_only: int}\npub type Left = {details: Details}\n",
        );
        let right = write_file(
            root,
            "right.harn",
            "type Details = {right_only: string}\npub type Right = {details: Details}\n",
        );
        let entry = write_file(
            root,
            "entry.harn",
            "import { Left } from \"./left\"\nimport { Right } from \"./right\"\n",
        );
        let graph = crate::build(std::slice::from_ref(&entry));
        let sites = graph
            .imported_type_declaration_sites_for_file(&entry)
            .unwrap();
        assert_eq!(
            sites
                .iter()
                .filter(|site| type_decl_name(&site.declaration) == Some("Details"))
                .count(),
            2
        );
        for (name, origin) in [("Left", &left), ("Right", &right)] {
            let site = sites
                .iter()
                .find(|site| type_decl_name(&site.declaration) == Some(name))
                .unwrap();
            assert_eq!(
                graph.type_declaration_bindings(site),
                BTreeMap::from([("Details".to_string(), normalize_path(origin)),])
            );
        }
        let left_details = graph
            .type_declaration_visible_from(&left, "Details")
            .unwrap();
        let right_details = graph
            .type_declaration_visible_from(&right, "Details")
            .unwrap();
        assert_ne!(left_details.file, right_details.file);
        assert_ne!(
            left_details.declaration.node,
            right_details.declaration.node
        );
        assert!(graph
            .type_declaration_visible_from(&entry, "Details")
            .is_none());
    }

    #[test]
    fn declaration_bindings_omit_generic_parameters_even_when_a_private_type_shadows_them() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let models = write_file(root, "models.harn",
            "type T = {unrelated: bool}\ntype Marker = {present: string}\npub struct Box<T> { value: T, count: int, marker: Marker }\n");
        let entry = write_file(root, "entry.harn", "import { Box } from \"./models\"\n");
        let graph = crate::build(std::slice::from_ref(&entry));
        let site = graph.type_declaration_visible_from(&models, "Box").unwrap();
        assert!(graph.type_declaration_visible_from(&models, "T").is_some());
        assert!(graph
            .type_declaration_visible_from(&models, "int")
            .is_none());
        assert_eq!(
            graph.type_declaration_bindings(&site),
            BTreeMap::from([("Marker".to_string(), normalize_path(&models)),])
        );
    }

    #[test]
    fn unresolved_import_cannot_become_an_empty_located_type_census() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let good = write_file(root, "good.harn", "pub type Known = {value: string}\n");
        let entry = write_file(
            root,
            "entry.harn",
            "import { Known } from \"./good\"\nimport \"./missing\"\n",
        );
        let graph = crate::build(std::slice::from_ref(&entry));
        assert!(graph
            .type_declaration_visible_from(&good, "Known")
            .is_some());
        assert!(graph
            .imported_type_declaration_sites_for_file(&entry)
            .is_none());
        assert!(graph.imported_type_declarations_for_file(&entry).is_none());
    }
}
