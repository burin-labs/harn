//! Fail-closed binding proof for rename targets, using the existing module model.
//!
//! The symbol graph identifies the seed, not every lexical binding. A spelling
//! match is writable only in a known reference position with an unambiguous
//! binding. A file-level seed needs an unambiguous file-level binding in every
//! file. A seed declared inside a body is invisible outside that scope, so only
//! its scope is proved and other spellings are left alone. Unknown syntax,
//! shadowing bindings and receiver members stop the whole plan before
//! persistence.

use tree_sitter::{Node, Tree};

use crate::ast::name_resolution::{
    binding_defeaters, binding_defeaters_within, has_dynamic_class_lookup, rust_itemless_std_macro,
    Defeater,
};
use crate::ast::Language;

use super::move_symbol::{Family, Lang, Shape};
use super::refactor_core::IdentifierSpan;
use super::state::IndexState;
use super::symbol_graph::{Node as SymbolNode, NodeKind};

pub(super) struct Seed<'a> {
    pub state: &'a IndexState,
    pub node: &'a SymbolNode,
    pub name: &'a str,
    pub go_package: Option<&'a super::rename_go::PackageProof>,
}

pub(super) fn check(
    seed: Seed<'_>,
    path: &str,
    language: Language,
    source: &str,
    tree: &Tree,
    targets: &[IdentifierSpan],
) -> Result<Vec<IdentifierSpan>, String> {
    let Seed {
        state,
        node: seed_node,
        name,
        go_package,
    } = seed;
    let seed_path = seed_node.path.as_ref();
    let seed_line = seed_node.line;
    let seed_kind = seed_node.kind;
    let seed_language = Language::detect(std::path::Path::new(seed_path), None);
    if seed_language.is_none_or(|seed_language| seed_language != language) {
        return Err("cross-language bindings require a binding-aware LSP rename".into());
    }
    if path == seed_path {
        if let Some(lexical) = lexical(seed_node, language, source, tree) {
            return lexical.targets(path, language, source, name, tree, targets);
        }
    }
    if path == seed_path
        && !targets.iter().any(|span| {
            tree.root_node()
                .named_descendant_for_byte_range(span.start_byte, span.end_byte)
                .is_some_and(|node| declaration(node, seed_line, seed_kind))
        })
    {
        return Err("the indexed seed is no longer a supported top-level declaration".into());
    }
    if !matches!(seed_kind, NodeKind::Function | NodeKind::Type) {
        return Err(
            "receiver and non-declaration bindings require a binding-aware LSP rename".into(),
        );
    }
    let defeaters = binding_defeaters(tree, source, language);
    if !defeaters.is_empty() {
        return Err(format!(
            "AST binding proof for {path} is incomplete ({})",
            defeaters
                .into_iter()
                .map(Defeater::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let mut lang = Family::of(language).map(|family| Lang::new(family, state, &[]));
    let module = lang.as_mut().and_then(|lang| lang.module_of(seed_path));
    let model = lang.as_mut().map(|lang| lang.model(path, tree, source));
    let mut bound = path == seed_path
        || (language == Language::Go
            && go_package.is_some_and(|package| package.proves_binding(path)));
    if let Some(model) = &model {
        for binding in &model.bindings {
            if binding.shape == Shape::Glob {
                return Err("wildcard imports do not establish a unique lexical binding".into());
            }
            if binding.local == name || binding.imported == name {
                if binding.shape != Shape::Named
                    || binding.imported != name
                    || Some(&binding.target) != module.as_ref()
                {
                    return Err("a same-spelled import does not resolve to the seed module".into());
                }
                bound |= binding.local == name && binding.top_level;
            }
        }
    }
    for span in targets {
        let node = tree
            .root_node()
            .named_descendant_for_byte_range(span.start_byte, span.end_byte)
            .ok_or_else(|| "identifier has no syntax node".to_string())?;
        if has_dynamic_class_lookup(node, language) {
            return Err(format!(
                "Python class namespace lookup at {path}:{}:{} is unresolved",
                span.start_row + 1,
                span.start_col + 1
            ));
        }
        if let Some(model) = &model {
            if model.in_import(span.start_byte) {
                let proven = model.bindings.iter().any(|binding| {
                    binding.statement.contains(&span.start_byte)
                        && binding.shape == Shape::Named
                        && binding.imported == name
                        && Some(&binding.target) == module.as_ref()
                });
                if proven && import_name(node) {
                    continue;
                }
                return Err("import occurrence has no resolved seed binding".into());
            }
        }
        if path == seed_path && declaration(node, seed_line, seed_kind) {
            continue;
        }
        if !bound || !reference(node, language, source, name, None) {
            return Err(format!(
                "unresolved binding at {path}:{}:{} ({})",
                span.start_row + 1,
                span.start_col + 1,
                node.parent().map_or("root", |parent| parent.kind())
            ));
        }
    }
    Ok(targets.to_vec())
}

fn import_name(node: Node<'_>) -> bool {
    let at = node.start_byte();
    let mut outer = node.parent();
    while let Some(parent) = outer {
        let module_field = match parent.kind() {
            "import_from_statement" => Some("module_name"),
            "scoped_identifier" | "scoped_type_identifier" | "scoped_use_list" => Some("path"),
            _ => None,
        };
        if module_field
            .and_then(|field| parent.child_by_field_name(field))
            .is_some_and(|module| module.byte_range().contains(&at))
        {
            return false;
        }
        outer = parent.parent();
    }
    true
}

fn field(node: Node<'_>, parent: Node<'_>, name: &str) -> bool {
    parent
        .child_by_field_name(name)
        .is_some_and(|child| child.id() == node.id())
}

fn declaration(node: Node<'_>, line: u32, kind: NodeKind) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if node.start_position().row + 1 != line as usize || !field(node, parent, "name") {
        return false;
    }
    let correct_kind = match kind {
        NodeKind::Function => matches!(
            parent.kind(),
            "function_item" | "function_definition" | "function_declaration"
        ),
        NodeKind::Type => matches!(
            parent.kind(),
            "struct_item"
                | "enum_item"
                | "trait_item"
                | "type_item"
                | "class_definition"
                | "class_declaration"
                | "interface_declaration"
                | "type_alias_declaration"
                | "type_spec"
                | "struct_declaration"
        ),
        _ => false,
    };
    if !correct_kind {
        return false;
    }
    let mut outer = parent.parent();
    while let Some(ancestor) = outer {
        if !matches!(
            ancestor.kind(),
            "source_file"
                | "program"
                | "module"
                | "export_statement"
                | "decorated_definition"
                | "type_declaration"
        ) {
            return false;
        }
        outer = ancestor.parent();
    }
    true
}

/// Only syntax positions with no binding or receiver semantics are accepted.
/// Keep this allowlist conservative: a new grammar production must be proved
/// before it can become a mutation target.
///
/// `boundary` ends the ancestor walk at the scope that declares the seed.
fn reference(
    node: Node<'_>,
    language: Language,
    source: &str,
    name: &str,
    boundary: Option<Node<'_>>,
) -> bool {
    let mut outer = node.parent();
    while let Some(ancestor) = outer {
        if boundary.is_some_and(|boundary| boundary.id() == ancestor.id()) {
            break;
        }
        if matches!(
            ancestor.kind(),
            "mod_item" | "internal_module" | "module_declaration"
        ) {
            return false;
        }
        if declares_type_parameter(ancestor, source, name) {
            return false;
        }
        outer = ancestor.parent();
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() == "token_tree" {
        return macro_argument_reference(node, source);
    }
    if type_position(node, parent, language) {
        return true;
    }
    if matches!(
        node.kind(),
        "field_identifier"
            | "property_identifier"
            | "shorthand_property_identifier"
            | "shorthand_property_identifier_pattern"
            | "shorthand_field_identifier"
            | "package_identifier"
    ) {
        return false;
    }
    match parent.kind() {
        "call_expression" => {
            field(node, parent, "function")
                || field(node, parent, "constructor")
                || language == Language::Swift
        }
        "call" => field(node, parent, "function"),
        "new_expression" => field(node, parent, "constructor"),
        "scoped_identifier" | "scoped_type_identifier" => {
            // `Widget::new` reads Widget; `other::Widget` has another owner.
            field(node, parent, "path") && node.kind() != "field_identifier"
        }
        "let_declaration" | "variable_declarator" | "const_spec" | "var_spec" => {
            field(node, parent, "value") || field(node, parent, "type")
        }
        "assignment" | "assignment_expression" => field(node, parent, "right"),
        "field_initializer" | "keyword_argument" => field(node, parent, "value"),
        "parameter"
        | "required_parameter"
        | "optional_parameter"
        | "typed_parameter"
        | "typed_default_parameter"
        | "default_parameter" => field(node, parent, "type") || field(node, parent, "value"),
        "field_expression" => field(node, parent, "value"),
        "attribute" => field(node, parent, "object"),
        "member_expression" => field(node, parent, "object"),
        "struct_expression" | "composite_literal" => {
            field(node, parent, "name") || field(node, parent, "type")
        }
        "field_declaration"
        | "struct_field"
        | "property_signature"
        | "property_declaration"
        | "type_item"
        | "type_alias_declaration" => field(node, parent, "type") || field(node, parent, "value"),
        "impl_item" | "impl_block" => {
            field(node, parent, "type")
                || field(node, parent, "trait")
                || field(node, parent, "type_name")
        }
        "arguments"
        | "argument_list"
        | "value_arguments"
        | "return_expression"
        | "return_statement"
        | "expression_statement"
        | "binary_expression"
        | "binary_operator"
        | "boolean_operator"
        | "comparison_operator"
        | "unary_expression"
        | "unary_operator"
        | "await_expression"
        // `typeof name` in a type reads the value binding in lexical scope.
        | "type_query"
        | "try_expression"
        | "parenthesized_expression"
        | "tuple_expression"
        | "list"
        | "list_expression"
        | "array"
        | "array_expression"
        | "type_annotation"
        | "type_arguments"
        | "generic_type"
        | "reference_type"
        | "pointer_type"
        | "nullable_type"
        | "optional_type"
        | "user_type"
        | "return_type" => true,
        "function_item" | "function_definition" | "function_declaration" => {
            field(node, parent, "return_type")
        }
        _ => false,
    }
}

/// A generic parameter of `scope` spelled `name`, which shadows the seed for
/// everything inside that item.
fn declares_type_parameter(scope: Node<'_>, source: &str, name: &str) -> bool {
    let Some(parameters) = scope.child_by_field_name("type_parameters") else {
        return false;
    };
    let mut cursor = parameters.walk();
    let declared = parameters.named_children(&mut cursor).any(|parameter| {
        let declared = match parameter.kind() {
            "type_identifier" | "identifier" => Some(parameter),
            "constrained_type_parameter" => parameter.child_by_field_name("left"),
            "type_parameter" | "optional_type_parameter" => parameter
                .child_by_field_name("name")
                .or_else(|| parameter.named_child(0)),
            _ => None,
        };
        declared.is_some_and(|declared| source.get(declared.byte_range()) == Some(name))
    });
    declared
}

/// A type name used in type syntax. Type syntax cannot bind a name except in a
/// declaring slot (an item's name, a generic parameter, `infer`), and generic
/// shadowing is ruled out by [`declares_type_parameter`] first.
fn type_position(node: Node<'_>, parent: Node<'_>, language: Language) -> bool {
    match language {
        Language::Rust | Language::TypeScript | Language::Tsx => {
            node.kind() == "type_identifier"
                && !field(node, parent, "name")
                && !matches!(
                    parent.kind(),
                    "scoped_type_identifier"
                        | "type_parameters"
                        | "constrained_type_parameter"
                        | "type_parameter"
                        | "optional_type_parameter"
                        | "infer_type"
                        | "mapped_type_clause"
                )
        }
        // An annotation is evaluated as an expression; a plain name or the
        // head of a dotted name in it reads the module binding.
        Language::Python => {
            if parent.kind() == "attribute" && !field(node, parent, "object") {
                return false;
            }
            let mut outer = Some(parent);
            while let Some(ancestor) = outer {
                match ancestor.kind() {
                    "type" => return true,
                    "function_definition" | "class_definition" | "lambda" | "block" | "module" => {
                        return false
                    }
                    _ => outer = ancestor.parent(),
                }
            }
            false
        }
        _ => false,
    }
}

/// An identifier token inside a macro's arguments. Only an itemless `std`
/// macro is known to treat its arguments as expressions, where a bare name or
/// a path head reads the file binding. A token after `.` is a member, after
/// `::` a path tail with its own owner, and before a single `=` a named
/// format argument.
fn macro_argument_reference(node: Node<'_>, source: &str) -> bool {
    let mut invocation = node.parent();
    while let Some(ancestor) = invocation {
        if ancestor.kind() == "macro_invocation" {
            break;
        }
        if ancestor.kind() != "token_tree" {
            return false;
        }
        invocation = ancestor.parent();
    }
    let Some(invocation) = invocation else {
        return false;
    };
    if !rust_itemless_std_macro(invocation, source) {
        return false;
    }
    let before = node
        .prev_sibling()
        .and_then(|token| source.get(token.byte_range()));
    let after = node
        .next_sibling()
        .and_then(|token| source.get(token.byte_range()));
    !matches!(before, Some("." | "::" | "$")) && after != Some("=")
}

/// A seed declared inside a body. Its binding is invisible outside `scope`, so
/// a same-spelled name elsewhere is another binding and stays unchanged.
/// `region` adds what shares the scope's names without being in it, such as
/// a function's parameters or a module's own name; a mention there refuses.
pub(super) struct Lexical<'t> {
    declaration: Node<'t>,
    scope: Node<'t>,
    region: Node<'t>,
}

/// The seed's declaration when it is block, function or private-module local.
pub(super) fn lexical<'t>(
    seed: &SymbolNode,
    language: Language,
    source: &str,
    tree: &'t Tree,
) -> Option<Lexical<'t>> {
    let kinds = language.rename_identifier_kinds()?;
    let mut spans = Vec::new();
    super::refactor_core::collect_identifier_spans(
        tree.root_node(),
        source.as_bytes(),
        &seed.name,
        "",
        kinds,
        "",
        &mut spans,
        &mut Vec::new(),
    );
    let names: Vec<Node<'t>> = spans
        .iter()
        .filter(|span| span.start_row + 1 == seed.line as usize)
        .filter(|span| {
            seed.col.is_none_or(|col| {
                let before = source.get(..span.start_byte).unwrap_or_default();
                let line = before.rsplit('\n').next().unwrap_or_default();
                line.encode_utf16().count() + 1 == col as usize
            })
        })
        .filter_map(|span| {
            tree.root_node()
                .named_descendant_for_byte_range(span.start_byte, span.end_byte)
        })
        .collect();
    // Another spelling at the seed's position could be the indexed declaration.
    let [name] = names.as_slice() else {
        return None;
    };
    local_declaration(*name, language, seed.kind)
}

impl Lexical<'_> {
    /// Classify every spelling in the seed file as the binding, a name with no
    /// lexical meaning, or unresolved. Any unresolved spelling refuses.
    fn targets(
        &self,
        path: &str,
        language: Language,
        source: &str,
        name: &str,
        tree: &Tree,
        targets: &[IdentifierSpan],
    ) -> Result<Vec<IdentifierSpan>, String> {
        let defeaters = binding_defeaters_within(self.region, source, language);
        if !defeaters.is_empty() {
            return Err(format!(
                "AST binding proof for the scope of `{name}` in {path} is incomplete ({})",
                defeaters
                    .into_iter()
                    .map(Defeater::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        // A child module, possibly in another file, can name a private parent item.
        if language == Language::Rust && nested_module(self.scope) {
            return Err(format!(
                "a module nested in the scope of `{name}` in {path} may reference it"
            ));
        }
        let mut kept = Vec::new();
        for span in targets {
            let node = tree
                .root_node()
                .named_descendant_for_byte_range(span.start_byte, span.end_byte)
                .ok_or_else(|| "identifier has no syntax node".to_string())?;
            if !encloses(self.region, node) {
                continue;
            }
            if node.id() == self.declaration.id() {
                kept.push(*span);
                continue;
            }
            let resolved = encloses(self.scope, node)
                && !has_dynamic_class_lookup(node, language)
                && (member_name(node, language)
                    || reference(node, language, source, name, Some(self.scope)));
            if !resolved {
                return Err(format!(
                    "unresolved binding at {path}:{}:{} ({})",
                    span.start_row + 1,
                    span.start_col + 1,
                    node.parent().map_or("root", |parent| parent.kind())
                ));
            }
            if !member_name(node, language) {
                kept.push(*span);
            }
        }
        Ok(kept)
    }
}

fn nested_module(scope: Node<'_>) -> bool {
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        if node.kind() == "mod_item" {
            return true;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    false
}

fn encloses(outer: Node<'_>, inner: Node<'_>) -> bool {
    outer.start_byte() <= inner.start_byte() && inner.end_byte() <= outer.end_byte()
}

/// A property, field or keyword name: it never reads a lexical binding.
/// Shorthand forms both name a member and read or bind a variable, so they
/// are absent here and refuse.
fn member_name(node: Node<'_>, language: Language) -> bool {
    match language {
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx => {
            node.kind() == "property_identifier"
        }
        Language::Rust => node.kind() == "field_identifier",
        Language::Python => node.parent().is_some_and(|parent| {
            (parent.kind() == "attribute" && field(node, parent, "attribute"))
                || (parent.kind() == "keyword_argument" && field(node, parent, "name"))
        }),
        _ => false,
    }
}

/// `name` declares a `kind` binding whose scope is a body, not the file.
fn local_declaration<'t>(
    name: Node<'t>,
    language: Language,
    kind: NodeKind,
) -> Option<Lexical<'t>> {
    let parent = name.parent()?;
    if !field(name, parent, "name") {
        return None;
    }
    let (scope, region) = match language {
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx => {
            script_scope(parent, kind)?
        }
        Language::Rust => rust_scope(parent, kind)?,
        Language::Python => python_scope(parent, kind)?,
        _ => return None,
    };
    Some(Lexical {
        declaration: name,
        scope,
        region,
    })
}

/// The body a function-like node owns, if `block` is it.
fn body_of<'t>(owner: Node<'t>, block: Node<'t>) -> bool {
    owner
        .child_by_field_name("body")
        .is_some_and(|body| body.id() == block.id())
}

fn script_scope<'t>(declaration: Node<'t>, kind: NodeKind) -> Option<(Node<'t>, Node<'t>)> {
    const FUNCTION_LIKE: &[&str] = &[
        "function_declaration",
        "generator_function_declaration",
        "function_expression",
        "function",
        "generator_function",
        "arrow_function",
        "method_definition",
    ];
    // Block-scoped declarations. A function declaration is block scoped only
    // in strict code, so it is admitted only directly in a function body,
    // where sloppy-mode hoisting reaches the same scope.
    let (statement, function_only) = match (kind, declaration.kind()) {
        (NodeKind::Function, "function_declaration" | "generator_function_declaration") => {
            (declaration, true)
        }
        (NodeKind::Function, "variable_declarator") => {
            let statement = declaration.parent()?;
            (statement.kind() == "lexical_declaration").then_some((statement, false))?
        }
        (
            NodeKind::Type,
            "class_declaration"
            | "abstract_class_declaration"
            | "interface_declaration"
            | "type_alias_declaration"
            | "enum_declaration",
        ) => (declaration, false),
        _ => return None,
    };
    let holder = statement.parent()?;
    let scope = match holder.kind() {
        "statement_block" => holder,
        "switch_case" | "switch_default" if !function_only => holder.parent()?,
        _ => return None,
    };
    let mut outer = scope.parent();
    while let Some(ancestor) = outer {
        // Namespace members merge and export across declarations.
        if matches!(
            ancestor.kind(),
            "internal_module" | "module" | "ambient_declaration"
        ) {
            return None;
        }
        outer = ancestor.parent();
    }
    let owner = scope
        .parent()
        .filter(|owner| FUNCTION_LIKE.contains(&owner.kind()) && body_of(*owner, scope));
    if function_only && owner.is_none() {
        return None;
    }
    Some((scope, owner.unwrap_or(scope)))
}

fn rust_scope<'t>(item: Node<'t>, kind: NodeKind) -> Option<(Node<'t>, Node<'t>)> {
    let declares = match kind {
        NodeKind::Function => item.kind() == "function_item",
        NodeKind::Type => matches!(
            item.kind(),
            "struct_item" | "enum_item" | "union_item" | "type_item" | "trait_item"
        ),
        _ => false,
    };
    if !declares {
        return None;
    }
    let holder = item.parent()?;
    let owner = holder.parent()?;
    match holder.kind() {
        // A block item is visible only inside the block, whatever its visibility.
        "block" => {
            let region = if matches!(owner.kind(), "function_item" | "closure_expression")
                && body_of(owner, holder)
            {
                owner
            } else {
                holder
            };
            Some((holder, region))
        }
        // A private item of an inline module is visible only inside it.
        "declaration_list" if owner.kind() == "mod_item" => {
            let mut cursor = item.walk();
            let private = !item
                .children(&mut cursor)
                .any(|child| child.kind() == "visibility_modifier");
            private.then_some((holder, owner))
        }
        _ => None,
    }
}

fn python_scope<'t>(definition: Node<'t>, kind: NodeKind) -> Option<(Node<'t>, Node<'t>)> {
    let declares = match kind {
        NodeKind::Function => definition.kind() == "function_definition",
        NodeKind::Type => definition.kind() == "class_definition",
        _ => false,
    };
    if !declares {
        return None;
    }
    let statement = definition
        .parent()
        .filter(|parent| parent.kind() == "decorated_definition")
        .unwrap_or(definition);
    // A function's locals span its whole body, through compound statements;
    // a class body is a separate namespace its methods do not see.
    const COMPOUND: &[&str] = &[
        "block",
        "if_statement",
        "elif_clause",
        "else_clause",
        "for_statement",
        "while_statement",
        "try_statement",
        "except_clause",
        "except_group_clause",
        "finally_clause",
        "with_statement",
        "match_statement",
        "case_clause",
    ];
    let mut child = statement;
    let mut outer = statement.parent();
    while let Some(ancestor) = outer {
        if ancestor.kind() == "function_definition" {
            return body_of(ancestor, child).then_some((child, ancestor));
        }
        if !COMPOUND.contains(&ancestor.kind()) {
            return None;
        }
        child = ancestor;
        outer = ancestor.parent();
    }
    None
}
