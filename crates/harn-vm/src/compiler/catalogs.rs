use std::collections::HashSet;

use harn_parser::{Node, SNode};

use super::{Compiler, EnumCatalogSnapshot};

impl Compiler {
    pub(super) fn collect_imported_enum_candidates(&mut self, program: &[SNode]) {
        if self.imported_enum_candidates_authoritative {
            return;
        }
        if !harn_parser::visit::contains_identifier_receiver_access(program) {
            return;
        }
        let imported_names: HashSet<&str> = program
            .iter()
            .filter_map(|node| match &node.node {
                Node::SelectiveImport { names, .. } => Some(names),
                _ => None,
            })
            .flatten()
            .map(String::as_str)
            .collect();
        if imported_names.is_empty() {
            return;
        }

        // A selective import is not necessarily an enum. Keep ordinary
        // imports out of every nested compiler's lexical catalog unless the
        // source actually uses the syntax whose meaning depends on enum
        // resolution (`ImportedEnum.Variant`). Wildcard imports are resolved
        // by the module graph and supplied separately by the artifact path.
        let mut used_imports = HashSet::new();
        harn_parser::visit::walk_program(program, &mut |node| {
            let object = match &node.node {
                Node::PropertyAccess { object, .. }
                | Node::OptionalPropertyAccess { object, .. }
                | Node::MethodCall { object, .. }
                | Node::OptionalMethodCall { object, .. } => object,
                _ => return,
            };
            if let Node::Identifier(name) = &object.node {
                if imported_names.contains(name.as_str()) {
                    used_imports.insert(name.clone());
                }
            }
        });
        self.imported_enum_candidates.extend(used_imports);
    }

    pub(super) fn is_known_enum_name(&self, name: &str) -> bool {
        self.enum_names.contains(name) || self.imported_enum_candidates.contains(name)
    }

    pub(super) fn enum_catalog_snapshot(&self) -> EnumCatalogSnapshot {
        EnumCatalogSnapshot {
            names: self.enum_names.clone(),
            variant_owners: self.enum_variant_owners.clone(),
        }
    }

    pub(super) fn restore_enum_catalog(&mut self, snapshot: EnumCatalogSnapshot) {
        self.enum_names = snapshot.names;
        self.enum_variant_owners = snapshot.variant_owners;
    }

    /// Register one enum in the active lexical catalog. A same-named inner
    /// declaration shadows the outer enum, so remove the old owner's variants
    /// before inserting the replacement. Scope snapshots restore them later.
    pub(super) fn register_enum_decl(&mut self, name: &str, variants: &[harn_parser::EnumVariant]) {
        for owners in self.enum_variant_owners.values_mut() {
            owners.retain(|owner| owner != name);
        }
        self.enum_variant_owners
            .retain(|_, owners| !owners.is_empty());
        self.enum_names.insert(name.to_string());
        for variant in variants {
            let owners = self
                .enum_variant_owners
                .entry(variant.name.clone())
                .or_default();
            if !owners.iter().any(|owner| owner == name) {
                owners.push(name.to_string());
            }
        }
    }

    /// Seed declarations that the typechecker places in the module scope:
    /// direct top-level nodes plus direct pipeline-body nodes. Only top-level
    /// declarations remain fixed at their pre-pass value. Pipeline-body enums
    /// are revisited by `compile_node` so same-name declarations shadow in
    /// source order, matching the typechecker's pipeline child scope. Nested
    /// callable and block declarations are likewise registered in source order.
    pub(super) fn collect_module_enum_catalog(&mut self, program: &[SNode]) {
        for nodes in harn_parser::lexical::module_scope_node_slices(program) {
            for node in nodes {
                let declaration = match &node.node {
                    Node::AttributedDecl { inner, .. } => inner.as_ref(),
                    _ => node,
                };
                if let Node::EnumDecl { name, variants, .. } = &declaration.node {
                    self.register_enum_decl(name, variants);
                }
            }
        }

        for node in program {
            let declaration = match &node.node {
                Node::AttributedDecl { inner, .. } => inner.as_ref(),
                _ => node,
            };
            if matches!(&declaration.node, Node::EnumDecl { .. }) {
                self.predeclared_enum_declarations
                    .insert((declaration.span.start, declaration.span.end));
            }
        }
    }

    /// Seed the built-in `Result` enum's variants into the owner map.
    pub(super) fn seed_builtin_variant_owners(
        owners: &mut std::collections::HashMap<String, Vec<String>>,
    ) {
        for variant in ["Ok", "Err"] {
            let entry = owners.entry(variant.to_string()).or_default();
            if !entry.iter().any(|owner| owner == "Result") {
                entry.push("Result".to_string());
            }
        }
    }

    pub(super) fn collect_struct_layouts(
        nodes: &[SNode],
        layouts: &mut std::collections::HashMap<String, Vec<String>>,
    ) {
        for sn in nodes {
            match &sn.node {
                Node::StructDecl { name, fields, .. } => {
                    layouts.insert(
                        name.clone(),
                        fields.iter().map(|field| field.name.clone()).collect(),
                    );
                }
                Node::Pipeline { body, .. }
                | Node::FnDecl { body, .. }
                | Node::ToolDecl { body, .. } => Self::collect_struct_layouts(body, layouts),
                Node::SkillDecl { fields, .. } => {
                    for (_, value) in fields {
                        Self::collect_struct_layouts(std::slice::from_ref(value), layouts);
                    }
                }
                Node::EvalPackDecl {
                    fields,
                    body,
                    summarize,
                    ..
                } => {
                    for (_, value) in fields {
                        Self::collect_struct_layouts(std::slice::from_ref(value), layouts);
                    }
                    Self::collect_struct_layouts(body, layouts);
                    if let Some(summary_body) = summarize {
                        Self::collect_struct_layouts(summary_body, layouts);
                    }
                }
                Node::Block(stmts) => Self::collect_struct_layouts(stmts, layouts),
                Node::AttributedDecl { inner, .. } => {
                    Self::collect_struct_layouts(std::slice::from_ref(inner), layouts);
                }
                _ => {}
            }
        }
    }

    pub(super) fn collect_interface_methods(
        nodes: &[SNode],
        interfaces: &mut std::collections::HashMap<String, Vec<String>>,
    ) {
        for sn in nodes {
            match &sn.node {
                Node::InterfaceDecl { name, methods, .. } => {
                    interfaces.insert(
                        name.clone(),
                        methods.iter().map(|method| method.name.clone()).collect(),
                    );
                }
                Node::Pipeline { body, .. }
                | Node::FnDecl { body, .. }
                | Node::ToolDecl { body, .. } => {
                    Self::collect_interface_methods(body, interfaces);
                }
                Node::SkillDecl { fields, .. } => {
                    for (_, value) in fields {
                        Self::collect_interface_methods(std::slice::from_ref(value), interfaces);
                    }
                }
                Node::EvalPackDecl {
                    fields,
                    body,
                    summarize,
                    ..
                } => {
                    for (_, value) in fields {
                        Self::collect_interface_methods(std::slice::from_ref(value), interfaces);
                    }
                    Self::collect_interface_methods(body, interfaces);
                    if let Some(summary_body) = summarize {
                        Self::collect_interface_methods(summary_body, interfaces);
                    }
                }
                Node::Block(stmts) => Self::collect_interface_methods(stmts, interfaces),
                Node::AttributedDecl { inner, .. } => {
                    Self::collect_interface_methods(std::slice::from_ref(inner), interfaces);
                }
                _ => {}
            }
        }
    }
}
