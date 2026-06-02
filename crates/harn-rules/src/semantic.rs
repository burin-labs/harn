//! Harn-only semantic capture enrichment.
//!
//! The rule engine is grammar-agnostic by default. This module adds a narrow
//! Harn pass that annotates capture bindings with local declaration identity
//! and simple type labels where the tree-sitter syntax gives us enough static
//! information.

use harn_hostlib::ast::{api, Language};
use tree_sitter::Node;

use crate::engine::{BindingMetadata, ResolvedBinding, RuleMatch, Span};

#[derive(Clone, Copy)]
struct Scope {
    start: usize,
    end: usize,
}

impl Scope {
    fn contains(self, byte: usize) -> bool {
        self.start <= byte && byte <= self.end
    }

    fn of(node: Node<'_>) -> Self {
        Scope {
            start: node.start_byte(),
            end: node.end_byte(),
        }
    }
}

#[derive(Clone)]
struct Decl {
    name: String,
    kind: &'static str,
    name_span: Span,
    scope: Scope,
    hoisted: bool,
    ty: Option<String>,
    return_ty: Option<String>,
}

impl Decl {
    fn resolved(&self) -> ResolvedBinding {
        ResolvedBinding {
            id: format!(
                "{}:{}@{}:{}",
                self.kind,
                self.name,
                self.name_span.start_row + 1,
                self.name_span.start_col + 1
            ),
            name: self.name.clone(),
            kind: self.kind.to_string(),
            span: self.name_span,
        }
    }
}

struct HarnSemanticIndex<'src> {
    source: &'src str,
    decls: Vec<Decl>,
}

impl<'src> HarnSemanticIndex<'src> {
    fn build(source: &'src str, root: Node<'_>) -> Self {
        let mut index = HarnSemanticIndex {
            source,
            decls: Vec::new(),
        };
        index.collect(
            root,
            Scope {
                start: 0,
                end: source.len(),
            },
        );
        index
    }

    fn enrich_binding(&self, root: Node<'_>, binding: &mut crate::engine::Binding) {
        let Some(node) = exact_named_node(root, binding.span.start_byte, binding.span.end_byte)
        else {
            return;
        };

        let mut metadata = BindingMetadata::default();
        if let Some(decl) = self.resolve_capture_node(node) {
            metadata.resolved = Some(decl.resolved());
            metadata.ty = match node.kind() {
                "call_expression" | "generic_call_expression" => decl.return_ty.clone(),
                _ => decl.ty.clone(),
            };
        }
        if metadata.ty.is_none() {
            metadata.ty = self.infer_node_type(node);
        }
        binding.metadata = metadata;
    }

    fn resolve_capture_node(&self, node: Node<'_>) -> Option<&Decl> {
        let ident = resolvable_identifier(node)?;
        let name = self.text(ident);
        if let Some(decl) = self.decl_at(ident) {
            return Some(decl);
        }
        self.resolve_name(name, ident.start_byte())
    }

    fn decl_at(&self, ident: Node<'_>) -> Option<&Decl> {
        self.decls.iter().find(|decl| {
            decl.name_span.start_byte == ident.start_byte()
                && decl.name_span.end_byte == ident.end_byte()
        })
    }

    fn resolve_name(&self, name: &str, byte: usize) -> Option<&Decl> {
        self.decls
            .iter()
            .filter(|decl| {
                decl.name == name
                    && decl.scope.contains(byte)
                    && decl.name_span.start_byte != byte
                    && (decl.hoisted || decl.name_span.start_byte <= byte)
            })
            .max_by_key(|decl| (decl.scope.start, decl.name_span.start_byte))
    }

    fn infer_node_type(&self, node: Node<'_>) -> Option<String> {
        match node.kind() {
            "integer_literal" => Some("int".into()),
            "float_literal" => Some("float".into()),
            "string_literal"
            | "raw_string_literal"
            | "multiline_string_literal"
            | "interpolated_string" => Some("string".into()),
            "true" | "false" => Some("bool".into()),
            "nil" => Some("nil".into()),
            "list_literal" => Some("list".into()),
            "dict_literal" => Some("dict".into()),
            "struct_construct" => node
                .child_by_field_name("type")
                .or_else(|| node.child_by_field_name("name"))
                .map(|name| self.text(name).to_string())
                .or_else(|| Some("dict".into())),
            "identifier" => self
                .resolve_name(self.text(node), node.start_byte())
                .and_then(|decl| decl.ty.clone()),
            "call_expression" | "generic_call_expression" => self
                .resolve_capture_node(node)
                .and_then(|decl| decl.return_ty.clone()),
            _ => None,
        }
    }

    fn collect(&mut self, node: Node<'_>, scope: Scope) {
        match node.kind() {
            "source_file" => self.collect_children(
                node,
                Scope {
                    start: 0,
                    end: self.source.len(),
                },
            ),
            "block" => {
                let block_scope = Scope::of(node);
                self.collect_children(node, block_scope);
            }
            "fn_declaration" => self.collect_callable(node, scope, "fn"),
            "pipeline_declaration" => self.collect_callable(node, scope, "pipeline"),
            "tool_declaration" => self.collect_callable(node, scope, "tool"),
            "struct_declaration" => self.add_named_decl(node, scope, "struct", true, None, None),
            "type_declaration" => {
                let ty = node
                    .child_by_field_name("type")
                    .map(|ty| self.type_text(ty));
                self.add_named_decl(node, scope, "type", true, ty, None);
            }
            "let_binding" | "var_binding" | "const_binding" => self.collect_binding(node, scope),
            "for_statement" => self.collect_for_statement(node, scope),
            "select_case" => self.collect_select_case(node, scope),
            "try_expression" => self.collect_try_expression(node, scope),
            _ => self.collect_children(node, scope),
        }
    }

    fn collect_callable(&mut self, node: Node<'_>, scope: Scope, kind: &'static str) {
        let body = callable_body(node);
        let body_scope = body.map(Scope::of).unwrap_or_else(|| Scope::of(node));
        let (ty, return_ty) = self.callable_types(node);
        self.add_named_decl(node, scope, kind, true, ty, return_ty);
        self.collect_params(node, body_scope);
        if let Some(body) = body {
            self.collect(body, body_scope);
        } else {
            self.collect_children(node, body_scope);
        }
    }

    fn collect_binding(&mut self, node: Node<'_>, scope: Scope) {
        let explicit_ty = node
            .child_by_field_name("type")
            .map(|ty| self.type_text(ty));
        let inferred_ty = explicit_ty.or_else(|| {
            node.child_by_field_name("value")
                .and_then(|value| self.infer_node_type(value))
        });
        let kind = match node.kind() {
            "var_binding" => "var",
            "const_binding" => "const",
            _ => "let",
        };
        let binding_scope = Scope {
            start: node.end_byte(),
            end: scope.end,
        };
        if let Some(pattern) = node.child_by_field_name("name") {
            self.add_pattern_decls(pattern, binding_scope, kind, false, inferred_ty);
        }
        if let Some(value) = node.child_by_field_name("value") {
            self.collect(value, scope);
        }
    }

    fn collect_for_statement(&mut self, node: Node<'_>, scope: Scope) {
        if let Some(iterable) = node.child_by_field_name("iterable") {
            self.collect(iterable, scope);
        }
        let body = node.child_by_field_name("body");
        let body_scope = body.map(Scope::of).unwrap_or(scope);
        if let Some(pattern) = node.child_by_field_name("variable") {
            self.add_pattern_decls(pattern, body_scope, "let", false, None);
        }
        if let Some(body) = body {
            self.collect(body, body_scope);
        }
    }

    fn collect_select_case(&mut self, node: Node<'_>, scope: Scope) {
        if let Some(channel) = node.child_by_field_name("channel") {
            self.collect(channel, scope);
        }
        let body = node.child_by_field_name("body");
        let body_scope = body.map(Scope::of).unwrap_or(scope);
        if let Some(variable) = node.child_by_field_name("variable") {
            self.add_identifier_decl(variable, body_scope, "let", false, None, None);
        }
        if let Some(body) = body {
            self.collect(body, body_scope);
        }
    }

    fn collect_try_expression(&mut self, node: Node<'_>, scope: Scope) {
        if let Some(body) = node.child_by_field_name("body") {
            self.collect(body, Scope::of(body));
        }
        let handler = node.child_by_field_name("handler");
        let handler_scope = handler.map(Scope::of).unwrap_or(scope);
        if let Some(error_var) = node.child_by_field_name("error_var") {
            let error_ty = node
                .child_by_field_name("error_type")
                .map(|ty| self.type_text(ty));
            self.add_identifier_decl(error_var, handler_scope, "let", false, error_ty, None);
        }
        if let Some(handler) = handler {
            self.collect(handler, handler_scope);
        }
        if let Some(finalizer) = node.child_by_field_name("finalizer") {
            self.collect(finalizer, Scope::of(finalizer));
        }
    }

    fn collect_params(&mut self, callable: Node<'_>, scope: Scope) {
        let Some(params) = direct_child_kind(callable, "parameter_list") else {
            return;
        };
        let mut cursor = params.walk();
        for param in params.named_children(&mut cursor) {
            if param.kind() != "typed_parameter" {
                continue;
            }
            let ty = param
                .child_by_field_name("type")
                .map(|ty| self.type_text(ty));
            if let Some(name) = param.child_by_field_name("name") {
                self.add_identifier_decl(name, scope, "param", false, ty, None);
            }
            if let Some(default) = param.child_by_field_name("default") {
                self.collect(default, scope);
            }
        }
    }

    fn callable_types(&self, callable: Node<'_>) -> (Option<String>, Option<String>) {
        let params = direct_child_kind(callable, "parameter_list")
            .map(|list| {
                let mut out = Vec::new();
                let mut cursor = list.walk();
                for param in list.named_children(&mut cursor) {
                    if param.kind() == "typed_parameter" {
                        out.push(
                            param
                                .child_by_field_name("type")
                                .map(|ty| self.type_text(ty))
                                .unwrap_or_else(|| "_".into()),
                        );
                    }
                }
                out
            })
            .unwrap_or_default();
        let return_ty = return_type_annotation(callable).map(|ty| self.type_text(ty));
        let ty = Some(format!(
            "fn({}) -> {}",
            params.join(", "),
            return_ty.clone().unwrap_or_else(|| "unknown".into())
        ));
        (ty, return_ty)
    }

    fn add_named_decl(
        &mut self,
        node: Node<'_>,
        scope: Scope,
        kind: &'static str,
        hoisted: bool,
        ty: Option<String>,
        return_ty: Option<String>,
    ) {
        if let Some(name) = node.child_by_field_name("name") {
            self.add_identifier_decl(name, scope, kind, hoisted, ty, return_ty);
        }
    }

    fn add_pattern_decls(
        &mut self,
        pattern: Node<'_>,
        scope: Scope,
        kind: &'static str,
        hoisted: bool,
        ty: Option<String>,
    ) {
        if pattern.kind() == "identifier" {
            self.add_identifier_decl(pattern, scope, kind, hoisted, ty, None);
            return;
        }
        for_identifier_descendants(pattern, &mut |ident| {
            self.add_identifier_decl(ident, scope, kind, hoisted, ty.clone(), None);
        });
    }

    fn add_identifier_decl(
        &mut self,
        ident: Node<'_>,
        scope: Scope,
        kind: &'static str,
        hoisted: bool,
        ty: Option<String>,
        return_ty: Option<String>,
    ) {
        let name = self.text(ident);
        if name == "_" {
            return;
        }
        self.decls.push(Decl {
            name: name.to_string(),
            kind,
            name_span: Span::of(ident),
            scope,
            hoisted,
            ty,
            return_ty,
        });
    }

    fn collect_children(&mut self, node: Node<'_>, scope: Scope) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.collect(child, scope);
        }
    }

    fn type_text(&self, node: Node<'_>) -> String {
        self.text(node).trim().to_string()
    }

    fn text(&self, node: Node<'_>) -> &'src str {
        &self.source[node.start_byte()..node.end_byte()]
    }
}

pub(crate) fn enrich_harn_matches(source: &str, matches: &mut [RuleMatch]) -> Result<(), String> {
    let tree = api::parse_tree(source, Language::Harn).map_err(|err| err.to_string())?;
    let root = tree.root_node();
    let index = HarnSemanticIndex::build(source, root);
    for m in matches {
        for binding in m.bindings.values_mut() {
            index.enrich_binding(root, binding);
        }
    }
    Ok(())
}

fn callable_body(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("body")
        .or_else(|| direct_child_kind(node, "block"))
}

fn return_type_annotation<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    node.child_by_field_name("return_type").or_else(|| {
        let mut cursor = node.walk();
        let found = node
            .named_children(&mut cursor)
            .find(|child| child.kind() == "type_annotation");
        found
    })
}

fn resolvable_identifier<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    match node.kind() {
        "identifier" => Some(node),
        "call_expression" | "generic_call_expression" => node
            .child_by_field_name("function")
            .filter(|function| function.kind() == "identifier"),
        _ => None,
    }
}

fn direct_child_kind<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == kind);
    found
}

fn exact_named_node<'tree>(node: Node<'tree>, start: usize, end: usize) -> Option<Node<'tree>> {
    if start < node.start_byte() || end > node.end_byte() {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.start_byte() <= start && end <= child.end_byte() {
            if let Some(found) = exact_named_node(child, start, end) {
                return Some(found);
            }
        }
    }
    if node.start_byte() == start && node.end_byte() == end {
        return Some(node);
    }
    None
}

fn for_identifier_descendants<'tree>(node: Node<'tree>, f: &mut impl FnMut(Node<'tree>)) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "identifier" {
            f(child);
        }
        for_identifier_descendants(child, f);
    }
}
