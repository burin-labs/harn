use harn_parser::{Node, SNode};

use super::{Compiler, EnumCatalogSnapshot};

impl Compiler {
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

    /// Seed the full module type catalog into this compiler: enum names and
    /// variant owners (including the built-in `Result`), struct field layouts,
    /// interface method sets, and type aliases.
    ///
    /// Every path that compiles a module-scoped body must seed the same
    /// catalog so catalog-dependent lowerings resolve identically no matter
    /// which module later calls the compiled body: the top-level program
    /// (`compile`), pipeline callables (`compile_pipeline_callable`), and the
    /// per-function and `init` chunks emitted for imported modules
    /// (`compile_module_artifact`). The most visible dependent lowering is
    /// `EnumName.Variant(...)` construction, which only becomes a `BuildEnum`
    /// op when the enum is in `enum_names`. Without this seed an imported
    /// function body recompiled in a fresh, catalog-less compiler lowers enum
    /// construction to a bare variable load of the enum name and crashes at
    /// runtime with "Undefined variable" even though `check` passes (harn#5062).
    pub(crate) fn seed_module_catalog(&mut self, program: &[SNode]) {
        self.collect_module_enum_catalog(program);
        if self.enum_names.insert("Result".to_string()) {
            Self::seed_builtin_variant_owners(&mut self.enum_variant_owners);
        }
        Self::collect_struct_layouts(program, &mut self.struct_layouts);
        Self::collect_interface_methods(program, &mut self.interface_methods);
        self.collect_type_aliases(program);
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
