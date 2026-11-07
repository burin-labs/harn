//! Type namespace resolution inside the shared Java binding owner.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    children, descendants, has_supertypes, is_local_scope, is_type, local_declarations, text,
    BindingType, JavaBindings,
};
use crate::ast::java::type_path;
use crate::host_call::HostCallContext;
use tree_sitter::Node;

#[derive(Default)]
pub(crate) struct JavaTypeCatalog {
    declarations: BTreeMap<String, usize>,
    value_members: BTreeSet<String>,
}

impl JavaTypeCatalog {
    pub fn add(
        &mut self,
        bindings: &JavaBindings<'_>,
        root: Node<'_>,
        call: Option<&HostCallContext>,
    ) -> Result<(), String> {
        for node in descendants(root) {
            if call.is_some_and(|call| call.interrupted().is_some()) {
                return Err("Java type catalog was interrupted".into());
            }
            if is_type(node) {
                if let Some(identity) = bindings.type_identity(node) {
                    if !identity.starts_with('#') {
                        *self.declarations.entry(identity).or_default() += 1;
                    }
                }
            }
            if node.kind() == "method_declaration"
                || node.kind() == "variable_declarator"
                    && node
                        .parent()
                        .is_some_and(|parent| parent.kind() == "field_declaration")
            {
                if let Some(name) = node.child_by_field_name("name") {
                    if let Some(owner) = std::iter::successors(node.parent(), |node| node.parent())
                        .find(|node| is_type(*node))
                        .and_then(|owner| bindings.type_identity(owner))
                    {
                        self.value_members
                            .insert(format!("{owner}.{}", text(name, bindings.source)));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.declarations.contains_key(name)
    }

    pub fn unique(&self, name: &str) -> bool {
        self.declarations.get(name) == Some(&1)
    }
}

impl JavaBindings<'_> {
    pub fn type_name_capture(
        &self,
        node: Node<'_>,
        new_name: &str,
        catalog: &JavaTypeCatalog,
    ) -> Result<bool, String> {
        let mut enclosing = node.parent();
        while let Some(scope) = enclosing {
            if scope.kind() == "import_declaration" {
                if children(scope)
                    .find(|part| matches!(part.kind(), "identifier" | "scoped_identifier"))
                    .is_some_and(|name| name.end_byte() == node.end_byte())
                    && self
                        .imports
                        .iter()
                        .any(|import| !import.wildcard && import.short_name() == new_name)
                {
                    return Ok(true);
                }
                break;
            }
            if scope.kind() != "scoped_identifier" {
                break;
            }
            enclosing = scope.parent();
        }
        let parent = node.parent();
        if parent.is_some_and(|parent| {
            matches!(
                parent.kind(),
                "scoped_type_identifier" | "scoped_identifier" | "field_access"
            ) && children(parent)
                .last()
                .is_some_and(|last| last.id() == node.id())
                && children(parent)
                    .next()
                    .is_some_and(|first| first.id() != node.id())
        }) {
            return Ok(false);
        }
        if parent.is_some_and(|parent| {
            matches!(
                parent.kind(),
                "field_access" | "method_invocation" | "method_reference"
            )
        }) && !matches!(
            self.variable_type(node, new_name, false),
            BindingType::Absent
        ) {
            return Ok(true);
        }
        if self
            .imports
            .iter()
            .any(|import| !import.wildcard && import.short_name() == new_name)
        {
            return Ok(true);
        }
        let old_identity = self
            .type_occurrence(node, catalog)?
            .ok_or("the Java capture check has no type binding")?;
        let expected = old_identity.rsplit_once('.').map_or_else(
            || new_name.to_string(),
            |(owner, _)| format!("{owner}.{new_name}"),
        );
        if catalog.contains(&expected) {
            return Ok(true);
        }
        let binding =
            self.lookup_type_name(&[new_name.to_string()], node, catalog, Some(&expected))?;
        Ok(binding != expected)
    }

    pub fn type_shadow_conflicts(
        &self,
        node: Node<'_>,
        old_identity: &str,
        new_identity: &str,
        catalog: &JavaTypeCatalog,
    ) -> Result<bool, String> {
        let Some(actual) = self.type_occurrence(node, catalog)? else {
            return Ok(false);
        };
        if actual == new_identity {
            return Ok(true);
        }
        if node.parent().is_some_and(|parent| {
            matches!(
                parent.kind(),
                "scoped_type_identifier" | "scoped_identifier" | "field_access"
            )
        }) {
            return Ok(false);
        }
        let name = text(node, self.source);
        if self
            .imports
            .iter()
            .any(|import| !import.wildcard && import.short_name() == name)
        {
            return Ok(false);
        }
        let implicit = actual == format!("java.lang.{name}")
            || self
                .imports
                .iter()
                .any(|import| import.wildcard && actual == format!("{}.{name}", import.path));
        if !implicit {
            return Ok(false);
        }
        let old_name = old_identity
            .rsplit('.')
            .next()
            .ok_or("the selected Java type name is empty")?;
        Ok(self.resolve_type_name(&[old_name.to_string()], node, catalog)? == old_identity)
    }

    /// Unlike method dispatch, a type's declared identity does not depend on
    /// its supertypes. Local declarations have file-local lexical identities.
    pub fn type_identity(&self, node: Node<'_>) -> Option<String> {
        let mut names = Vec::new();
        let mut outer = Some(node);
        while let Some(scope) = outer {
            if is_type(scope) {
                names.push(text(scope.child_by_field_name("name")?, self.source));
            } else if is_local_scope(scope)
                || matches!(scope.kind(), "object_creation_expression" | "enum_constant")
            {
                return Some(format!("#local:{}:{}", scope.start_byte(), names.join(".")));
            }
            outer = scope.parent();
        }
        names.reverse();
        (!names.is_empty()).then(|| self.in_package(&names.join(".")))
    }

    pub fn type_occurrence(
        &self,
        node: Node<'_>,
        catalog: &JavaTypeCatalog,
    ) -> Result<Option<String>, String> {
        if self.unresolved_syntax {
            return Err("Java source contains unsupported or invalid binding syntax".into());
        }
        let Some(parent) = node.parent() else {
            return Ok(None);
        };
        let named = parent
            .child_by_field_name("name")
            .is_some_and(|name| name.id() == node.id());
        if named && is_type(parent) {
            return Ok(self.type_identity(parent));
        }
        if named
            && matches!(
                parent.kind(),
                "constructor_declaration" | "compact_constructor_declaration"
            )
        {
            let mut outer = parent.parent();
            while let Some(scope) = outer {
                if is_type(scope) {
                    return Ok(self.type_identity(scope));
                }
                outer = scope.parent();
            }
            return Err("the Java constructor has no declaring type".into());
        }
        if node.kind() == "type_identifier" {
            let mut context = node.parent();
            while let Some(scope) = context {
                if scope.kind() == "object_creation_expression" {
                    if scope.child(0).is_some_and(|first| first.kind() != "new") {
                        return Err("receiver-qualified Java inner construction requires compiler resolution".into());
                    }
                    break;
                }
                if !matches!(
                    scope.kind(),
                    "generic_type" | "scoped_type_identifier" | "type_arguments" | "annotated_type"
                ) {
                    break;
                }
                context = scope.parent();
            }
            if parent.kind() == "type_parameter"
                && super::type_parameter_name(parent).is_some_and(|name| name.id() == node.id())
            {
                return Ok(Some(format!("#parameter:{}", text(node, self.source))));
            }
            let prefix = if parent.kind() == "scoped_type_identifier"
                && children(parent)
                    .last()
                    .is_some_and(|last| last.id() == node.id())
            {
                parent
            } else {
                node
            };
            return self
                .resolve_type_name(&type_path(prefix, self.source)?, node, catalog)
                .map(Some);
        }
        let mut outer = Some(node);
        while let Some(scope) = outer {
            if scope.kind() == "import_declaration" {
                let name = children(scope)
                    .find(|child| matches!(child.kind(), "identifier" | "scoped_identifier"))
                    .ok_or("the Java import has no name")?;
                let names = descendants(name)
                    .filter(|part| {
                        part.kind() == "identifier" && part.end_byte() <= node.end_byte()
                    })
                    .map(|part| (part.start_byte(), text(part, self.source)))
                    .collect::<BTreeMap<_, _>>();
                let identity = names.into_values().collect::<Vec<_>>().join(".");
                if self
                    .imports
                    .iter()
                    .any(|import| import.is_static && import.path == identity)
                    && catalog.contains(&identity)
                    && catalog.value_members.contains(&identity)
                {
                    return Err(
                        "the Java static import binds both a type and a value member".into(),
                    );
                }
                return Ok(Some(identity));
            }
            if !matches!(scope.kind(), "identifier" | "scoped_identifier") {
                break;
            }
            outer = scope.parent();
        }
        if matches!(parent.kind(), "annotation" | "marker_annotation") && named {
            return self
                .resolve_type_name(&type_path(node, self.source)?, node, catalog)
                .map(Some);
        }
        if parent.kind() == "scoped_identifier" {
            let mut name = parent;
            while let Some(next) = name
                .parent()
                .filter(|next| next.kind() == "scoped_identifier")
            {
                name = next;
            }
            if name
                .parent()
                .is_some_and(|scope| matches!(scope.kind(), "annotation" | "marker_annotation"))
            {
                let prefix = prefix_path(name, node, self.source)?;
                return self.resolve_type_name(&prefix, node, catalog).map(Some);
            }
        }
        // An expression receiver names a type only if the value namespace did
        // not bind its first segment. Fields, parameters and inferred values
        // must never become a same-spelled class receiver.
        let receiver = if parent.kind() == "field_access" {
            if parent
                .child_by_field_name("object")
                .is_some_and(|object| object.id() == node.id())
            {
                Some(node)
            } else if parent
                .child_by_field_name("field")
                .is_some_and(|field| field.id() == node.id())
            {
                Some(parent)
            } else {
                None
            }
        } else if parent.kind() == "method_invocation"
            && parent
                .child_by_field_name("object")
                .is_some_and(|object| object.id() == node.id())
            || parent.kind() == "method_reference"
                && children(parent)
                    .next()
                    .is_some_and(|object| object.id() == node.id())
        {
            Some(node)
        } else {
            None
        };
        let Some(receiver) = receiver else {
            return Ok(None);
        };
        let parts = type_path(receiver, self.source)?;
        match self.variable_type(node, &parts[0], false) {
            BindingType::Known(_) => Ok(None),
            BindingType::Unknown => {
                Err("the Java expression receiver may be a value binding".into())
            }
            BindingType::Absent => self.resolve_type_name(&parts, node, catalog).map(Some),
        }
    }

    pub fn resolve_type_name(
        &self,
        parts: &[String],
        at: Node<'_>,
        catalog: &JavaTypeCatalog,
    ) -> Result<String, String> {
        self.lookup_type_name(parts, at, catalog, None)
    }

    fn lookup_type_name(
        &self,
        parts: &[String],
        at: Node<'_>,
        catalog: &JavaTypeCatalog,
        prospective: Option<&str>,
    ) -> Result<String, String> {
        let first = parts.first().ok_or("the Java type name is empty")?;
        let suffix = if parts.len() > 1 {
            format!(".{}", parts[1..].join("."))
        } else {
            String::new()
        };
        let mut outer = Some(at);
        while let Some(scope) = outer {
            if self.declares_type_parameter(scope, first) {
                return Ok(format!("#parameter:{first}{suffix}"));
            }
            if scope.kind() == "enum_constant"
                || scope.kind() == "object_creation_expression"
                    && children(scope).any(|child| child.kind() == "class_body")
            {
                return Err(
                    "anonymous Java member type bindings require compiler resolution".into(),
                );
            }
            if is_type(scope) {
                let within_body = scope.child_by_field_name("body").filter(|body| {
                    body.start_byte() <= at.start_byte() && at.end_byte() <= body.end_byte()
                });
                if let Some(body) = within_body {
                    let members = super::direct_members(body)
                        .into_iter()
                        .filter(|member| {
                            is_type(*member)
                                && member
                                    .child_by_field_name("name")
                                    .is_some_and(|name| text(name, self.source) == first)
                        })
                        .collect::<Vec<_>>();
                    if members.len() > 1 {
                        return Err("duplicate Java member types are ambiguous".into());
                    }
                    if let Some(member) = members.first() {
                        return self
                            .type_identity(*member)
                            .map(|identity| format!("{identity}{suffix}"))
                            .ok_or("the Java member owner is absent".into());
                    }
                }
                if within_body.is_some() && has_supertypes(scope) {
                    return Err(
                        "inherited Java member type bindings require compiler resolution".into(),
                    );
                }
                if scope
                    .child_by_field_name("name")
                    .is_some_and(|name| text(name, self.source) == first)
                {
                    return self
                        .type_identity(scope)
                        .map(|identity| format!("{identity}{suffix}"))
                        .ok_or("the Java type owner is absent".into());
                }
            }
            if is_local_scope(scope) {
                let locals = local_declarations(scope)
                    .into_iter()
                    .filter(|local| {
                        is_type(*local)
                            && local.start_byte() < at.start_byte()
                            && local
                                .child_by_field_name("name")
                                .is_some_and(|name| text(name, self.source) == first)
                    })
                    .collect::<Vec<_>>();
                if locals.len() > 1 {
                    return Err("duplicate Java local types are ambiguous".into());
                }
                if let Some(local) = locals.first() {
                    return self
                        .type_identity(*local)
                        .map(|identity| format!("{identity}{suffix}"))
                        .ok_or("the Java local type owner is absent".into());
                }
            }
            outer = scope.parent();
        }
        let imports = self
            .imports
            .iter()
            .filter(|import| !import.wildcard && import.short_name() == first)
            .collect::<Vec<_>>();
        if imports.len() > 1 {
            return Err("competing Java imports are ambiguous".into());
        }
        if let Some(import) = imports.first() {
            if import.is_static
                && (!catalog.contains(&import.path) || catalog.value_members.contains(&import.path))
            {
                return Err("the Java static import may bind a value instead of a type".into());
            }
            return Ok(format!("{}{suffix}", import.path));
        }
        let local = self.in_package(first);
        if catalog.contains(&local) {
            if !catalog.unique(&local) {
                return Err("duplicate Java type declarations are ambiguous".into());
            }
            return Ok(format!("{local}{suffix}"));
        }
        if parts.len() > 1 {
            return Ok(parts.join("."));
        }
        if let Some(identity) = prospective {
            return Ok(identity.to_string());
        }
        if self.imports.iter().any(|import| import.wildcard) {
            return Err("Java on-demand imports may compete with implicit java.lang types; compiler resolution is required".into());
        }
        // An absent same-package declaration cannot be the selected workspace
        // type. Keep the implicit Java namespace distinct from it.
        Ok(format!("java.lang.{first}"))
    }
}

fn prefix_path(root: Node<'_>, through: Node<'_>, source: &str) -> Result<Vec<String>, String> {
    let parts = descendants(root)
        .filter(|node| {
            matches!(node.kind(), "identifier" | "type_identifier")
                && node.end_byte() <= through.end_byte()
        })
        .map(|node| (node.start_byte(), text(node, source).to_string()))
        .collect::<BTreeMap<_, _>>();
    if parts.is_empty() {
        return Err("the Java qualified type name is empty".into());
    }
    Ok(parts.into_values().collect())
}
