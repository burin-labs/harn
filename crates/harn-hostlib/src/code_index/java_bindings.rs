//! Java member ownership from explicit source types, without guessing through
//! inheritance, inferred variables, or expression receivers.

use tree_sitter::{Node, Tree};

use crate::ast::java::{direct_members, is_object_method, type_parameter_name};

#[derive(Clone, Copy)]
enum BindingType<T> {
    Absent,
    Unknown,
    Known(T),
}

impl<T> BindingType<T> {
    fn from_optional(value: Option<T>) -> Self {
        value.map_or(Self::Unknown, Self::Known)
    }

    fn known(self) -> Option<T> {
        if let Self::Known(value) = self {
            Some(value)
        } else {
            None
        }
    }

    fn and_then<U>(self, resolve: impl FnOnce(T) -> Option<U>) -> BindingType<U> {
        match self {
            Self::Absent => BindingType::Absent,
            Self::Unknown => BindingType::Unknown,
            Self::Known(value) => BindingType::from_optional(resolve(value)),
        }
    }
}

pub(super) struct JavaBindings<'a> {
    source: &'a str,
    package: String,
    imports: Vec<(String, String)>,
    wildcard_import: bool,
    unresolved_syntax: bool,
}

impl<'a> JavaBindings<'a> {
    pub fn new(tree: &'a Tree, source: &'a str) -> Self {
        let root = tree.root_node();
        let mut package = String::new();
        let mut imports = Vec::new();
        let mut wildcard_import = false;
        for node in children(root) {
            match node.kind() {
                "package_declaration" => {
                    if let Some(name) = children(node)
                        .find(|child| matches!(child.kind(), "identifier" | "scoped_identifier"))
                    {
                        package = text(name, source).to_string();
                    }
                }
                "import_declaration" => {
                    let raw = text(node, source);
                    if raw.starts_with("import static ") {
                        continue;
                    }
                    if raw.contains('*') {
                        wildcard_import = true;
                    } else if let Some(name) = children(node)
                        .find(|child| matches!(child.kind(), "identifier" | "scoped_identifier"))
                    {
                        let full = text(name, source).to_string();
                        let short = full.rsplit('.').next().unwrap_or(&full).to_string();
                        imports.push((short, full));
                    }
                }
                _ => {}
            }
        }
        Self {
            source,
            package,
            imports,
            wildcard_import,
            unresolved_syntax: root.has_error()
                || source.contains("\\u")
                || descendants(root).any(|node| {
                    matches!(
                        node.kind(),
                        "type_pattern"
                            | "record_pattern"
                            | "pattern"
                            | "guarded_pattern"
                            | "resource"
                    ) || (node.kind() == "instanceof_expression"
                        && node.child_by_field_name("name").is_some())
                }),
        }
    }

    pub fn declaration_owner(&self, node: Node<'_>) -> Option<String> {
        if matches!(
            node.kind(),
            "constructor_declaration" | "compact_constructor_declaration"
        ) {
            return None;
        }
        if node.kind() == "method_declaration" {
            let name = text(node.child_by_field_name("name")?, self.source);
            if is_object_method(name) {
                return None;
            }
            if descendants(node).any(|child| {
                matches!(child.kind(), "annotation" | "marker_annotation")
                    && child.child_by_field_name("name").is_some_and(|name| {
                        matches!(text(name, self.source), "Override" | "java.lang.Override")
                    })
            }) {
                return None;
            }
        }
        let mut names = Vec::new();
        let mut outer = node.parent();
        while let Some(parent) = outer {
            if is_type(parent) {
                if parent.kind() == "interface_declaration" || has_supertypes(parent) {
                    return None;
                }
                names.push(text(parent.child_by_field_name("name")?, self.source));
            } else if matches!(
                parent.kind(),
                "object_creation_expression" | "enum_constant"
            ) {
                return None;
            }
            outer = parent.parent();
        }
        names.reverse();
        (!names.is_empty()).then(|| self.in_package(&names.join(".")))
    }

    pub fn method_has_closed_dispatch(&self, method: Node<'_>) -> bool {
        if has_modifier(method, "private") {
            return true;
        }
        let mut outer = method.parent();
        while let Some(node) = outer {
            if is_type(node) {
                return has_modifier(node, "final");
            }
            outer = node.parent();
        }
        false
    }

    pub fn receiver_owner(&self, call: Node<'_>) -> Option<String> {
        if self.unresolved_syntax {
            return None;
        }
        let Some(receiver) = call.child_by_field_name("object") else {
            return self.declaration_owner(call);
        };
        match receiver.kind() {
            "this" => self.declaration_owner(call),
            "identifier" => {
                let name = text(receiver, self.source);
                self.variable_type(call, name, false).known()
            }
            "field_access" => {
                let object = receiver.child_by_field_name("object")?;
                if object.kind() != "this" {
                    return None;
                }
                self.variable_type(
                    call,
                    text(receiver.child_by_field_name("field")?, self.source),
                    true,
                )
                .known()
            }
            _ => None,
        }
    }

    fn in_package(&self, name: &str) -> String {
        if self.package.is_empty() {
            name.to_string()
        } else {
            format!("{}.{name}", self.package)
        }
    }

    fn resolve_type(&self, ty: Node<'_>, at: Node<'_>) -> Option<String> {
        let ty = if ty.kind() == "generic_type" {
            ty.named_child(0)?
        } else {
            ty
        };
        if !matches!(
            ty.kind(),
            "type_identifier" | "scoped_type_identifier" | "identifier"
        ) {
            return None;
        }
        let name = text(ty, self.source);
        let first = name.split('.').next()?;
        if name == "var" || name.contains('\\') || self.type_parameter(at, first) {
            return None;
        }
        let mut outer = Some(at);
        while let Some(scope) = outer {
            if is_type(scope) {
                if text(scope.child_by_field_name("name")?, self.source) == first {
                    if has_supertypes(scope) {
                        return None;
                    }
                    return self
                        .declaration_owner(scope)
                        .map(|owner| format!("{owner}.{name}"))
                        .or_else(|| {
                            scope
                                .parent()
                                .filter(|parent| parent.kind() == "program")
                                .map(|_| self.in_package(name))
                        });
                }
                if let Some(body) = scope.child_by_field_name("body") {
                    if direct_members(body).into_iter().any(|child| {
                        is_type(child)
                            && child
                                .child_by_field_name("name")
                                .is_some_and(|id| text(id, self.source) == first)
                    }) {
                        return None;
                    }
                }
                if has_supertypes(scope) {
                    return None;
                }
            }
            if is_local_scope(scope)
                && local_declarations(scope).into_iter().any(|child| {
                    is_type(child)
                        && child.start_byte() < at.start_byte()
                        && child
                            .child_by_field_name("name")
                            .is_some_and(|id| text(id, self.source) == first)
                })
            {
                return None;
            }
            outer = scope.parent();
        }
        let imported: Vec<_> = self
            .imports
            .iter()
            .filter(|(short, _)| short == first)
            .collect();
        if imported.len() == 1 {
            return Some(format!("{}{}", imported[0].1, name.strip_prefix(first)?));
        }
        if !imported.is_empty() {
            return None;
        }
        if name.contains('.') {
            // A dotted source name may start with a relative type, not a package.
            return None;
        }
        if self.wildcard_import {
            return None;
        }
        Some(self.in_package(name))
    }

    fn type_parameter(&self, at: Node<'_>, name: &str) -> bool {
        let mut outer = Some(at);
        while let Some(node) = outer {
            if let Some(parameters) = node.child_by_field_name("type_parameters") {
                if descendants(parameters).any(|parameter| {
                    parameter.kind() == "type_parameter"
                        && type_parameter_name(parameter)
                            .is_some_and(|id| text(id, self.source) == name)
                }) {
                    return true;
                }
            }
            outer = node.parent();
        }
        false
    }

    fn variable_type(&self, at: Node<'_>, name: &str, fields_only: bool) -> BindingType<String> {
        let mut outer = at.parent();
        while let Some(scope) = outer {
            if matches!(scope.kind(), "object_creation_expression" | "enum_constant") {
                return BindingType::Unknown;
            }
            if is_type(scope) {
                let Some(body) = scope.child_by_field_name("body") else {
                    return BindingType::Absent;
                };
                for field in direct_members(body) {
                    if field.kind() == "field_declaration" {
                        let ty = self.declared_type(field, name);
                        if !matches!(ty, BindingType::Absent) {
                            return ty.and_then(|ty| self.resolve_type(ty, at));
                        }
                    }
                }
                // Captured outer-instance and inherited fields need a semantic resolver.
                return if has_supertypes(scope) {
                    BindingType::Unknown
                } else {
                    BindingType::Absent
                };
            }
            if !fields_only {
                if is_local_scope(scope) {
                    let mut locals = Vec::new();
                    for child in local_declarations(scope) {
                        if child.start_byte() >= at.start_byte() {
                            break;
                        }
                        if child.kind() == "local_variable_declaration" {
                            let ty = self.declared_type(child, name);
                            if !matches!(ty, BindingType::Absent) {
                                if matches!(
                                    scope.kind(),
                                    "switch_block" | "switch_block_statement_group"
                                ) {
                                    return BindingType::Unknown;
                                }
                                locals.push(ty);
                            }
                        }
                    }
                    if let Some(ty) = locals.last() {
                        return ty.and_then(|ty| self.resolve_type(ty, at));
                    }
                }
                if let Some(parameters) = scope.child_by_field_name("parameters") {
                    for parameter in children(parameters) {
                        if parameter.child_by_field_name("name").is_none()
                            && descendants(parameter).any(|node| {
                                node.kind() == "identifier" && text(node, self.source) == name
                            })
                        {
                            return BindingType::Unknown;
                        }
                        if parameter
                            .child_by_field_name("name")
                            .is_some_and(|id| text(id, self.source) == name)
                        {
                            if children(parameter).any(|child| child.kind() == "dimensions") {
                                return BindingType::Unknown;
                            }
                            return BindingType::from_optional(
                                parameter
                                    .child_by_field_name("type")
                                    .and_then(|ty| self.resolve_type(ty, at)),
                            );
                        }
                    }
                }
                if matches!(
                    scope.kind(),
                    "lambda_expression" | "catch_clause" | "enhanced_for_statement"
                ) {
                    // These scopes can introduce bindings not covered by declaration fields above.
                    if descendants(scope).any(|node| {
                        node.start_byte() < at.start_byte()
                            && node.kind() == "identifier"
                            && text(node, self.source) == name
                    }) {
                        return BindingType::Unknown;
                    }
                }
            }
            outer = scope.parent();
        }
        BindingType::Absent
    }

    fn declared_type<'n>(&self, declaration: Node<'n>, name: &str) -> BindingType<Node<'n>> {
        children(declaration)
            .find(|child| {
                child.kind() == "variable_declarator"
                    && child
                        .child_by_field_name("name")
                        .is_some_and(|id| text(id, self.source) == name)
            })
            .map_or(BindingType::Absent, |variable| {
                if children(variable).any(|child| child.kind() == "dimensions") {
                    BindingType::Unknown
                } else {
                    BindingType::from_optional(declaration.child_by_field_name("type"))
                }
            })
    }

    pub fn definition_owner(&self, node: Node<'_>) -> Option<String> {
        self.declaration_owner(node)
            .or_else(|| (is_type(node) && !self.package.is_empty()).then(|| self.package.clone()))
    }

    pub fn is_unresolved(&self) -> bool {
        self.unresolved_syntax
    }
}

fn is_type(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration"
    )
}

fn has_supertypes(node: Node<'_>) -> bool {
    node.child_by_field_name("superclass").is_some()
        || node.child_by_field_name("interfaces").is_some()
        || children(node).any(|child| child.kind() == "extends_interfaces")
}

fn has_modifier(node: Node<'_>, modifier: &str) -> bool {
    let Some(modifiers) = children(node).find(|child| child.kind() == "modifiers") else {
        return false;
    };
    let mut cursor = modifiers.walk();
    let found = modifiers
        .children(&mut cursor)
        .any(|child| child.kind() == modifier);
    found
}

fn is_local_scope(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "block"
            | "constructor_body"
            | "for_statement"
            | "switch_block"
            | "switch_block_statement_group"
    )
}

fn local_declarations(node: Node<'_>) -> Vec<Node<'_>> {
    children(node)
        .flat_map(|child| {
            if child.kind() == "switch_block_statement_group" {
                children(child).collect()
            } else {
                vec![child]
            }
        })
        .collect()
}

pub(super) fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    source.get(node.byte_range()).unwrap_or_default()
}

fn children(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .collect::<Vec<_>>()
        .into_iter()
}

fn descendants(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut pending = vec![node];
    std::iter::from_fn(move || {
        let next = pending.pop()?;
        pending.extend(children(next));
        Some(next)
    })
}
