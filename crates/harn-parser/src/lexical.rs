//! Lexical binding and capture analysis shared by compiler and typechecker.
//!
//! AST visitors answer structural questions. Capture analysis is different: an
//! identifier only captures a binding when it resolves outside the callable
//! that contains the reference. Keeping that resolution here avoids each
//! consumer inventing a slightly different notion of scope and shadowing.

use std::collections::{BTreeSet, HashMap, HashSet};

use harn_lexer::Span;

use crate::ast::{is_discard_name, BindingPattern, Node, SNode, TypedParam};

/// Stable identity for a source binding. Patterns do not carry individual
/// spans, so the declaration span plus the bound name is the narrowest source
/// identity available without changing the AST.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BindingId {
    pub name: String,
    pub declaration_start: usize,
    pub declaration_end: usize,
}

/// Compiler-resolved enum metadata needed to distinguish call-shaped enum
/// patterns from ordinary expression-equality patterns.
#[derive(Debug, Clone, Default)]
pub struct MatchPatternCatalog {
    enum_names: HashSet<String>,
    variant_owners: HashMap<String, Vec<String>>,
}

/// Resolution of a bare call-shaped match pattern such as `Ok(value)`.
/// Compiler lowering and lexical analysis share this decision so a pattern
/// cannot bind payload names in one subsystem and act as an expression in the
/// other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BareVariantResolution<'a> {
    NotVariant,
    Unique(&'a str),
    Ambiguous(&'a [String]),
}

pub fn resolve_bare_variant_owners(owners: Option<&[String]>) -> BareVariantResolution<'_> {
    match owners {
        None | Some([]) => BareVariantResolution::NotVariant,
        Some([owner]) => BareVariantResolution::Unique(owner),
        Some(owners) => BareVariantResolution::Ambiguous(owners),
    }
}

pub fn ambiguous_bare_variant_message(variant: &str, owners: &[String]) -> String {
    format!(
        "match pattern `{variant}(...)` is ambiguous: variant `{variant}` is declared by enums {}; qualify it as `{}.{variant}(...)`",
        owners.join(", "),
        owners[0],
    )
}

/// Node slices whose declarations are predeclared in the module type scope.
///
/// Top-level declarations and declarations directly inside pipeline bodies
/// share one module-visible namespace. Function, tool, closure, and nested
/// block declarations remain lexical to those bodies. The typechecker and VM
/// compiler consume this same projection so an inaccessible nested enum cannot
/// make a bare match pattern ambiguous in only one subsystem.
pub fn module_scope_node_slices(program: &[SNode]) -> Vec<&[SNode]> {
    let mut scopes = vec![program];
    for node in program {
        let inner = match &node.node {
            Node::AttributedDecl { inner, .. } => inner.as_ref(),
            _ => node,
        };
        if let Node::Pipeline { body, .. } = &inner.node {
            scopes.push(body);
        }
    }
    scopes
}

impl MatchPatternCatalog {
    pub fn new(
        enum_names: &HashSet<String>,
        variant_owners: &HashMap<String, Vec<String>>,
    ) -> Self {
        Self::from_parts(enum_names.clone(), variant_owners.clone())
    }

    pub fn from_parts(
        enum_names: HashSet<String>,
        mut variant_owners: HashMap<String, Vec<String>>,
    ) -> Self {
        for owners in variant_owners.values_mut() {
            owners.sort();
            owners.dedup();
        }
        Self {
            enum_names,
            variant_owners,
        }
    }

    pub fn resolve_bare_variant(&self, name: &str) -> BareVariantResolution<'_> {
        resolve_bare_variant_owners(self.variant_owners.get(name).map(Vec::as_slice))
    }

    pub fn is_enum_name(&self, name: &str) -> bool {
        self.enum_names.contains(name)
    }

    fn register_enum(&mut self, name: &str, variants: &[crate::ast::EnumVariant]) {
        for owners in self.variant_owners.values_mut() {
            owners.retain(|owner| owner != name);
        }
        self.variant_owners.retain(|_, owners| !owners.is_empty());
        self.enum_names.insert(name.to_string());
        for variant in variants {
            self.variant_owners
                .entry(variant.name.clone())
                .or_default()
                .push(name.to_string());
        }
    }
}

impl BindingId {
    pub fn from_declaration(name: impl Into<String>, span: Span) -> Self {
        Self {
            name: name.into(),
            declaration_start: span.start,
            declaration_end: span.end,
        }
    }
}

/// Return every name introduced by a destructuring pattern, in source order.
/// The projection is intentionally shared by the compiler and typechecker.
pub fn binding_pattern_names(pattern: &BindingPattern) -> Vec<String> {
    match pattern {
        BindingPattern::Identifier(name) => vec![name.clone()],
        BindingPattern::Pair(first, second) => vec![first.clone(), second.clone()],
        BindingPattern::Dict(fields) => fields
            .iter()
            .map(|field| field.alias.clone().unwrap_or_else(|| field.key.clone()))
            .collect(),
        BindingPattern::List(elements) => elements
            .iter()
            .map(|element| element.name.clone())
            .collect(),
    }
}

/// Return the source identities introduced by `pattern` at `declaration`.
pub fn binding_pattern_ids(pattern: &BindingPattern, declaration: Span) -> Vec<BindingId> {
    binding_pattern_names(pattern)
        .into_iter()
        .filter(|name| !is_discard_name(name))
        .map(|name| BindingId::from_declaration(name, declaration))
        .collect()
}

/// Bindings in the current compiled body referenced by a nested callable.
///
/// A result is declaration-identity based rather than name based. That keeps
/// `let pin` distinct from a later `{ pin -> ... }` parameter or an inner
/// block-local `let pin`, which is essential when selecting VM storage.
pub fn captured_bindings_in_nested_callables(
    body: &[SNode],
    match_patterns: &MatchPatternCatalog,
) -> HashSet<BindingId> {
    let mut analysis = LexicalAnalysis::new(match_patterns);
    analysis.walk_body(body, Vec::new(), false, BindingOwner::Current);
    analysis.captured
}

/// Bindings captured across one pipeline inheritance chain.
///
/// Parent and child bodies share runtime value bindings, so their lexical value
/// scope is cumulative. Each body is typechecked and compiled from the final
/// module enum catalog, however, so source-order enum shadowing must reset at a
/// pipeline boundary instead of leaking from a parent into its child.
pub fn captured_bindings_in_pipeline_lineage(
    bodies: &[&[SNode]],
    match_patterns: &MatchPatternCatalog,
) -> HashSet<BindingId> {
    let mut analysis = LexicalAnalysis::new(match_patterns);
    let mut value_scope = Scope::new();
    for body in bodies {
        value_scope.extend(hoisted_callable_scope(body));
    }

    for body in bodies {
        analysis.match_patterns = match_patterns.clone();
        for node in *body {
            analysis.walk_node(
                node,
                std::slice::from_ref(&value_scope),
                false,
                &BindingOwner::Current,
            );
            let declaration = match &node.node {
                Node::AttributedDecl { inner, .. } => inner.as_ref(),
                _ => node,
            };
            if let Node::EnumDecl { name, variants, .. } = &declaration.node {
                analysis.match_patterns.register_enum(name, variants);
            }
            extend_scope_with_value_declaration(&mut value_scope, node, &BindingOwner::Current);
        }
    }

    analysis.captured
}

/// Bindings captured under module execution order.
///
/// Module statements execute in source order first; callable declarations and
/// pipeline bodies are materialized only after every statement has run. This
/// differs from an ordinary block, where a later value is not visible to an
/// earlier nested callable. Modeling the two phases here keeps boxing aligned
/// with the bytecode compiler without teaching the VM another scope heuristic.
pub fn captured_bindings_in_compiled_module(
    body: &[SNode],
    match_patterns: &MatchPatternCatalog,
) -> HashSet<BindingId> {
    let mut analysis = LexicalAnalysis::new(match_patterns);
    let mut value_scope = Scope::new();

    for node in body {
        if is_deferred_module_declaration(node) {
            continue;
        }
        analysis.walk_node(
            node,
            std::slice::from_ref(&value_scope),
            false,
            &BindingOwner::Current,
        );
        extend_scope_with_value_declaration(&mut value_scope, node, &BindingOwner::Current);
    }

    let mut phase_two_scope = hoisted_callable_scope(body);
    phase_two_scope.extend(value_scope);
    for node in body {
        if is_deferred_module_declaration(node) {
            analysis.walk_node(
                node,
                std::slice::from_ref(&phase_two_scope),
                false,
                &BindingOwner::Current,
            );
        }
    }
    analysis.captured
}

/// Names reassigned by a nested callable that are free relative to the current
/// callable body. Type-flow narrowing uses this conservative summary: unknown
/// names remain included so parameter captures continue to invalidate their
/// narrowing at the caller-owned scope.
pub fn nested_callable_reassigned_names(
    body: &[SNode],
    match_patterns: &MatchPatternCatalog,
) -> Vec<String> {
    let mut analysis = LexicalAnalysis::new(match_patterns);
    analysis.walk_body(body, Vec::new(), false, BindingOwner::Current);
    analysis.reassigned.into_iter().collect()
}

#[derive(Debug, Clone)]
enum BindingOwner {
    Current,
    Nested,
}

#[derive(Debug, Clone)]
enum ScopeBinding {
    Current(BindingId),
    Nested,
}

type Scope = HashMap<String, ScopeBinding>;

struct LexicalAnalysis {
    captured: HashSet<BindingId>,
    reassigned: BTreeSet<String>,
    match_patterns: MatchPatternCatalog,
}

impl LexicalAnalysis {
    fn new(match_patterns: &MatchPatternCatalog) -> Self {
        Self {
            captured: HashSet::new(),
            reassigned: BTreeSet::new(),
            match_patterns: match_patterns.clone(),
        }
    }

    fn walk_body(
        &mut self,
        body: &[SNode],
        scopes: Vec<Scope>,
        inside_nested_callable: bool,
        owner: BindingOwner,
    ) {
        self.walk_body_with_bindings(body, scopes, inside_nested_callable, owner, Scope::new());
    }

    fn walk_body_with_bindings(
        &mut self,
        body: &[SNode],
        mut scopes: Vec<Scope>,
        inside_nested_callable: bool,
        owner: BindingOwner,
        extra_bindings: Scope,
    ) {
        let outer_match_patterns = self.match_patterns.clone();
        // Named callables are late-bound and may recurse or mutually recurse.
        // Value bindings become visible only after their declaration executes.
        let mut scope = hoisted_callable_scope(body);
        scope.extend(extra_bindings);
        scopes.push(scope);
        for node in body {
            self.walk_node(node, &scopes, inside_nested_callable, &owner);
            let declaration = match &node.node {
                Node::AttributedDecl { inner, .. } => inner.as_ref(),
                _ => node,
            };
            if let Node::EnumDecl { name, variants, .. } = &declaration.node {
                self.match_patterns.register_enum(name, variants);
            }
            extend_scope_with_value_declaration(
                scopes.last_mut().expect("body scope"),
                node,
                &owner,
            );
        }
        self.match_patterns = outer_match_patterns;
    }

    fn walk_node(
        &mut self,
        node: &SNode,
        scopes: &[Scope],
        inside_nested_callable: bool,
        owner: &BindingOwner,
    ) {
        match &node.node {
            Node::Identifier(name) => self.record_reference(name, scopes, inside_nested_callable),
            Node::FunctionCall { name, .. } => {
                // Bare calls resolve a user binding before falling back to a
                // builtin. Keep the complete name intact so dotted builtin
                // names do not become references to their first component.
                self.record_reference(name, scopes, inside_nested_callable);
                self.walk_children(node, scopes, inside_nested_callable, owner);
            }
            Node::Assignment { target, .. } => {
                if inside_nested_callable {
                    if let Node::Identifier(name) = &target.node {
                        self.record_reassignment(name, scopes);
                    }
                }
                self.walk_children(node, scopes, inside_nested_callable, owner);
            }
            Node::Closure { params, body, .. }
            | Node::FnDecl { params, body, .. }
            | Node::ToolDecl { params, body, .. } => {
                // Defaults run left to right: earlier parameters are visible,
                // while the current and later parameters still resolve outside
                // the callable.
                let mut default_scopes = scopes.to_vec();
                default_scopes.push(Scope::new());
                for param in params {
                    if let Some(default) = &param.default_value {
                        self.walk_node(default, &default_scopes, true, owner);
                    }
                    default_scopes
                        .last_mut()
                        .expect("parameter default scope")
                        .extend(names_scope([param.name.clone()]));
                }
                self.walk_callable_body(body, params, scopes);
            }
            Node::Pipeline { params, body, .. } => {
                self.walk_callable_body(body, params, scopes);
            }
            Node::OverrideDecl { params, body, .. } => {
                let bindings = names_scope(params.iter().cloned());
                self.walk_body_with_bindings(
                    body,
                    scopes.to_vec(),
                    true,
                    BindingOwner::Nested,
                    bindings,
                );
            }
            Node::SpawnExpr { body } => {
                self.walk_body(body, scopes.to_vec(), true, BindingOwner::Nested);
            }
            Node::Parallel {
                expr,
                variable,
                body,
                options,
                ..
            } => {
                self.walk_node(expr, scopes, inside_nested_callable, owner);
                for (_, option) in options {
                    self.walk_node(option, scopes, inside_nested_callable, owner);
                }
                let bindings = variable.iter().cloned().collect::<Vec<_>>();
                self.walk_body_with_bindings(
                    body,
                    scopes.to_vec(),
                    true,
                    BindingOwner::Nested,
                    names_scope(bindings),
                );
            }
            Node::ForIn {
                pattern,
                iterable,
                body,
            } => {
                self.walk_pattern_defaults(pattern, scopes, inside_nested_callable, owner);
                self.walk_node(iterable, scopes, inside_nested_callable, owner);
                self.walk_body_with_bindings(
                    body,
                    scopes.to_vec(),
                    inside_nested_callable,
                    owner.clone(),
                    pattern_scope(pattern, node.span, owner),
                );
            }
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                self.walk_node(condition, scopes, inside_nested_callable, owner);
                self.walk_body(
                    then_body,
                    scopes.to_vec(),
                    inside_nested_callable,
                    owner.clone(),
                );
                if let Some(else_body) = else_body {
                    self.walk_body(
                        else_body,
                        scopes.to_vec(),
                        inside_nested_callable,
                        owner.clone(),
                    );
                }
            }
            Node::MatchExpr { value, arms } => {
                self.walk_node(value, scopes, inside_nested_callable, owner);
                for arm in arms {
                    let bindings = self.analyze_match_pattern(
                        &arm.pattern,
                        scopes,
                        inside_nested_callable,
                        owner,
                    );
                    let mut arm_scopes = scopes.to_vec();
                    arm_scopes.push(bindings);
                    if let Some(guard) = &arm.guard {
                        self.walk_node(guard, &arm_scopes, inside_nested_callable, owner);
                    }
                    self.walk_body(&arm.body, arm_scopes, inside_nested_callable, owner.clone());
                }
            }
            Node::WhileLoop { condition, body } => {
                self.walk_node(condition, scopes, inside_nested_callable, owner);
                self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
            }
            Node::Retry { count, body } => {
                self.walk_node(count, scopes, inside_nested_callable, owner);
                self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
            }
            Node::CostRoute { options, body } => {
                for (_, option) in options {
                    self.walk_node(option, scopes, inside_nested_callable, owner);
                }
                self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
            }
            Node::TryCatch {
                body,
                error_var,
                catch_body,
                finally_body,
                ..
            } => {
                self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
                let catch_binding = names_scope(error_var.iter().cloned());
                self.walk_body_with_bindings(
                    catch_body,
                    scopes.to_vec(),
                    inside_nested_callable,
                    owner.clone(),
                    catch_binding,
                );
                if let Some(finally_body) = finally_body {
                    self.walk_body(
                        finally_body,
                        scopes.to_vec(),
                        inside_nested_callable,
                        owner.clone(),
                    );
                }
            }
            Node::TryExpr { body }
            | Node::ScopeBlock { body }
            | Node::DeferStmt { body }
            | Node::Block(body) => {
                self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
            }
            Node::GuardStmt {
                condition,
                else_body,
            } => {
                self.walk_node(condition, scopes, inside_nested_callable, owner);
                self.walk_body(
                    else_body,
                    scopes.to_vec(),
                    inside_nested_callable,
                    owner.clone(),
                );
            }
            Node::DeadlineBlock { duration, body } => {
                self.walk_node(duration, scopes, inside_nested_callable, owner);
                self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
            }
            Node::MutexBlock { key, body } => {
                if let Some(key) = key {
                    self.walk_node(key, scopes, inside_nested_callable, owner);
                }
                self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
            }
            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            } => {
                for case in cases {
                    self.walk_node(&case.channel, scopes, inside_nested_callable, owner);
                    self.walk_body_with_bindings(
                        &case.body,
                        scopes.to_vec(),
                        inside_nested_callable,
                        owner.clone(),
                        names_scope([case.variable.clone()]),
                    );
                }
                if let Some((duration, body)) = timeout {
                    self.walk_node(duration, scopes, inside_nested_callable, owner);
                    self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
                }
                if let Some(body) = default_body {
                    self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
                }
            }
            Node::EvalPackDecl {
                fields,
                body,
                summarize,
                ..
            } => {
                for (_, value) in fields {
                    self.walk_node(value, scopes, inside_nested_callable, owner);
                }
                self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
                if let Some(summary) = summarize {
                    self.walk_body(
                        summary,
                        scopes.to_vec(),
                        inside_nested_callable,
                        owner.clone(),
                    );
                }
            }
            _ => self.walk_children(node, scopes, inside_nested_callable, owner),
        }
    }

    fn walk_callable_body(&mut self, body: &[SNode], params: &[TypedParam], scopes: &[Scope]) {
        self.walk_body_with_bindings(
            body,
            scopes.to_vec(),
            true,
            BindingOwner::Nested,
            names_scope(params.iter().map(|param| param.name.clone())),
        );
    }

    /// Analyze the expression parts of a match pattern and return the names
    /// that compiler lowering binds before the arm guard and body execute.
    fn analyze_match_pattern(
        &mut self,
        pattern: &SNode,
        scopes: &[Scope],
        inside_nested_callable: bool,
        owner: &BindingOwner,
    ) -> Scope {
        let mut bindings = Vec::new();
        match &pattern.node {
            Node::Identifier(name) if name != "_" => bindings.push(name.clone()),
            Node::Identifier(_) => {}
            Node::EnumConstruct { args, .. } => {
                for arg in args {
                    if let Node::Identifier(name) = &arg.node {
                        bindings.push(name.clone());
                    }
                }
            }
            Node::FunctionCall { name, args, .. }
                if matches!(
                    self.match_patterns.resolve_bare_variant(name),
                    BareVariantResolution::Unique(_)
                ) =>
            {
                for arg in args {
                    if let Node::Identifier(name) = &arg.node {
                        bindings.push(name.clone());
                    }
                }
            }
            Node::PropertyAccess { object, .. } if matches!(&object.node, Node::Identifier(name) if self.match_patterns.is_enum_name(name)) =>
                {}
            Node::MethodCall { object, args, .. } if matches!(&object.node, Node::Identifier(name) if self.match_patterns.is_enum_name(name)) => {
                for arg in args {
                    if let Node::Identifier(name) = &arg.node {
                        bindings.push(name.clone());
                    }
                }
            }
            Node::DictLiteral(entries)
                if entries
                    .iter()
                    .all(|entry| matches!(&entry.key.node, Node::StringLiteral(_))) =>
            {
                for entry in entries {
                    if let Node::Identifier(name) = &entry.value.node {
                        bindings.push(name.clone());
                    } else {
                        self.walk_node(&entry.value, scopes, inside_nested_callable, owner);
                    }
                }
            }
            Node::ListLiteral(elements) => {
                for element in elements {
                    match &element.node {
                        Node::Identifier(name) if name != "_" => bindings.push(name.clone()),
                        Node::Identifier(_) => {}
                        Node::Spread(inner) => {
                            if let Node::Identifier(name) = &inner.node {
                                bindings.push(name.clone());
                            } else {
                                self.walk_node(inner, scopes, inside_nested_callable, owner);
                            }
                        }
                        _ => {
                            self.walk_node(element, scopes, inside_nested_callable, owner);
                        }
                    }
                }
            }
            _ => self.walk_node(pattern, scopes, inside_nested_callable, owner),
        }
        names_scope(bindings)
    }

    fn walk_pattern_defaults(
        &mut self,
        pattern: &BindingPattern,
        scopes: &[Scope],
        inside_nested_callable: bool,
        owner: &BindingOwner,
    ) {
        match pattern {
            BindingPattern::Dict(fields) => {
                for field in fields {
                    if let Some(default) = &field.default_value {
                        self.walk_node(default, scopes, inside_nested_callable, owner);
                    }
                }
            }
            BindingPattern::List(elements) => {
                for element in elements {
                    if let Some(default) = &element.default_value {
                        self.walk_node(default, scopes, inside_nested_callable, owner);
                    }
                }
            }
            BindingPattern::Identifier(_) | BindingPattern::Pair(_, _) => {}
        }
    }

    fn walk_children(
        &mut self,
        node: &SNode,
        scopes: &[Scope],
        inside_nested_callable: bool,
        owner: &BindingOwner,
    ) {
        for child in crate::visit::immediate_children(node) {
            self.walk_node(child, scopes, inside_nested_callable, owner);
        }
    }

    fn record_reference(&mut self, name: &str, scopes: &[Scope], inside_nested_callable: bool) {
        if !inside_nested_callable {
            return;
        }
        if let Some(ScopeBinding::Current(binding)) = resolve(scopes, name) {
            self.captured.insert(binding.clone());
        }
    }

    fn record_reassignment(&mut self, name: &str, scopes: &[Scope]) {
        match resolve(scopes, name) {
            Some(ScopeBinding::Nested) => {}
            Some(ScopeBinding::Current(binding)) => {
                self.reassigned.insert(binding.name.clone());
            }
            None => {
                self.reassigned.insert(name.to_string());
            }
        }
    }
}

fn hoisted_callable_scope(body: &[SNode]) -> Scope {
    let mut scope = Scope::new();
    for node in body {
        match &node.node {
            Node::FnDecl { name, .. }
            | Node::ToolDecl { name, .. }
            | Node::Pipeline { name, .. }
            | Node::OverrideDecl { name, .. } => {
                scope.insert(name.clone(), ScopeBinding::Nested);
            }
            Node::AttributedDecl { inner, .. } => {
                if let Node::FnDecl { name, .. }
                | Node::ToolDecl { name, .. }
                | Node::Pipeline { name, .. }
                | Node::OverrideDecl { name, .. } = &inner.node
                {
                    scope.insert(name.clone(), ScopeBinding::Nested);
                }
            }
            _ => {}
        }
    }
    scope
}

/// Whether module compilation defers this declaration until after executable
/// top-level statements. Capture analysis and bytecode lowering share this
/// predicate so their visibility phases cannot drift.
pub fn is_deferred_module_declaration(node: &SNode) -> bool {
    let node = match &node.node {
        Node::AttributedDecl { inner, .. } => &inner.node,
        node => node,
    };
    matches!(
        node,
        Node::Pipeline { .. }
            | Node::OverrideDecl { .. }
            | Node::EvalPackDecl { .. }
            | Node::FnDecl { .. }
            | Node::ToolDecl { .. }
            | Node::SkillDecl { .. }
            | Node::ImplBlock { .. }
            | Node::StructDecl { .. }
            | Node::EnumDecl { .. }
            | Node::InterfaceDecl { .. }
            | Node::TypeDecl { .. }
            | Node::ImportDecl { .. }
            | Node::SelectiveImport { .. }
    )
}

fn extend_scope_with_value_declaration(scope: &mut Scope, node: &SNode, owner: &BindingOwner) {
    let (Node::LetBinding { pattern, .. } | Node::ConstBinding { pattern, .. }) = &node.node else {
        return;
    };
    for binding in binding_pattern_ids(pattern, node.span) {
        let name = binding.name.clone();
        let entry = match owner {
            BindingOwner::Current => ScopeBinding::Current(binding),
            BindingOwner::Nested => ScopeBinding::Nested,
        };
        scope.insert(name, entry);
    }
}

fn pattern_scope(pattern: &BindingPattern, declaration: Span, owner: &BindingOwner) -> Scope {
    let mut scope = Scope::new();
    for binding in binding_pattern_ids(pattern, declaration) {
        let name = binding.name.clone();
        let entry = match owner {
            BindingOwner::Current => ScopeBinding::Current(binding),
            BindingOwner::Nested => ScopeBinding::Nested,
        };
        scope.insert(name, entry);
    }
    scope
}

fn names_scope(names: impl IntoIterator<Item = String>) -> Scope {
    names
        .into_iter()
        .filter(|name| !is_discard_name(name))
        .map(|name| (name, ScopeBinding::Nested))
        .collect()
}

fn resolve<'a>(scopes: &'a [Scope], name: &str) -> Option<&'a ScopeBinding> {
    scopes.iter().rev().find_map(|scope| scope.get(name))
}

#[cfg(test)]
mod tests {
    use harn_lexer::Span;

    use crate::ast::{DictEntry, MatchArm, SelectCase};

    use super::*;

    fn node(offset: usize, node: Node) -> SNode {
        SNode::new(node, Span::with_offsets(offset, offset + 1, 1, offset + 1))
    }

    fn identifier(offset: usize, name: &str) -> SNode {
        node(offset, Node::Identifier(name.to_string()))
    }

    fn function_call(offset: usize, name: &str) -> SNode {
        node(
            offset,
            Node::FunctionCall {
                name: name.to_string(),
                type_args: Vec::new(),
                args: Vec::new(),
            },
        )
    }

    fn let_binding(offset: usize, name: &str) -> SNode {
        node(
            offset,
            Node::LetBinding {
                pattern: BindingPattern::Identifier(name.to_string()),
                type_ann: None,
                value: Box::new(identifier(offset + 100, "value")),
                is_pub: false,
            },
        )
    }

    fn closure(offset: usize, params: Vec<TypedParam>, body: Vec<SNode>) -> SNode {
        node(
            offset,
            Node::Closure {
                params,
                return_type: None,
                throws: None,
                body,
                fn_syntax: false,
            },
        )
    }

    fn fn_decl(offset: usize, name: &str, body: Vec<SNode>) -> SNode {
        node(
            offset,
            Node::FnDecl {
                name: name.to_string(),
                type_params: Vec::new(),
                params: Vec::new(),
                return_type: None,
                throws: None,
                where_clauses: Vec::new(),
                body,
                is_pub: false,
                is_stream: false,
            },
        )
    }

    fn defaulted_param(name: &str, default: SNode) -> TypedParam {
        TypedParam {
            name: name.to_string(),
            type_expr: None,
            default_value: Some(Box::new(default)),
            rest: false,
        }
    }

    fn captured(body: &[SNode]) -> HashSet<BindingId> {
        captured_bindings_in_nested_callables(body, &MatchPatternCatalog::default())
    }

    fn enum_pattern_catalog() -> MatchPatternCatalog {
        MatchPatternCatalog::new(
            &HashSet::from(["Option".to_string(), "Result".to_string()]),
            &HashMap::from([
                ("Some".to_string(), vec!["Option".to_string()]),
                ("Ok".to_string(), vec!["Result".to_string()]),
            ]),
        )
    }

    #[test]
    fn function_call_callee_is_a_lexical_reference() {
        let callable = let_binding(10, "callable");
        let nested = closure(
            30,
            Vec::new(),
            vec![function_call(31, "callable"), function_call(33, "log")],
        );

        let captured = captured(&[callable.clone(), nested]);
        assert!(captured.contains(&BindingId::from_declaration("callable", callable.span)));
    }

    #[test]
    fn earlier_value_binding_shadows_later_hoisted_callable_for_capture() {
        let callable = let_binding(10, "callable");
        let invoke = node(
            20,
            Node::ConstBinding {
                pattern: BindingPattern::Identifier("invoke".to_string()),
                type_ann: None,
                value: Box::new(closure(21, Vec::new(), vec![function_call(22, "callable")])),
                is_pub: false,
            },
        );
        let later_callable = fn_decl(30, "callable", Vec::new());

        let captured = captured(&[callable.clone(), invoke, later_callable]);
        assert_eq!(
            captured,
            HashSet::from([BindingId::from_declaration("callable", callable.span)])
        );
    }

    #[test]
    fn deferred_module_callable_sees_later_module_value() {
        let read = fn_decl(10, "read", vec![identifier(11, "counter")]);
        let counter = let_binding(20, "counter");

        let captured = captured_bindings_in_compiled_module(
            &[read, counter.clone()],
            &MatchPatternCatalog::default(),
        );

        assert_eq!(
            captured,
            HashSet::from([BindingId::from_declaration("counter", counter.span)])
        );
    }

    #[test]
    fn module_statement_does_not_see_later_module_value() {
        let early = node(
            10,
            Node::ConstBinding {
                pattern: BindingPattern::Identifier("read".to_string()),
                type_ann: None,
                value: Box::new(closure(11, Vec::new(), vec![identifier(12, "counter")])),
                is_pub: false,
            },
        );
        let counter = let_binding(20, "counter");

        let captured = captured_bindings_in_compiled_module(
            &[early, counter],
            &MatchPatternCatalog::default(),
        );

        assert!(captured.is_empty());
    }

    #[test]
    fn match_bindings_shadow_same_named_outer_mutables() {
        let pin = let_binding(10, "pin");
        let alias = let_binding(20, "alias");
        let rest = let_binding(30, "rest");
        let match_expr = node(
            40,
            Node::MatchExpr {
                value: Box::new(identifier(41, "value")),
                arms: vec![
                    MatchArm {
                        pattern: identifier(42, "pin"),
                        guard: Some(Box::new(identifier(43, "pin"))),
                        body: vec![identifier(44, "pin")],
                        span: Span::with_offsets(42, 47, 1, 43),
                    },
                    MatchArm {
                        pattern: node(
                            50,
                            Node::DictLiteral(vec![DictEntry {
                                key: node(51, Node::StringLiteral("key".to_string())),
                                value: identifier(52, "alias"),
                            }]),
                        ),
                        guard: None,
                        body: vec![identifier(53, "alias")],
                        span: Span::with_offsets(50, 55, 1, 51),
                    },
                    MatchArm {
                        pattern: node(
                            60,
                            Node::ListLiteral(vec![
                                identifier(61, "pin"),
                                node(62, Node::Spread(Box::new(identifier(63, "rest")))),
                            ]),
                        ),
                        guard: None,
                        body: vec![identifier(64, "pin"), identifier(65, "rest")],
                        span: Span::with_offsets(60, 67, 1, 61),
                    },
                    MatchArm {
                        pattern: node(
                            70,
                            Node::FunctionCall {
                                name: "Some".to_string(),
                                type_args: Vec::new(),
                                args: vec![identifier(71, "alias")],
                            },
                        ),
                        guard: None,
                        body: vec![identifier(72, "alias")],
                        span: Span::with_offsets(70, 74, 1, 71),
                    },
                    MatchArm {
                        pattern: node(
                            80,
                            Node::MethodCall {
                                object: Box::new(identifier(81, "Result")),
                                method: "Ok".to_string(),
                                args: vec![identifier(82, "rest")],
                            },
                        ),
                        guard: None,
                        body: vec![identifier(83, "rest")],
                        span: Span::with_offsets(80, 85, 1, 81),
                    },
                ],
            },
        );

        let nested = closure(39, Vec::new(), vec![match_expr]);
        let captured = captured_bindings_in_nested_callables(
            &[pin.clone(), alias.clone(), rest.clone(), nested],
            &enum_pattern_catalog(),
        );
        assert!(!captured.contains(&BindingId::from_declaration("pin", pin.span)));
        assert!(!captured.contains(&BindingId::from_declaration("alias", alias.span)));
        assert!(!captured.contains(&BindingId::from_declaration("rest", rest.span)));
    }

    #[test]
    fn unresolved_call_patterns_capture_expression_references() {
        let callable = let_binding(10, "callable");
        let object = let_binding(20, "object");
        let argument = let_binding(30, "argument");
        let match_expr = node(
            40,
            Node::MatchExpr {
                value: Box::new(identifier(41, "value")),
                arms: vec![
                    MatchArm {
                        pattern: node(
                            42,
                            Node::FunctionCall {
                                name: "callable".to_string(),
                                type_args: Vec::new(),
                                args: vec![identifier(43, "argument")],
                            },
                        ),
                        guard: None,
                        body: Vec::new(),
                        span: Span::with_offsets(42, 44, 1, 43),
                    },
                    MatchArm {
                        pattern: node(
                            45,
                            Node::MethodCall {
                                object: Box::new(identifier(46, "object")),
                                method: "compute".to_string(),
                                args: vec![identifier(47, "argument")],
                            },
                        ),
                        guard: None,
                        body: Vec::new(),
                        span: Span::with_offsets(45, 48, 1, 46),
                    },
                ],
            },
        );
        let nested = closure(39, Vec::new(), vec![match_expr]);

        let captured = captured(&[callable.clone(), object.clone(), argument.clone(), nested]);
        assert!(captured.contains(&BindingId::from_declaration("callable", callable.span)));
        assert!(captured.contains(&BindingId::from_declaration("object", object.span)));
        assert!(captured.contains(&BindingId::from_declaration("argument", argument.span)));
    }

    #[test]
    fn qualified_enum_constant_pattern_does_not_capture_enum_name() {
        let result = let_binding(10, "Result");
        let match_expr = node(
            20,
            Node::MatchExpr {
                value: Box::new(identifier(21, "value")),
                arms: vec![MatchArm {
                    pattern: node(
                        22,
                        Node::PropertyAccess {
                            object: Box::new(identifier(23, "Result")),
                            property: "Ok".to_string(),
                        },
                    ),
                    guard: None,
                    body: Vec::new(),
                    span: Span::with_offsets(22, 25, 1, 23),
                }],
            },
        );
        let nested = closure(19, Vec::new(), vec![match_expr]);

        let captured = captured_bindings_in_nested_callables(
            &[result.clone(), nested],
            &enum_pattern_catalog(),
        );
        assert!(!captured.contains(&BindingId::from_declaration("Result", result.span)));
    }

    #[test]
    fn parameter_defaults_see_only_earlier_parameters() {
        let first = let_binding(10, "first");
        let current = let_binding(20, "current");
        let later = let_binding(30, "later");
        let nested = closure(
            40,
            vec![
                defaulted_param("first", identifier(41, "later")),
                defaulted_param("current", identifier(42, "current")),
                defaulted_param("later", identifier(43, "first")),
            ],
            Vec::new(),
        );

        let captured = captured(&[first.clone(), current.clone(), later.clone(), nested]);
        assert!(!captured.contains(&BindingId::from_declaration("first", first.span)));
        assert!(captured.contains(&BindingId::from_declaration("current", current.span)));
        assert!(captured.contains(&BindingId::from_declaration("later", later.span)));
    }

    #[test]
    fn parameter_shadow_does_not_capture_outer_binding() {
        let outer = let_binding(10, "pin");
        let body = vec![
            outer.clone(),
            closure(
                20,
                vec![TypedParam::untyped("pin")],
                vec![identifier(21, "pin")],
            ),
        ];

        assert!(!captured(&body).contains(&BindingId::from_declaration("pin", outer.span)));
    }

    #[test]
    fn block_shadow_captures_exact_inner_binding() {
        let outer = let_binding(10, "pin");
        let inner = let_binding(20, "pin");
        let body = vec![
            outer.clone(),
            node(
                19,
                Node::Block(vec![
                    inner.clone(),
                    closure(30, Vec::new(), vec![identifier(31, "pin")]),
                ]),
            ),
        ];

        let captured = captured(&body);
        assert!(captured.contains(&BindingId::from_declaration("pin", inner.span)));
        assert!(!captured.contains(&BindingId::from_declaration("pin", outer.span)));
    }

    #[test]
    fn later_block_binding_does_not_shadow_an_earlier_reference() {
        let outer = let_binding(10, "pin");
        let inner = let_binding(30, "pin");
        let body = vec![
            outer.clone(),
            node(
                19,
                Node::Block(vec![
                    closure(20, Vec::new(), vec![identifier(21, "pin")]),
                    inner.clone(),
                ]),
            ),
        ];

        let captured = captured(&body);
        assert!(captured.contains(&BindingId::from_declaration("pin", outer.span)));
        assert!(!captured.contains(&BindingId::from_declaration("pin", inner.span)));
    }

    #[test]
    fn loop_binding_shadows_outer_capture() {
        let outer = let_binding(10, "pin");
        let loop_node = node(
            20,
            Node::ForIn {
                pattern: BindingPattern::Identifier("pin".to_string()),
                iterable: Box::new(identifier(21, "pins")),
                body: vec![closure(22, Vec::new(), vec![identifier(23, "pin")])],
            },
        );
        let captured = captured(&[outer.clone(), loop_node.clone()]);

        assert!(captured.contains(&BindingId::from_declaration("pin", loop_node.span)));
        assert!(!captured.contains(&BindingId::from_declaration("pin", outer.span)));
    }

    #[test]
    fn catch_and_select_bindings_shadow_outer_capture() {
        let outer = let_binding(10, "pin");
        let try_catch = node(
            20,
            Node::TryCatch {
                body: Vec::new(),
                has_catch: true,
                error_var: Some("pin".to_string()),
                error_type: None,
                catch_body: vec![closure(21, Vec::new(), vec![identifier(22, "pin")])],
                finally_body: None,
            },
        );
        let select = node(
            30,
            Node::SelectExpr {
                cases: vec![SelectCase {
                    variable: "pin".to_string(),
                    channel: Box::new(identifier(31, "channel")),
                    body: vec![closure(32, Vec::new(), vec![identifier(33, "pin")])],
                }],
                timeout: None,
                default_body: None,
            },
        );

        let captured = captured(&[outer.clone(), try_catch, select]);
        assert!(!captured.contains(&BindingId::from_declaration("pin", outer.span)));
    }

    #[test]
    fn nested_callable_capture_is_transitive() {
        let outer = let_binding(10, "pin");
        let nested = closure(
            20,
            Vec::new(),
            vec![closure(30, Vec::new(), vec![identifier(31, "pin")])],
        );
        let captured = captured(&[outer.clone(), nested]);

        assert!(captured.contains(&BindingId::from_declaration("pin", outer.span)));
    }

    #[test]
    fn nested_reassignment_ignores_shadowed_parameter() {
        let body = vec![closure(
            10,
            vec![TypedParam::untyped("pin")],
            vec![node(
                11,
                Node::Assignment {
                    target: Box::new(identifier(12, "pin")),
                    value: Box::new(identifier(13, "next")),
                    op: None,
                },
            )],
        )];

        assert!(
            nested_callable_reassigned_names(&body, &MatchPatternCatalog::default()).is_empty()
        );
    }

    #[test]
    fn nested_reassignment_ignores_enum_payload_binding() {
        let assignment = node(
            14,
            Node::Assignment {
                target: Box::new(identifier(15, "pin")),
                value: Box::new(identifier(16, "next")),
                op: None,
            },
        );
        let body = vec![node(
            10,
            Node::MatchExpr {
                value: Box::new(identifier(11, "value")),
                arms: vec![MatchArm {
                    pattern: node(
                        12,
                        Node::FunctionCall {
                            name: "Some".to_string(),
                            type_args: Vec::new(),
                            args: vec![identifier(13, "pin")],
                        },
                    ),
                    guard: None,
                    body: vec![closure(14, Vec::new(), vec![assignment])],
                    span: Span::with_offsets(12, 18, 1, 13),
                }],
            },
        )];

        assert_eq!(
            nested_callable_reassigned_names(&body, &enum_pattern_catalog()),
            Vec::<String>::new()
        );
    }
}
