//! Flow-sensitive narrowing: refinement extraction and exhaustiveness checks.
//!
//! `extract_refinements` is the dispatch entry — given a condition AST node
//! it yields a `Refinements` describing the narrowings to apply on the
//! truthy and falsy branches. The supporting `extract_*_refinements`
//! helpers cover the specific patterns the type checker recognises:
//! `x != nil`, `type_of(x) == "T"`, `x.has("k")`, `schema_is(x, S)`, and
//! their negations.
//!
//! Match exhaustiveness and the `unknown`-variant fallback check live here
//! too, sharing the same flow facts that refinement extraction populates.

use crate::ast::*;
use crate::diagnostic_codes::Code;
use harn_lexer::Span;

use super::super::exits::block_definitely_exits;
use super::super::schema_inference::schema_type_expr_from_node;
use super::super::scope::{PathNarrowing, Refinements, TypeScope};
use super::super::union::{
    discriminant_field, extract_type_of_arg, intersect_types, narrow_shape_union_by_tag,
    narrow_to_single, reference_path_key, remove_from_union, subtract_type, DiscriminantValue,
};
use super::super::TypeChecker;

/// Flatten a match-arm pattern into its leaf alternatives. For an
/// `OrPattern(a, b, c)` this yields `[a, b, c]`; for any other pattern
/// node it yields a single-element iterator over the pattern itself.
/// Exhaustiveness checks and arm-level narrowing treat each alternative
/// as an independent "sub-arm" that contributes to coverage.
pub(in crate::typechecker) fn pattern_alternatives(p: &SNode) -> Vec<&SNode> {
    match &p.node {
        Node::OrPattern(alts) => alts.iter().collect(),
        _ => vec![p],
    }
}

/// Resolve every member of a union through the `Named`-alias chain so
/// downstream shape-union helpers (`discriminant_field`,
/// `narrow_shape_union_by_tag`) see concrete shapes. Without this,
/// `type Ping = {kind:"ping",…}; type Msg = Ping | {kind:"pong",…}`
/// wouldn't be recognised as a tagged shape union — the `Named("Ping")`
/// member would fail the bare-`Shape` check. Only simple (non-
/// parameterised) alias chains are unwrapped here; generic aliases
/// still route through `TypeChecker::resolve_alias` at the narrowing
/// call sites that have `&self` access.
pub(in crate::typechecker) fn resolve_union_shape_members(
    members: &[TypeExpr],
    scope: &TypeScope,
) -> Vec<TypeExpr> {
    members
        .iter()
        .map(|m| resolve_named_alias_chain(m.clone(), scope))
        .collect()
}

/// Walk through `Named(alias)` indirections in `scope.type_aliases` to a
/// concrete type. Stops as soon as the alias body is something other than
/// another `Named` reference, or the lookup fails. The parameterised
/// distribution path still lives in `TypeChecker::resolve_alias`; this
/// flow-only helper exists because the refinement extractors are
/// associated functions without access to `&self`.
fn resolve_named_alias_chain(ty: TypeExpr, scope: &TypeScope) -> TypeExpr {
    let mut current = ty;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let TypeExpr::Named(name) = &current else {
            return current;
        };
        if seen.iter().any(|s| s == name) {
            return current;
        }
        seen.push(name.clone());
        match scope.resolve_type(name) {
            Some(body) => current = body.clone(),
            None => return current,
        }
    }
}

/// Split a property access into its object node and property name, for both
/// plain and optional access. Returns `None` for non-property-access nodes.
/// Unlike a bare-identifier extractor this keeps the full object node, so the
/// caller can narrow a reference path (`o.msg.kind`) as well as a variable.
fn property_access_parts(node: &SNode) -> Option<(&SNode, String)> {
    match &node.node {
        Node::PropertyAccess { object, property }
        | Node::OptionalPropertyAccess { object, property } => Some((object, property.clone())),
        _ => None,
    }
}

/// The runtime kinds that `type_of(...)` can return and that the refinement
/// logic knows how to map to a `TypeExpr` member kind. Sourced from the
/// canonical tag registry in `harn-builtin-meta`, which `harn-vm` asserts
/// against `VmValue::type_name`, so the narrower and the runtime cannot
/// drift. Shared by `if`/`guard` `type_of` narrowing and `match type_of(…)`
/// arm narrowing.
fn is_type_of_tag(tag: &str) -> bool {
    harn_builtin_meta::runtime_type_tags::is_narrowable_tag(tag)
}

/// The literal `TypeExpr` for a discriminant value, used to build the
/// `{tag: literal}` schema that drives path-based discriminant narrowing.
fn discriminant_literal_type(value: &DiscriminantValue) -> TypeExpr {
    match value {
        DiscriminantValue::Str(s) => TypeExpr::LitString(s.clone()),
        DiscriminantValue::Int(v) => TypeExpr::LitInt(*v),
    }
}

/// Walk an *optional*-access chain (`o?.a`, `o?.[i]`, `o?.m()`, and nestings
/// thereof) down to the base identifier it is rooted on. Returns `None`
/// unless `node` is itself an optional access whose object resolves — through
/// further optional links only — to a bare identifier.
///
/// Soundness: by optional-chaining semantics `obj?.x` (and `?.[]`/`?.()`)
/// evaluates to `nil` whenever `obj` is `nil`. So if the *outermost* optional
/// access is non-nil, its object was non-nil; applying that fact down a chain
/// of optional links proves the base identifier is non-nil. We deliberately
/// do **not** descend through *non-optional* links (`o.a?.b`), since a plain
/// `.a` being read tells the type system nothing it is willing to assume.
fn optional_chain_base_identifier(node: &SNode) -> Option<&str> {
    fn root(node: &SNode) -> Option<&str> {
        match &node.node {
            Node::Identifier(name) => Some(name.as_str()),
            Node::OptionalPropertyAccess { object, .. }
            | Node::OptionalSubscriptAccess { object, .. }
            | Node::OptionalMethodCall { object, .. } => root(object),
            _ => None,
        }
    }
    match &node.node {
        Node::OptionalPropertyAccess { object, .. }
        | Node::OptionalSubscriptAccess { object, .. }
        | Node::OptionalMethodCall { object, .. } => root(object),
        _ => None,
    }
}

/// Project a literal expression node to a [`DiscriminantValue`]. Only the
/// literal kinds eligible as tagged-shape-union discriminants
/// (`StringLiteral`, `IntLiteral`) are recognised here.
fn discriminant_value_from_node(node: &SNode) -> Option<DiscriminantValue> {
    match &node.node {
        Node::StringLiteral(s) => Some(DiscriminantValue::Str(s.clone())),
        Node::IntLiteral(v) => Some(DiscriminantValue::Int(*v)),
        _ => None,
    }
}

fn format_discriminant(v: &DiscriminantValue) -> String {
    match v {
        DiscriminantValue::Str(s) => format!("\"{s}\""),
        DiscriminantValue::Int(v) => v.to_string(),
    }
}

enum PatternCoverage<T> {
    Covers(T),
    Wildcard,
    NoCoverage,
    Unknown,
}

enum MatchCoverageSubject {
    Bool,
    Enum(String),
    TaggedShapeUnion,
    LiteralUnion,
    NamedUnion,
}

struct MatchCoverage {
    subject: MatchCoverageSubject,
    missing: Vec<String>,
    has_wildcard: bool,
    analyzable: bool,
}

impl MatchCoverage {
    fn is_exhaustive(&self) -> bool {
        self.has_wildcard || self.missing.is_empty()
    }

    fn should_diagnose(&self) -> bool {
        self.analyzable && !self.is_exhaustive()
    }
}

fn collect_arm_pattern_coverage<T: PartialEq>(
    arms: &[MatchArm],
    mut classify: impl FnMut(&SNode) -> PatternCoverage<T>,
) -> (Vec<T>, bool, bool) {
    let mut covered = Vec::new();
    let mut has_wildcard = false;
    let mut analyzable = true;
    for arm in arms.iter().filter(|arm| arm.guard.is_none()) {
        for leaf in pattern_alternatives(&arm.pattern) {
            match classify(leaf) {
                PatternCoverage::Covers(value) => {
                    if !covered.contains(&value) {
                        covered.push(value);
                    }
                }
                PatternCoverage::Wildcard => has_wildcard = true,
                PatternCoverage::NoCoverage => {}
                PatternCoverage::Unknown => analyzable = false,
            }
        }
    }
    (covered, has_wildcard, analyzable)
}

fn missing_expected<T: PartialEq>(
    expected: &[T],
    covered: &[T],
    format: impl Fn(&T) -> String,
) -> Vec<String> {
    expected
        .iter()
        .filter(|value| !covered.contains(value))
        .map(format)
        .collect()
}

/// Whether a condition is a bare scalar literal (no operator wrapping).
/// Used by the vacuous-condition lint to skip the `if true { … }` /
/// `if false { … }` block-scope idiom while still flagging compound
/// expressions that fold to the same constant (`if !true`, `if (a || true)`,
/// `if (false && cond)`, …) — the latter almost always signal a partial
/// refactor that left dead code behind, the former is intentional.
fn is_bare_literal(node: &SNode) -> bool {
    matches!(
        &node.node,
        Node::BoolLiteral(_)
            | Node::NilLiteral
            | Node::IntLiteral(_)
            | Node::FloatLiteral(_)
            | Node::StringLiteral(_)
            | Node::RawStringLiteral(_)
    )
}

/// Compile-time evaluation of a boolean condition over literal operands and
/// the short-circuit / negation rules. Returns `Some(value)` only when the
/// truthiness is determinable from the AST alone — leaves are limited to
/// literal forms (matching `VmValue::is_truthy` semantics for the same node
/// kinds), and the recursion mirrors how `&&` / `||` short-circuit at
/// runtime. Returns `None` for any sub-expression whose truthiness depends
/// on a runtime value.
fn evaluate_constant_bool(condition: &SNode) -> Option<bool> {
    match &condition.node {
        Node::BoolLiteral(b) => Some(*b),
        Node::NilLiteral => Some(false),
        Node::IntLiteral(v) => Some(*v != 0),
        Node::FloatLiteral(v) => Some(*v != 0.0),
        Node::StringLiteral(s) | Node::RawStringLiteral(s) => Some(!s.is_empty()),
        Node::BinaryOp { op, left, right } if op == "&&" || op == "||" => {
            let lv = evaluate_constant_bool(left);
            let rv = evaluate_constant_bool(right);
            if op == "&&" {
                match (lv, rv) {
                    // Either side known-falsy collapses `&&` to falsy.
                    (Some(false), _) | (_, Some(false)) => Some(false),
                    (Some(true), Some(true)) => Some(true),
                    _ => None,
                }
            } else {
                match (lv, rv) {
                    // Either side known-truthy collapses `||` to truthy.
                    (Some(true), _) | (_, Some(true)) => Some(true),
                    (Some(false), Some(false)) => Some(false),
                    _ => None,
                }
            }
        }
        Node::UnaryOp { op, operand } if op == "!" => evaluate_constant_bool(operand).map(|b| !b),
        _ => None,
    }
}

/// Names of variables assigned (plain or compound) anywhere in the same
/// enclosing callable body. Nested control flow is scanned; nested callables are
/// skipped because their assignments do not execute as part of the current
/// branch/loop flow.
pub(in crate::typechecker) fn assigned_var_names(body: &[SNode]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for node in body {
        collect_assigned_var_names(node, &mut names);
    }
    names
}

fn collect_assigned_var_names(node: &SNode, names: &mut Vec<String>) {
    match &node.node {
        Node::Assignment { target, .. } => {
            if let Node::Identifier(name) = &target.node {
                if !names.iter().any(|n| n == name) {
                    names.push(name.clone());
                }
            }
        }
        Node::Closure { .. }
        | Node::FnDecl { .. }
        | Node::ToolDecl { .. }
        | Node::Pipeline { .. }
        | Node::OverrideDecl { .. } => return,
        _ => {}
    }
    for child in crate::visit::immediate_children(node) {
        collect_assigned_var_names(child, names);
    }
}

/// Variables reassigned *inside a nested closure* within `body` — the mirror of
/// [`assigned_var_names`], which deliberately stops at closure boundaries.
///
/// Post-#4479 closures capture by reference, so a closure that reassigns an
/// outer variable can reset it (e.g. to nil) when it is later called. Any
/// flow-narrowing on such a variable is therefore unsound to keep. The callable
/// pre-marks this set on its body scope so `apply_refinements` never narrows
/// them — the conservative, TypeScript/Flow-aligned "assigned in a nested
/// function ⇒ not narrowed" rule. See harn#4523.
pub(in crate::typechecker) fn vars_reassigned_in_nested_closures(
    body: &[SNode],
    match_patterns: &crate::lexical::MatchPatternCatalog,
) -> Vec<String> {
    crate::lexical::nested_callable_reassigned_names(body, match_patterns)
}

impl TypeChecker {
    /// Invalidate every narrowing (variable or reference path) whose subject is
    /// reassigned in a branch or loop body that can continue in the current
    /// callable.
    /// Pre-mark, on a fresh callable body scope, every variable a nested
    /// closure reassigns, so its flow-narrowing is suppressed for the whole
    /// body. Call once at each callable entry (fn/pipeline/tool/closure) before
    /// its statements are checked. See [`vars_reassigned_in_nested_closures`].
    pub(in crate::typechecker) fn mark_closure_mutated_captures(
        scope: &mut TypeScope,
        body: &[SNode],
    ) {
        let match_patterns = scope.lexical_match_pattern_catalog();
        for name in vars_reassigned_in_nested_closures(body, &match_patterns) {
            scope.mark_closure_mutated(&name);
        }
    }

    pub(in crate::typechecker) fn invalidate_assigned_narrowings(
        scope: &mut TypeScope,
        body: &[SNode],
    ) {
        for name in assigned_var_names(body) {
            if let Some(original) = scope.narrowed_original(&name).cloned() {
                scope.narrowed_vars.remove(&name);
                scope.define_var(&name, original);
            }
            scope.clear_narrowed_paths_rooted_at(&name);
            scope.clear_unknown_ruled_out_paths_rooted_at(&name);
        }
    }

    /// Wrap `extract_refinements` with the [`Code::LintVacuousCondition`]
    /// emission pass. Callers that own `&mut self` (every `if` / `while` /
    /// `guard` site) should prefer this over the bare associated form so the
    /// lint fires consistently. The `&self`-only call site in `infer_type`'s
    /// ternary arm still calls the bare form — a small, deliberate coverage
    /// gap that mirrors how the rest of the typechecker emits diagnostics.
    pub(in crate::typechecker) fn extract_refinements_with_lint(
        &mut self,
        condition: &SNode,
        scope: &TypeScope,
    ) -> Refinements {
        self.lint_vacuous_condition(condition, scope);
        self.extract_refinements(condition, scope)
    }

    /// Walk a boolean condition reporting `HARN-LNT-058`. Two patterns fire:
    ///
    /// 1. The whole subexpression reduces to a constant via
    ///    [`evaluate_constant_bool`] — but only when there's compound
    ///    folding involved (an operator). A bare `if true { … }` or
    ///    `if false { … }` is left alone: in Harn it's the canonical
    ///    block-scope / disable-block idiom, the conformance suite uses
    ///    it intentionally, and the unreachable-code analysis already
    ///    handles "this branch never runs" diagnostics for those bare
    ///    cases. One warning at the subexpression's span; no descent
    ///    (children are dead either way).
    /// 2. The subexpression is `schema_is(x, S)` / `is_type(x, S)` and
    ///    `x`'s static type makes the predicate's answer known.
    ///
    /// `&&` and `||` re-enter with the refined scope so a nested predicate
    /// sees the narrowings the left-hand operand established.
    fn lint_vacuous_condition(&mut self, condition: &SNode, scope: &TypeScope) {
        if !is_bare_literal(condition) {
            if let Some(value) = evaluate_constant_bool(condition) {
                let (state, dead_branch) = if value {
                    ("truthy", "falsy")
                } else {
                    ("falsy", "truthy")
                };
                self.warning_at(
                    Code::LintVacuousCondition,
                    format!("condition is statically always {state}; the {dead_branch} branch is unreachable"),
                    condition.span,
                );
                return;
            }
        }
        match &condition.node {
            Node::BinaryOp { op, left, right } if op == "&&" || op == "||" => {
                self.lint_vacuous_condition(left, scope);
                let left_ref = self.extract_refinements(left, scope);
                let mut right_scope = scope.child();
                if op == "&&" {
                    left_ref.apply_truthy(&mut right_scope);
                } else {
                    left_ref.apply_falsy(&mut right_scope);
                }
                self.lint_vacuous_condition(right, &right_scope);
            }
            Node::UnaryOp { op, operand } if op == "!" => {
                self.lint_vacuous_condition(operand, scope);
            }
            Node::FunctionCall { name, args, .. }
                if (name == "schema_is" || name == "is_type") && args.len() == 2 =>
            {
                self.check_vacuous_schema_call(name, args, condition.span, scope);
            }
            _ => {}
        }
    }

    fn check_vacuous_schema_call(
        &mut self,
        name: &str,
        args: &[SNode],
        span: Span,
        scope: &TypeScope,
    ) {
        let Node::Identifier(var_name) = &args[0].node else {
            return;
        };
        let Some(schema_type) = schema_type_expr_from_node(&args[1], scope) else {
            return;
        };
        let Some(Some(var_type)) = scope.get_var(var_name).cloned() else {
            return;
        };

        // Open top types — schema_is is genuinely informative here.
        let resolved_var = self.resolve_alias(&var_type, scope);
        if matches!(&resolved_var, TypeExpr::Named(n) if n == "unknown" || n == "any") {
            return;
        }

        if self.schema_is_definitely_satisfied(&resolved_var, &schema_type, scope) {
            self.emit_vacuous_schema_warning(name, var_name, span, true);
            return;
        }

        // `intersect_types == None` is the existing check the refinement
        // extractor uses to collapse the truthy branch, so reusing it keeps
        // the lint perfectly aligned with the narrower's view.
        if intersect_types(&resolved_var, &schema_type).is_none() {
            self.emit_vacuous_schema_warning(name, var_name, span, false);
        }
    }

    fn emit_vacuous_schema_warning(
        &mut self,
        name: &str,
        var_name: &str,
        span: Span,
        always_true: bool,
    ) {
        let (verdict, reason, dead_branch) = if always_true {
            (
                "always true",
                "is statically known to match the schema",
                "falsy",
            )
        } else {
            (
                "always false",
                "'s static type cannot match the schema",
                "truthy",
            )
        };
        self.warning_at(
            Code::LintVacuousCondition,
            format!(
                "`{name}({var_name}, …)` is {verdict} here: `{var_name}`{reason}, so the {dead_branch} branch is unreachable"
            ),
            span,
        );
    }

    /// Strict structural subtype check used by the vacuous-schema lint.
    /// Unlike `types_compatible`, this REJECTS optional-vs-required field
    /// mismatches on shapes — `schema_is` is a runtime presence check, so
    /// `{b: string?}` is *not* a guaranteed subtype of `{b: string}` (the
    /// runtime value can lack `b`). All other relations defer to
    /// `types_compatible`, which already handles unions, named-type aliases,
    /// numeric widening, and literal-vs-base intersections.
    fn schema_is_definitely_satisfied(
        &self,
        var_type: &TypeExpr,
        schema_type: &TypeExpr,
        scope: &TypeScope,
    ) -> bool {
        let var = self.resolve_alias(var_type, scope);
        let schema = self.resolve_alias(schema_type, scope);

        // Reject open top types on either side.
        if matches!(&var, TypeExpr::Named(n) if n == "unknown" || n == "any")
            || matches!(&schema, TypeExpr::Named(n) if n == "unknown" || n == "any")
        {
            return false;
        }

        match (&var, &schema) {
            (TypeExpr::Shape(actual_fields), TypeExpr::Shape(expected_fields)) => {
                expected_fields.iter().all(|expected| {
                    if expected.optional {
                        return true;
                    }
                    actual_fields.iter().any(|actual| {
                        actual.name == expected.name
                            && !actual.optional
                            && self.schema_is_definitely_satisfied(
                                &actual.type_expr,
                                &expected.type_expr,
                                scope,
                            )
                    })
                })
            }
            (TypeExpr::Union(members), _) => members
                .iter()
                .all(|m| self.schema_is_definitely_satisfied(m, &schema, scope)),
            _ => self.types_compatible(&schema, &var, scope),
        }
    }

    /// Extract bidirectional type refinements from a condition expression.
    pub(in crate::typechecker) fn extract_refinements(
        &self,
        condition: &SNode,
        scope: &TypeScope,
    ) -> Refinements {
        match &condition.node {
            Node::BinaryOp { op, left, right } if op == "!=" || op == "==" => {
                let nil_ref = self.extract_nil_refinements(op, left, right, scope);
                if !nil_ref.is_empty() {
                    return nil_ref;
                }
                let typeof_ref = self.extract_typeof_refinements(op, left, right, scope);
                if !typeof_ref.is_empty() {
                    return typeof_ref;
                }
                let tag_ref = self.extract_discriminator_refinements(op, left, right, scope);
                if !tag_ref.is_empty() {
                    return tag_ref;
                }
                Refinements::empty()
            }

            // Logical AND: both operands must be truthy, so truthy refinements compose.
            Node::BinaryOp { op, left, right } if op == "&&" => {
                let left_ref = self.extract_refinements(left, scope);
                let mut right_scope = scope.child();
                left_ref.apply_truthy(&mut right_scope);
                let right_ref = self.extract_refinements(right, &right_scope);
                let mut truthy = left_ref.truthy;
                truthy.extend(right_ref.truthy);
                let mut truthy_paths = left_ref.truthy_paths;
                truthy_paths.extend(right_ref.truthy_paths);
                let mut truthy_ruled_out = left_ref.truthy_ruled_out;
                truthy_ruled_out.extend(right_ref.truthy_ruled_out);
                Refinements {
                    truthy,
                    falsy: vec![],
                    truthy_paths,
                    falsy_paths: vec![],
                    truthy_ruled_out,
                    falsy_ruled_out: vec![],
                }
            }

            // Logical OR: both operands must be falsy for the whole to be falsy.
            Node::BinaryOp { op, left, right } if op == "||" => {
                let left_ref = self.extract_refinements(left, scope);
                let mut right_scope = scope.child();
                left_ref.apply_falsy(&mut right_scope);
                let right_ref = self.extract_refinements(right, &right_scope);
                let mut falsy = left_ref.falsy;
                falsy.extend(right_ref.falsy);
                let mut falsy_paths = left_ref.falsy_paths;
                falsy_paths.extend(right_ref.falsy_paths);
                let mut falsy_ruled_out = left_ref.falsy_ruled_out;
                falsy_ruled_out.extend(right_ref.falsy_ruled_out);
                Refinements {
                    truthy: vec![],
                    falsy,
                    truthy_paths: vec![],
                    falsy_paths,
                    truthy_ruled_out: vec![],
                    falsy_ruled_out,
                }
            }

            Node::UnaryOp { op, operand } if op == "!" => {
                self.extract_refinements(operand, scope).inverted()
            }

            // Bare identifier in condition position: narrow `T | nil` to `T`.
            Node::Identifier(name) => {
                if let Some(Some(TypeExpr::Union(members))) = scope.get_var(name) {
                    if members
                        .iter()
                        .any(|m| matches!(m, TypeExpr::Named(n) if n == "nil"))
                    {
                        if let Some(narrowed) = remove_from_union(members, "nil") {
                            return Refinements {
                                truthy: vec![(name.clone(), Some(narrowed))],
                                falsy: vec![(name.clone(), Some(TypeExpr::Named("nil".into())))],
                                ..Refinements::default()
                            };
                        }
                    }
                }
                Refinements::empty()
            }

            // Bare reference path in condition position (`if entry.arguments`,
            // `if xs[0]`): a truthy value is non-nil, so remove `nil` from the
            // path's type on the truthy branch. The falsy branch stays open —
            // `false`/`0`/`""` are falsy without being `nil`.
            Node::PropertyAccess { .. }
            | Node::OptionalPropertyAccess { .. }
            | Node::SubscriptAccess { .. }
            | Node::OptionalSubscriptAccess { .. } => {
                if let Some(key) = reference_path_key(condition) {
                    return Refinements {
                        truthy_paths: vec![(key, PathNarrowing::Remove("nil".into()))],
                        ..Refinements::default()
                    };
                }
                Refinements::empty()
            }

            Node::MethodCall {
                object,
                method,
                args,
            } if method == "has" && args.len() == 1 => {
                self.extract_has_refinements(object, args, scope)
            }

            Node::FunctionCall { name, args, .. }
                if (name == "schema_is" || name == "is_type") && args.len() == 2 =>
            {
                self.extract_schema_refinements(args, scope)
            }

            _ => Refinements::empty(),
        }
    }

    /// Extract nil-check refinements from `x != nil` / `x == nil` patterns.
    fn extract_nil_refinements(
        &self,
        op: &str,
        left: &SNode,
        right: &SNode,
        scope: &TypeScope,
    ) -> Refinements {
        let var_node = if matches!(right.node, Node::NilLiteral) {
            left
        } else if matches!(left.node, Node::NilLiteral) {
            right
        } else {
            return Refinements::empty();
        };

        if let Node::Identifier(name) = &var_node.node {
            let var_type = scope.get_var(name).cloned().flatten();
            match var_type {
                Some(TypeExpr::Union(ref members)) => {
                    if let Some(narrowed) = remove_from_union(members, "nil") {
                        let neq_refs = Refinements {
                            truthy: vec![(name.clone(), Some(narrowed))],
                            falsy: vec![(name.clone(), Some(TypeExpr::Named("nil".into())))],
                            ..Refinements::default()
                        };
                        return if op == "!=" {
                            neq_refs
                        } else {
                            neq_refs.inverted()
                        };
                    }
                }
                Some(TypeExpr::Named(ref n)) if n == "nil" => {
                    // Single nil type: == nil is always true, != nil narrows to never.
                    let eq_refs = Refinements {
                        truthy: vec![(name.clone(), Some(TypeExpr::Named("nil".into())))],
                        falsy: vec![(name.clone(), Some(TypeExpr::Never))],
                        ..Refinements::default()
                    };
                    return if op == "==" {
                        eq_refs
                    } else {
                        eq_refs.inverted()
                    };
                }
                _ => {}
            }
            // A bare identifier never participates in path narrowing — return
            // here so an unmatched scalar (`x: int`) doesn't fall through and
            // get treated as a single-segment reference path.
            return Refinements::empty();
        }

        // Non-identifier subject (a property path like `o.a` / `o?.a`). Two
        // independent, sound facts combine on the "chain is non-nil" branch:
        let mut refs = Refinements::default();

        // (1) `o?.a != nil` (and `?.[]`/`?.()` chains) proves the *base*
        // identifier is non-nil — by optional-chaining semantics the chain
        // is `nil` whenever the base is. This only refines the non-nil branch
        // (`o?.a == nil` is satisfiable with `o` non-nil but `o.a` nil).
        if let Some(base) = optional_chain_base_identifier(var_node) {
            if let Some(TypeExpr::Union(members)) = scope.get_var(base).cloned().flatten() {
                if let Some(narrowed) = remove_from_union(&members, "nil") {
                    refs.truthy.push((base.to_string(), Some(narrowed)));
                }
            }
        }

        // (2) The reference path itself: `path != nil` removes `nil` from the
        // path's natural type on the truthy branch and pins it to `nil` on the
        // falsy branch. Deferred via `PathNarrowing` so reads re-derive from the
        // path's freshly computed type (see `infer_property_access_type`).
        if let Some(key) = reference_path_key(var_node) {
            refs.truthy_paths
                .push((key.clone(), PathNarrowing::Remove("nil".into())));
            refs.falsy_paths
                .push((key, PathNarrowing::Keep("nil".into())));
        }

        if refs.truthy.is_empty() && refs.truthy_paths.is_empty() && refs.falsy_paths.is_empty() {
            return Refinements::empty();
        }
        if op == "!=" {
            refs
        } else {
            refs.inverted()
        }
    }

    /// Extract type_of refinements from `type_of(x) == "typename"` patterns.
    fn extract_typeof_refinements(
        &self,
        op: &str,
        left: &SNode,
        right: &SNode,
        scope: &TypeScope,
    ) -> Refinements {
        let (arg, type_name) = if let (Some(a), Node::StringLiteral(tn)) =
            (extract_type_of_arg(left), &right.node)
        {
            (a, tn.clone())
        } else if let (Node::StringLiteral(tn), Some(a)) = (&left.node, extract_type_of_arg(right))
        {
            (a, tn.clone())
        } else {
            return Refinements::empty();
        };

        if !is_type_of_tag(&type_name) {
            return Refinements::empty();
        }

        // `type_of(<property path>) == "T"` — e.g. `type_of(entry?.arguments)`.
        // Bare identifiers are handled by the variable logic below; anything
        // that is a stable property path narrows via a deferred directive that
        // `infer_property_access_type` re-applies to the path's natural type.
        let Node::Identifier(var_name) = &arg.node else {
            if let Some(key) = reference_path_key(arg) {
                let mut eq_refs = Refinements {
                    truthy_paths: vec![(key.clone(), PathNarrowing::Keep(type_name.clone()))],
                    falsy_paths: vec![(key.clone(), PathNarrowing::Remove(type_name.clone()))],
                    ..Refinements::default()
                };
                // When the path is itself a top type, the `==` falsy branch
                // can't subtract a concrete kind from it (it stays open), but
                // we record the ruled-out tag so an exhaustive `type_of`
                // chain on the path can be validated at `unreachable()` /
                // `throw` — exactly as for an `unknown`-typed variable.
                if self.path_type_is_top(arg, scope) {
                    eq_refs.falsy_ruled_out = vec![(key, type_name)];
                }
                return if op == "==" {
                    eq_refs
                } else {
                    eq_refs.inverted()
                };
            }
            return Refinements::empty();
        };
        let var_name = var_name.clone();

        let var_type = scope.get_var(&var_name).cloned().flatten();
        match var_type {
            Some(TypeExpr::Union(ref members)) => {
                let narrowed = narrow_to_single(members, &type_name);
                let remaining = remove_from_union(members, &type_name);
                if narrowed.is_some() || remaining.is_some() {
                    let eq_refs = Refinements {
                        truthy: narrowed
                            .map(|n| vec![(var_name.clone(), Some(n))])
                            .unwrap_or_default(),
                        falsy: remaining
                            .map(|r| vec![(var_name.clone(), Some(r))])
                            .unwrap_or_default(),
                        ..Refinements::default()
                    };
                    return if op == "==" {
                        eq_refs
                    } else {
                        eq_refs.inverted()
                    };
                }
            }
            Some(TypeExpr::Named(ref n)) if n == "unknown" => {
                // `unknown` narrows to the tested concrete type on the truthy
                // branch. The falsy branch keeps `unknown` — subtracting one
                // concrete type from an open top still leaves an open top —
                // but we remember which concrete variants have been ruled
                // out so `unreachable()` / `throw` can detect incomplete
                // exhaustive-narrowing chains.
                let eq_refs = Refinements {
                    truthy: vec![(var_name.clone(), Some(TypeExpr::Named(type_name.clone())))],
                    falsy_ruled_out: vec![(var_name, type_name)],
                    ..Refinements::default()
                };
                return if op == "==" {
                    eq_refs
                } else {
                    eq_refs.inverted()
                };
            }
            Some(ref ty) => {
                // Single (non-union) type: reuse the union helpers on a
                // one-element slice so parameterised constructors like
                // `list<int>` narrow the same way that `Named("list")`
                // does inside a union (`type_of(x) == "list"` →
                // truthy = list<int>, falsy = never). Without this,
                // single-typed parameterised values silently bypass
                // narrowing and the falsy branch retains the original
                // type even though it is provably unreachable.
                let single = std::slice::from_ref(ty);
                let narrowed = narrow_to_single(single, &type_name);
                let remaining = remove_from_union(single, &type_name);
                if narrowed.is_some() {
                    let eq_refs = Refinements {
                        truthy: narrowed
                            .map(|n| vec![(var_name.clone(), Some(n))])
                            .unwrap_or_default(),
                        falsy: remaining
                            .map(|r| vec![(var_name.clone(), Some(r))])
                            .unwrap_or_default(),
                        ..Refinements::default()
                    };
                    return if op == "==" {
                        eq_refs
                    } else {
                        eq_refs.inverted()
                    };
                }
            }
            _ => {}
        }
        Refinements::empty()
    }

    /// Extract refinements from `obj.<tag> == "value"` / `obj.<tag> == 7` style
    /// patterns where `obj` is a tagged shape union and `<tag>` is the union's
    /// auto-detected discriminant field. `obj` may be a bare variable or a
    /// reference path (`o.msg.kind == "ping"`). Truthy narrows `obj` to the
    /// matching variant; falsy narrows to the residual union. The
    /// tagged-shape-union gate keeps this from narrowing arbitrary
    /// `path.field == literal` comparisons (which would otherwise mangle a
    /// `dict`/`unknown` object into a closed one-field shape).
    fn extract_discriminator_refinements(
        &self,
        op: &str,
        left: &SNode,
        right: &SNode,
        scope: &TypeScope,
    ) -> Refinements {
        // Find which side is the property access and which is the literal.
        let (obj_node, tag_field, tag_value) = match (
            property_access_parts(left),
            discriminant_value_from_node(right),
        ) {
            (Some((obj, field)), Some(value)) => (obj, field, value),
            _ => match (
                property_access_parts(right),
                discriminant_value_from_node(left),
            ) {
                (Some((obj, field)), Some(value)) => (obj, field, value),
                _ => return Refinements::empty(),
            },
        };

        // Resolve the object's type to a union of shapes. A bare variable
        // reads from the scope (preserving the historical resolution path);
        // a reference path is inferred (needs `&self`).
        let resolved = if let Node::Identifier(name) = &obj_node.node {
            let Some(Some(raw_type)) = scope.get_var(name).cloned() else {
                return Refinements::empty();
            };
            resolve_named_alias_chain(raw_type, scope)
        } else {
            let Some(obj_type) = self.infer_type(obj_node, scope) else {
                return Refinements::empty();
            };
            self.resolve_alias(&obj_type, scope)
        };
        let TypeExpr::Union(members) = resolved else {
            return Refinements::empty();
        };
        let members = resolve_union_shape_members(&members, scope);
        let Some(detected) = discriminant_field(&members) else {
            return Refinements::empty();
        };
        if detected != tag_field {
            return Refinements::empty();
        }
        let Some((matched, residual)) = narrow_shape_union_by_tag(&members, &tag_field, &tag_value)
        else {
            return Refinements::empty();
        };

        let eq_refs = if let Node::Identifier(var_name) = &obj_node.node {
            // Variable: store the resolved matched/residual types directly.
            let falsy = match residual {
                TypeExpr::Never => vec![(var_name.clone(), Some(TypeExpr::Never))],
                other => vec![(var_name.clone(), Some(other))],
            };
            Refinements {
                truthy: vec![(var_name.clone(), Some(matched))],
                falsy,
                ..Refinements::default()
            }
        } else if let Some(key) = reference_path_key(obj_node) {
            // Path: defer via `{tag: literal}` intersect/subtract, which
            // re-derives the matched variant / residual union from the path's
            // natural type at read time.
            let schema = TypeExpr::Shape(vec![ShapeField::synthetic(
                tag_field,
                discriminant_literal_type(&tag_value),
                false,
            )]);
            Refinements {
                truthy_paths: vec![(key.clone(), PathNarrowing::Intersect(schema.clone()))],
                falsy_paths: vec![(key, PathNarrowing::Subtract(schema))],
                ..Refinements::default()
            }
        } else {
            return Refinements::empty();
        };
        if op == "==" {
            eq_refs
        } else {
            eq_refs.inverted()
        }
    }

    /// Extract `.has("key")` refinements: the presence check makes the named
    /// field required on the truthy branch. Works on a bare variable (narrow
    /// its `Shape` to make the field required) or a reference path (deferred
    /// `Intersect` with a `{key: any}` schema, which `intersect_shapes` folds
    /// into making the field required at read time).
    fn extract_has_refinements(
        &self,
        object: &SNode,
        args: &[SNode],
        scope: &TypeScope,
    ) -> Refinements {
        let Node::StringLiteral(key) = &args[0].node else {
            return Refinements::empty();
        };
        if let Node::Identifier(var_name) = &object.node {
            if let Some(Some(TypeExpr::Shape(fields))) = scope.get_var(var_name) {
                if fields.iter().any(|f| f.name == *key && f.optional) {
                    let narrowed_fields: Vec<ShapeField> = fields
                        .iter()
                        .map(|f| {
                            if f.name == *key {
                                ShapeField {
                                    name: f.name.clone(),
                                    type_expr: f.type_expr.clone(),
                                    optional: false,
                                    span: f.span,
                                }
                            } else {
                                f.clone()
                            }
                        })
                        .collect();
                    return Refinements {
                        truthy: vec![(var_name.clone(), Some(TypeExpr::Shape(narrowed_fields)))],
                        ..Refinements::default()
                    };
                }
            }
            return Refinements::empty();
        }
        if let Some(path_key) = reference_path_key(object) {
            // `{key: any}` required: intersecting with the path's shape marks
            // the field required (and drops `nil` members) at read time.
            let schema = TypeExpr::Shape(vec![ShapeField::synthetic(
                key.clone(),
                TypeExpr::Named("any".into()),
                false,
            )]);
            return Refinements {
                truthy_paths: vec![(path_key, PathNarrowing::Intersect(schema))],
                ..Refinements::default()
            };
        }
        Refinements::empty()
    }

    /// Extract `schema_is(x, S)` / `is_type(x, S)` refinements for a bare
    /// variable (resolved intersect/subtract) or a reference path (deferred
    /// `Intersect`/`Subtract` directives re-applied to the path's type).
    fn extract_schema_refinements(&self, args: &[SNode], scope: &TypeScope) -> Refinements {
        let Some(schema_type) = schema_type_expr_from_node(&args[1], scope) else {
            return Refinements::empty();
        };
        if let Node::Identifier(var_name) = &args[0].node {
            let Some(Some(var_type)) = scope.get_var(var_name).cloned() else {
                return Refinements::empty();
            };
            let truthy = intersect_types(&var_type, &schema_type)
                .map(|ty| vec![(var_name.clone(), Some(ty))])
                .unwrap_or_default();
            let falsy = subtract_type(&var_type, &schema_type)
                .map(|ty| vec![(var_name.clone(), Some(ty))])
                .unwrap_or_default();
            return Refinements {
                truthy,
                falsy,
                ..Refinements::default()
            };
        }
        if let Some(key) = reference_path_key(&args[0]) {
            return Refinements {
                truthy_paths: vec![(key.clone(), PathNarrowing::Intersect(schema_type.clone()))],
                falsy_paths: vec![(key, PathNarrowing::Subtract(schema_type))],
                ..Refinements::default()
            };
        }
        Refinements::empty()
    }

    /// Narrow the subject of a `match type_of(subject) { "tag" -> … }` arm.
    /// When the match scrutinee is `type_of(subject)` and an arm matches a
    /// single runtime-kind literal, the subject is narrowed to that kind in
    /// the arm scope — the `match` counterpart of `if type_of(subject) == "T"`.
    /// `subject` may be a variable (narrow its union, or a top type to the
    /// tag) or a reference path (deferred `Keep`). A no-op for any other
    /// scrutinee, so it is safe to call at every match-arm site.
    pub(in crate::typechecker) fn narrow_match_subject(
        &self,
        value: &SNode,
        pattern: &SNode,
        scope: &mut TypeScope,
    ) {
        let Some(subject) = extract_type_of_arg(value) else {
            return;
        };
        // Only a single concrete runtime-kind literal narrows; or-patterns over
        // several tags, wildcards, and non-literal patterns leave it un-narrowed.
        let leaves = pattern_alternatives(pattern);
        let [leaf] = leaves.as_slice() else {
            return;
        };
        let Node::StringLiteral(tag) = &leaf.node else {
            return;
        };
        if !is_type_of_tag(tag) {
            return;
        }

        match &subject.node {
            Node::Identifier(name) => {
                let narrowed = match scope.get_var(name).cloned().flatten() {
                    Some(TypeExpr::Union(members)) => narrow_to_single(&members, tag),
                    Some(TypeExpr::Named(n)) if n == "unknown" || n == "any" => {
                        Some(TypeExpr::Named(tag.clone()))
                    }
                    Some(other) => narrow_to_single(std::slice::from_ref(&other), tag),
                    None => None,
                };
                if let Some(narrowed) = narrowed {
                    scope.define_var(name, Some(narrowed));
                }
            }
            _ => {
                if let Some(key) = reference_path_key(subject) {
                    scope.set_narrowed_path(&key, PathNarrowing::Keep(tag.clone()));
                }
            }
        }
    }

    /// Check whether a block definitely exits (delegates to the free function).
    pub(in crate::typechecker) fn block_definitely_exits(stmts: &[SNode]) -> bool {
        block_definitely_exits(stmts)
    }

    pub(in crate::typechecker) fn match_is_exhaustive(
        &self,
        value: &SNode,
        arms: &[MatchArm],
        scope: &TypeScope,
    ) -> bool {
        self.match_coverage(value, arms, scope)
            .is_some_and(|coverage| coverage.is_exhaustive())
    }

    fn match_enum_name(&self, value: &SNode, scope: &TypeScope) -> Option<String> {
        let ty = match &value.node {
            Node::PropertyAccess { object, property } if property == "variant" => {
                self.infer_type(object, scope)?
            }
            _ => self.infer_type(value, scope)?,
        };
        match self.resolve_alias(&ty, scope) {
            TypeExpr::Named(name) | TypeExpr::Applied { name, .. }
                if scope.get_enum(&name).is_some() =>
            {
                Some(name)
            }
            _ => None,
        }
    }

    fn match_coverage(
        &self,
        value: &SNode,
        arms: &[MatchArm],
        scope: &TypeScope,
    ) -> Option<MatchCoverage> {
        if let Some(enum_name) = self.match_enum_name(value, scope) {
            return self.enum_match_coverage(&enum_name, arms, scope);
        }
        if let Some(coverage) = self.tagged_shape_match_coverage(value, arms, scope) {
            return Some(coverage);
        }
        if let Some(coverage) = self.bool_match_coverage(value, arms, scope) {
            return Some(coverage);
        }
        self.union_match_coverage(value, arms, scope)
    }

    fn enum_match_coverage(
        &self,
        enum_name: &str,
        arms: &[MatchArm],
        scope: &TypeScope,
    ) -> Option<MatchCoverage> {
        let enum_info = scope.get_enum(enum_name)?;
        let variant_names: Vec<String> = enum_info
            .variants
            .iter()
            .map(|variant| variant.name.clone())
            .collect();
        let (covered, has_wildcard, analyzable) =
            collect_arm_pattern_coverage(arms, |leaf| match &leaf.node {
                Node::StringLiteral(name) if variant_names.contains(name) => {
                    PatternCoverage::Covers(name.clone())
                }
                Node::EnumConstruct { variant, .. }
                | Node::PropertyAccess {
                    property: variant, ..
                }
                | Node::MethodCall {
                    method: variant, ..
                } if variant_names.contains(variant) => PatternCoverage::Covers(variant.clone()),
                Node::FunctionCall { name: variant, .. } if variant_names.contains(variant) => {
                    PatternCoverage::Covers(variant.clone())
                }
                Node::Identifier(_) => PatternCoverage::Wildcard,
                Node::StringLiteral(_) => PatternCoverage::NoCoverage,
                _ => PatternCoverage::Unknown,
            });
        let missing =
            missing_expected(&variant_names, &covered, |variant| format!("\"{variant}\""));
        Some(MatchCoverage {
            subject: MatchCoverageSubject::Enum(enum_name.to_string()),
            missing,
            has_wildcard,
            analyzable,
        })
    }

    fn tagged_shape_match_coverage(
        &self,
        value: &SNode,
        arms: &[MatchArm],
        scope: &TypeScope,
    ) -> Option<MatchCoverage> {
        let Node::PropertyAccess { object, property } = &value.node else {
            return None;
        };
        let Node::Identifier(obj_var) = &object.node else {
            return None;
        };
        let Some(Some(raw_type)) = scope.get_var(obj_var).cloned() else {
            return None;
        };
        let TypeExpr::Union(members) = self.resolve_alias(&raw_type, scope) else {
            return None;
        };
        let members = resolve_union_shape_members(&members, scope);
        if discriminant_field(&members).as_deref() != Some(property.as_str()) {
            return None;
        }

        let expected: Vec<DiscriminantValue> = members
            .iter()
            .filter_map(|member| {
                let TypeExpr::Shape(fields) = member else {
                    return None;
                };
                fields
                    .iter()
                    .find(|field| field.name == *property)
                    .and_then(|field| DiscriminantValue::from_type(&field.type_expr))
            })
            .collect();
        let (covered, has_wildcard, analyzable) =
            collect_arm_pattern_coverage(arms, |leaf| match &leaf.node {
                Node::StringLiteral(value) => {
                    PatternCoverage::Covers(DiscriminantValue::Str(value.clone()))
                }
                Node::IntLiteral(value) => PatternCoverage::Covers(DiscriminantValue::Int(*value)),
                Node::Identifier(_) => PatternCoverage::Wildcard,
                _ => PatternCoverage::Unknown,
            });
        let missing = missing_expected(&expected, &covered, format_discriminant);
        Some(MatchCoverage {
            subject: MatchCoverageSubject::TaggedShapeUnion,
            missing,
            has_wildcard,
            analyzable,
        })
    }

    fn bool_match_coverage(
        &self,
        value: &SNode,
        arms: &[MatchArm],
        scope: &TypeScope,
    ) -> Option<MatchCoverage> {
        let is_bool = matches!(
            self.infer_type(value, scope)
                .map(|ty| self.resolve_alias(&ty, scope)),
            Some(TypeExpr::Named(n)) if n == "bool"
        );
        if !is_bool {
            return None;
        }
        let (covered, has_wildcard, analyzable) =
            collect_arm_pattern_coverage(arms, |leaf| match &leaf.node {
                Node::BoolLiteral(true) => PatternCoverage::Covers("true"),
                Node::BoolLiteral(false) => PatternCoverage::Covers("false"),
                Node::Identifier(_) => PatternCoverage::Wildcard,
                _ => PatternCoverage::Unknown,
            });
        let expected = ["true", "false"];
        let missing = missing_expected(&expected, &covered, |name| (*name).to_string());
        Some(MatchCoverage {
            subject: MatchCoverageSubject::Bool,
            missing,
            has_wildcard,
            analyzable,
        })
    }

    fn union_match_coverage(
        &self,
        value: &SNode,
        arms: &[MatchArm],
        scope: &TypeScope,
    ) -> Option<MatchCoverage> {
        let inferred = self.infer_type(value, scope)?;
        let TypeExpr::Union(members) = self.resolve_alias(&inferred, scope) else {
            return None;
        };

        if members
            .iter()
            .all(|member| matches!(member, TypeExpr::LitString(_) | TypeExpr::LitInt(_)))
        {
            let expected: Vec<DiscriminantValue> = members
                .iter()
                .filter_map(DiscriminantValue::from_type)
                .collect();
            let (covered, has_wildcard, analyzable) =
                collect_arm_pattern_coverage(arms, |leaf| match &leaf.node {
                    Node::StringLiteral(value) => {
                        PatternCoverage::Covers(DiscriminantValue::Str(value.clone()))
                    }
                    Node::IntLiteral(value) => {
                        PatternCoverage::Covers(DiscriminantValue::Int(*value))
                    }
                    Node::Identifier(_) => PatternCoverage::Wildcard,
                    _ => PatternCoverage::Unknown,
                });
            let missing = missing_expected(&expected, &covered, format_discriminant);
            return Some(MatchCoverage {
                subject: MatchCoverageSubject::LiteralUnion,
                missing,
                has_wildcard,
                analyzable,
            });
        }

        if !members
            .iter()
            .all(|member| matches!(member, TypeExpr::Named(_)))
        {
            return None;
        }

        let expected: Vec<String> = members
            .iter()
            .filter_map(|member| match member {
                TypeExpr::Named(name) => Some(name.clone()),
                _ => None,
            })
            .collect();
        let (covered, has_wildcard, analyzable) =
            collect_arm_pattern_coverage(arms, |leaf| match &leaf.node {
                Node::NilLiteral => PatternCoverage::Covers("nil".to_string()),
                Node::BoolLiteral(_) => PatternCoverage::Covers("bool".to_string()),
                Node::IntLiteral(_) => PatternCoverage::Covers("int".to_string()),
                Node::FloatLiteral(_) => PatternCoverage::Covers("float".to_string()),
                Node::StringLiteral(_) => PatternCoverage::Covers("string".to_string()),
                Node::Identifier(_) => PatternCoverage::Wildcard,
                _ => PatternCoverage::Unknown,
            });
        let missing = missing_expected(&expected, &covered, |name| name.clone());
        Some(MatchCoverage {
            subject: MatchCoverageSubject::NamedUnion,
            missing,
            has_wildcard,
            analyzable,
        })
    }

    pub(in crate::typechecker) fn check_match_exhaustiveness(
        &mut self,
        value: &SNode,
        arms: &[MatchArm],
        scope: &TypeScope,
        span: Span,
    ) {
        let Some(coverage) = self.match_coverage(value, arms, scope) else {
            return;
        };
        if !coverage.should_diagnose() {
            return;
        }
        self.emit_match_coverage_diagnostic(&coverage, span);
    }

    fn emit_match_coverage_diagnostic(&mut self, coverage: &MatchCoverage, span: Span) {
        let missing = coverage.missing.join(", ");
        let message = match &coverage.subject {
            MatchCoverageSubject::Bool => {
                format!("Non-exhaustive match on bool: missing {missing}")
            }
            MatchCoverageSubject::Enum(enum_name) => {
                format!("Non-exhaustive match on enum {enum_name}: missing variants {missing}")
            }
            MatchCoverageSubject::TaggedShapeUnion => {
                format!("Non-exhaustive match on tagged shape union: missing variants {missing}")
            }
            MatchCoverageSubject::LiteralUnion => {
                format!("Non-exhaustive match on literal union: missing {missing}")
            }
            MatchCoverageSubject::NamedUnion => {
                format!("Non-exhaustive match on union type: missing {missing}")
            }
        };
        self.exhaustiveness_error_with_missing(
            Code::NonExhaustiveMatch,
            message,
            span,
            coverage.missing.clone(),
        );
    }

    /// The `type_of` variants an `unknown` value must rule out before an
    /// exhaustive-narrowing chain counts as complete. Sourced from the
    /// canonical tag registry in `harn-builtin-meta` (the boundary-API
    /// JSON-representable subset — see `UNKNOWN_COVERAGE` there).
    const UNKNOWN_CONCRETE_TYPES: &'static [&'static str] =
        harn_builtin_meta::runtime_type_tags::UNKNOWN_COVERAGE;

    /// Whether a reference node's type resolves to a top type (`unknown` /
    /// `any`) — the precondition for path-based `type_of` exhaustiveness
    /// tracking, mirroring the `unknown`-typed-variable case.
    fn path_type_is_top(&self, node: &SNode, scope: &TypeScope) -> bool {
        matches!(
            self.infer_type(node, scope).map(|t| self.resolve_alias(&t, scope)),
            Some(TypeExpr::Named(n)) if n == "unknown" || n == "any"
        )
    }

    /// Emit a warning if any `unknown`-typed variable or reference path in
    /// scope has been partially narrowed via `type_of(v) == "T"` checks but
    /// the current control-flow path reaches a never-returning site
    /// (`unreachable()`, a function with `Never` return, or a `throw`)
    /// without covering every concrete `type_of` variant.
    ///
    /// The ruled-out set must be non-empty — reaching `throw`/`unreachable`
    /// without any narrowing isn't an exhaustiveness claim, so it stays
    /// silent and avoids false positives on plain error paths.
    pub(in crate::typechecker) fn check_unknown_exhaustiveness(
        &mut self,
        scope: &TypeScope,
        span: Span,
        site_label: &str,
    ) {
        let entries = scope.collect_unknown_ruled_out();
        for (var_name, covered) in entries {
            if covered.is_empty() {
                continue;
            }
            // A path key (`o.x`, `xs[0]`) can't be re-typed from its string
            // form, but the ledger entry only exists because the path was a
            // top type at the guards; a fully-narrowed chain rules out all
            // nine variants, leaving `missing` empty. A bare variable must
            // still be `unknown` — otherwise the ruled-out set is stale.
            let is_path = var_name.contains('.') || var_name.contains('[');
            if !is_path
                && !matches!(
                    scope.get_var(&var_name),
                    Some(Some(TypeExpr::Named(n))) if n == "unknown"
                )
            {
                continue;
            }
            let missing: Vec<&str> = Self::UNKNOWN_CONCRETE_TYPES
                .iter()
                .copied()
                .filter(|t| !covered.iter().any(|c| c == t))
                .collect();
            if missing.is_empty() {
                continue;
            }
            let missing_str = missing
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            self.warning_at(Code::NonExhaustiveMatch,
                format!(
                    "`{site_label}` reached but `{var_name}: unknown` was not fully narrowed — uncovered concrete type(s): {missing_str}",
                ),
                span,
            );
        }
    }
}
