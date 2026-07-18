use harn_parser::{BindingPattern, Node, SNode, TypeExpr, TypedParam};

use crate::chunk::{Op, ParamSlot};

use super::Compiler;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrimitiveType {
    Int,
    Float,
    Bool,
    String,
    Nil,
}

impl Compiler {
    /// Build runtime parameter metadata from the compiler's normalized types.
    ///
    /// Parser annotations retain source-level alias names for diagnostics.
    /// Runtime guards must instead carry the recursively expanded shape so an
    /// imported or cached callable enforces `type Count = int` as `int`.
    pub(super) fn compile_param_slots(&self, params: &[TypedParam]) -> Vec<ParamSlot> {
        params
            .iter()
            .map(|param| {
                let type_expr = param
                    .type_expr
                    .as_ref()
                    .map(|type_expr| self.expand_alias(type_expr));
                ParamSlot::from_typed_param_with_type(param, type_expr)
            })
            .collect()
    }

    pub(super) fn record_param_types(&mut self, params: &[TypedParam]) {
        for param in params {
            if let Some(type_expr) = &param.type_expr {
                self.define_type_fact(&param.name, type_expr.clone());
            }
        }
    }

    pub(super) fn record_binding_type(
        &mut self,
        pattern: &BindingPattern,
        type_expr: Option<TypeExpr>,
    ) {
        match pattern {
            BindingPattern::Identifier(name) => {
                if let Some(type_expr) = type_expr {
                    self.define_type_fact(name, type_expr);
                }
            }
            BindingPattern::Dict(fields) => {
                let Some(TypeExpr::Shape(shape_fields)) = type_expr else {
                    return;
                };
                for field in fields.iter().filter(|field| !field.is_rest) {
                    let Some(shape_field) =
                        shape_fields.iter().find(|shape| shape.name == field.key)
                    else {
                        continue;
                    };
                    let binding_name = field.alias.as_deref().unwrap_or(&field.key);
                    self.define_type_fact(binding_name, shape_field.type_expr.clone());
                }
            }
            BindingPattern::List(elements) => {
                let Some(TypeExpr::List(item_type)) = type_expr else {
                    return;
                };
                for element in elements {
                    let element_type = if element.is_rest {
                        TypeExpr::List(item_type.clone())
                    } else {
                        (*item_type).clone()
                    };
                    self.define_type_fact(&element.name, element_type);
                }
            }
            BindingPattern::Pair(first, second) => {
                let Some(TypeExpr::Applied { name, args }) = type_expr else {
                    return;
                };
                if name == "Pair" && args.len() == 2 {
                    self.define_type_fact(first, args[0].clone());
                    self.define_type_fact(second, args[1].clone());
                }
            }
        }
    }

    pub(super) fn assign_type_fact(&mut self, name: &str, type_expr: Option<TypeExpr>) {
        if let Some(type_expr) = type_expr {
            let type_expr = self.expand_alias(&type_expr);
            for scope in self.type_scopes.iter_mut().rev() {
                if let Some(existing) = scope.get_mut(name) {
                    if *existing == type_expr {
                        return;
                    }
                    let existing_kind = Self::primitive_kind(existing);
                    let new_kind = Self::primitive_kind(&type_expr);
                    if existing_kind.is_some() && existing_kind == new_kind {
                        *existing = type_expr;
                    } else {
                        scope.remove(name);
                    }
                    return;
                }
            }
        } else {
            for scope in self.type_scopes.iter_mut().rev() {
                if scope.remove(name).is_some() {
                    return;
                }
            }
        }
    }

    pub(super) fn infer_expr_type(&self, expr: &SNode) -> Option<TypeExpr> {
        match &expr.node {
            Node::IntLiteral(_) => Some(TypeExpr::Named("int".into())),
            Node::FloatLiteral(_) => Some(TypeExpr::Named("float".into())),
            Node::StringLiteral(_) | Node::RawStringLiteral(_) | Node::InterpolatedString(_) => {
                Some(TypeExpr::Named("string".into()))
            }
            Node::BoolLiteral(_) => Some(TypeExpr::Named("bool".into())),
            Node::NilLiteral => Some(TypeExpr::Named("nil".into())),
            Node::DurationLiteral(_) => Some(TypeExpr::Named("duration".into())),
            Node::Identifier(name) => self.lookup_type_fact(name),
            Node::UnaryOp { op, operand } => {
                let operand_type = self.infer_expr_type(operand)?;
                match op.as_str() {
                    "-" if matches!(
                        Self::primitive_kind(&self.expand_alias(&operand_type)),
                        Some(PrimitiveType::Int | PrimitiveType::Float)
                    ) =>
                    {
                        Some(operand_type)
                    }
                    "!" => Some(TypeExpr::Named("bool".into())),
                    _ => None,
                }
            }
            Node::BinaryOp { op, left, right } => {
                let left_type = self.infer_expr_type(left);
                let right_type = self.infer_expr_type(right);
                self.infer_binary_result_type(op, left_type.as_ref(), right_type.as_ref())
            }
            Node::Ternary {
                true_expr,
                false_expr,
                ..
            } => {
                let true_type = self.infer_expr_type(true_expr)?;
                let false_type = self.infer_expr_type(false_expr)?;
                if true_type == false_type {
                    Some(true_type)
                } else {
                    None
                }
            }
            Node::ListLiteral(items) => self.infer_list_literal_type(items),
            Node::DictLiteral(entries) => {
                let mut fields = Vec::new();
                for entry in entries {
                    let key = match &entry.key.node {
                        Node::Identifier(key) | Node::StringLiteral(key) => key.clone(),
                        _ => return Some(TypeExpr::Named("dict".into())),
                    };
                    let Some(type_expr) = self.infer_expr_type(&entry.value) else {
                        return Some(TypeExpr::Named("dict".into()));
                    };
                    fields.push(harn_parser::ShapeField {
                        name: key,
                        type_expr,
                        optional: false,
                    });
                }
                if fields.is_empty() {
                    Some(TypeExpr::Named("dict".into()))
                } else {
                    Some(TypeExpr::Shape(fields))
                }
            }
            Node::RangeExpr { .. } => Some(TypeExpr::Named("range".into())),
            _ => None,
        }
    }

    pub(super) fn infer_for_item_type(&self, iterable: &SNode) -> Option<TypeExpr> {
        match self.infer_expr_type(iterable)? {
            TypeExpr::List(item)
            | TypeExpr::Iter(item)
            | TypeExpr::Generator(item)
            | TypeExpr::Stream(item) => Some(*item),
            TypeExpr::DictType(key, value) => Some(TypeExpr::Applied {
                name: "Pair".into(),
                args: vec![*key, *value],
            }),
            TypeExpr::Named(name) if name == "range" => Some(TypeExpr::Named("int".into())),
            _ => None,
        }
    }

    pub(super) fn specialized_binary_op(
        &self,
        op: &str,
        left: Option<&TypeExpr>,
        right: Option<&TypeExpr>,
    ) -> Option<Op> {
        let left = Self::primitive_kind(&self.expand_alias(left?))?;
        let right = Self::primitive_kind(&self.expand_alias(right?))?;
        match (left, right) {
            (PrimitiveType::Int, PrimitiveType::Int) => match op {
                "+" => Some(Op::AddInt),
                "-" => Some(Op::SubInt),
                "*" => Some(Op::MulInt),
                "/" => Some(Op::DivInt),
                "%" => Some(Op::ModInt),
                "==" => Some(Op::EqualInt),
                "!=" => Some(Op::NotEqualInt),
                "<" => Some(Op::LessInt),
                ">" => Some(Op::GreaterInt),
                "<=" => Some(Op::LessEqualInt),
                ">=" => Some(Op::GreaterEqualInt),
                _ => None,
            },
            (PrimitiveType::Float, PrimitiveType::Float) => match op {
                "+" => Some(Op::AddFloat),
                "-" => Some(Op::SubFloat),
                "*" => Some(Op::MulFloat),
                "/" => Some(Op::DivFloat),
                "%" => Some(Op::ModFloat),
                "==" => Some(Op::EqualFloat),
                "!=" => Some(Op::NotEqualFloat),
                "<" => Some(Op::LessFloat),
                ">" => Some(Op::GreaterFloat),
                "<=" => Some(Op::LessEqualFloat),
                ">=" => Some(Op::GreaterEqualFloat),
                _ => None,
            },
            (PrimitiveType::Bool, PrimitiveType::Bool) => match op {
                "==" => Some(Op::EqualBool),
                "!=" => Some(Op::NotEqualBool),
                _ => None,
            },
            (PrimitiveType::String, PrimitiveType::String) => match op {
                "==" => Some(Op::EqualString),
                "!=" => Some(Op::NotEqualString),
                _ => None,
            },
            _ => None,
        }
    }

    fn define_type_fact(&mut self, name: &str, type_expr: TypeExpr) {
        if harn_parser::is_discard_name(name) {
            return;
        }
        let type_expr = self.expand_alias(&type_expr);
        if let Some(scope) = self.type_scopes.last_mut() {
            scope.insert(name.to_string(), type_expr);
        }
    }

    fn lookup_type_fact(&self, name: &str) -> Option<TypeExpr> {
        self.type_scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).cloned())
    }

    fn infer_list_literal_type(&self, items: &[SNode]) -> Option<TypeExpr> {
        let mut item_type: Option<TypeExpr> = None;
        for item in items {
            let inferred = self.infer_expr_type(item)?;
            item_type = Some(match item_type {
                None => inferred,
                Some(current) if current == inferred => current,
                Some(_) => return Some(TypeExpr::Named("list".into())),
            });
        }
        Some(TypeExpr::List(Box::new(
            item_type.unwrap_or_else(|| TypeExpr::Named("_".into())),
        )))
    }

    pub(super) fn infer_binary_result_type(
        &self,
        op: &str,
        left: Option<&TypeExpr>,
        right: Option<&TypeExpr>,
    ) -> Option<TypeExpr> {
        if matches!(op, "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||") {
            return Some(TypeExpr::Named("bool".into()));
        }
        let left = self.expand_alias(left?);
        let right = self.expand_alias(right?);
        let left_kind = Self::primitive_kind(&left);
        let right_kind = Self::primitive_kind(&right);

        match op {
            "+" => match (left_kind, right_kind) {
                (Some(PrimitiveType::Int), Some(PrimitiveType::Int)) => {
                    Some(TypeExpr::Named("int".into()))
                }
                (Some(PrimitiveType::Float), Some(PrimitiveType::Float))
                | (Some(PrimitiveType::Int), Some(PrimitiveType::Float))
                | (Some(PrimitiveType::Float), Some(PrimitiveType::Int)) => {
                    Some(TypeExpr::Named("float".into()))
                }
                (Some(PrimitiveType::String), Some(PrimitiveType::String)) => {
                    Some(TypeExpr::Named("string".into()))
                }
                _ => None,
            },
            "-" | "/" | "%" | "**" => match (left_kind, right_kind) {
                (Some(PrimitiveType::Int), Some(PrimitiveType::Int)) => {
                    Some(TypeExpr::Named("int".into()))
                }
                (Some(PrimitiveType::Float), Some(PrimitiveType::Float))
                | (Some(PrimitiveType::Int), Some(PrimitiveType::Float))
                | (Some(PrimitiveType::Float), Some(PrimitiveType::Int)) => {
                    Some(TypeExpr::Named("float".into()))
                }
                _ => None,
            },
            "*" => match (left_kind, right_kind) {
                (Some(PrimitiveType::Int), Some(PrimitiveType::Int)) => {
                    Some(TypeExpr::Named("int".into()))
                }
                (Some(PrimitiveType::Float), Some(PrimitiveType::Float))
                | (Some(PrimitiveType::Int), Some(PrimitiveType::Float))
                | (Some(PrimitiveType::Float), Some(PrimitiveType::Int)) => {
                    Some(TypeExpr::Named("float".into()))
                }
                (Some(PrimitiveType::String), Some(PrimitiveType::Int))
                | (Some(PrimitiveType::Int), Some(PrimitiveType::String)) => {
                    Some(TypeExpr::Named("string".into()))
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Canonical primitive tag (`"int"` / `"float"` / `"bool"` / `"string"`)
    /// for a type expression that resolves to one of the four
    /// specialization-relevant primitives, else `None`. `nil` is excluded
    /// because no typed opcode specializes on it. Used by the monomorphic-let
    /// analysis and the mutable-binding gate.
    pub(super) fn primitive_type_tag(&self, type_expr: &TypeExpr) -> Option<&'static str> {
        match Self::primitive_kind(&self.expand_alias(type_expr)) {
            Some(PrimitiveType::Int) => Some("int"),
            Some(PrimitiveType::Float) => Some("float"),
            Some(PrimitiveType::Bool) => Some("bool"),
            Some(PrimitiveType::String) => Some("string"),
            Some(PrimitiveType::Nil) | None => None,
        }
    }

    /// Drop an initializer-inferred *primitive* type for a reassignable binding
    /// (`let` / `for`-item) that the monomorphic analysis did not prove safe, so
    /// typed-opcode specialization stays sound. Non-primitive types and
    /// proven-monomorphic bindings pass through unchanged.
    pub(super) fn gate_mutable_primitive_type(
        &self,
        span: harn_lexer::Span,
        type_expr: Option<TypeExpr>,
    ) -> Option<TypeExpr> {
        if !self.options.optimizations_enabled() {
            // No typed-opcode specialization happens, and the monomorphic set is
            // never populated — leave facts byte-identical to the pre-gate path.
            return type_expr;
        }
        match &type_expr {
            Some(t)
                if self.primitive_type_tag(t).is_some()
                    && !self.monomorphic_bindings.contains(&(span.start, span.end)) =>
            {
                None
            }
            _ => type_expr,
        }
    }

    /// Gate the inferred item type of a `for`-loop binding the same way `let`
    /// bindings are gated. For a simple `for name in …` the primitive item type
    /// is kept only when no body reassignment can change `name`'s primitive
    /// kind. A destructuring item pattern (`for [a, b] in …`) is left untouched
    /// unless one of its names is reassigned in the body, in which case the whole
    /// inferred type is dropped — destructured items are rarely reassigned, so
    /// this stays sound without per-element kind tracking.
    pub(super) fn gate_for_item_type(
        &mut self,
        pattern: &BindingPattern,
        item_type: Option<TypeExpr>,
        body: &[SNode],
    ) -> Option<TypeExpr> {
        if !self.options.optimizations_enabled() {
            return item_type;
        }
        match pattern {
            BindingPattern::Identifier(name) => {
                let Some(tag) = item_type.as_ref().and_then(|t| self.primitive_type_tag(t)) else {
                    return item_type;
                };
                if self.for_item_binding_is_monomorphic(name, tag, body) {
                    item_type
                } else {
                    None
                }
            }
            other => {
                let mut names = Vec::new();
                Self::collect_pattern_names(other, &mut names);
                if names
                    .iter()
                    .any(|name| Self::body_reassigns_name(name, body))
                {
                    None
                } else {
                    item_type
                }
            }
        }
    }

    fn collect_pattern_names(pattern: &BindingPattern, out: &mut Vec<String>) {
        match pattern {
            BindingPattern::Identifier(name) => out.push(name.clone()),
            BindingPattern::Dict(fields) => {
                for field in fields {
                    out.push(field.alias.clone().unwrap_or_else(|| field.key.clone()));
                }
            }
            BindingPattern::List(elements) => {
                for element in elements {
                    out.push(element.name.clone());
                }
            }
            BindingPattern::Pair(a, b) => {
                out.push(a.clone());
                out.push(b.clone());
            }
        }
    }

    fn body_reassigns_name(name: &str, body: &[SNode]) -> bool {
        let mut found = false;
        for sn in body {
            harn_parser::visit::walk_node(sn, &mut |node| {
                if let Node::Assignment { target, .. } = &node.node {
                    if let Node::Identifier(target_name) = &target.node {
                        if target_name == name {
                            found = true;
                        }
                    }
                }
            });
        }
        found
    }

    /// Prove which mutable `let` bindings declared at the top level of `stmts`
    /// are *monomorphic* — their value keeps one primitive type across the
    /// initializer and every reassignment reachable in this scope — and record
    /// their spans in [`Compiler::monomorphic_bindings`].
    ///
    /// This is the soundness gate for typed-opcode specialization. A typed op
    /// such as `AddInt` hard-errors when an operand is not the expected
    /// primitive at runtime, so the compiler may only emit it for operands whose
    /// type it can *prove*, not merely guess. A `let`'s initializer type is a
    /// guess: the binding can later be reassigned through an `any`-typed value
    /// (which the type checker accepts as assignable to the inferred type) of a
    /// different runtime primitive, at which point a hard-committed `AddInt`
    /// would spuriously throw on a program the generic path runs correctly.
    ///
    /// Only `let`/`for`-item bindings need this gate. `const` is immutable —
    /// the runtime rejects reassignment — so its initializer type is sound
    /// as-is and is left untouched.
    ///
    /// Strategy: gather the primitive-typed `let` candidates, collect every
    /// reassignment to each (the only way Harn can rebind a name; there is no
    /// destructuring assignment and values are immutable, so an `Identifier`
    /// assignment target is the sole rebind site), then run a monotone fixpoint.
    /// Each round assumes the not-yet-disproven candidates hold their initializer
    /// kind and demotes any whose reassignment then fails to yield that kind.
    /// The mutual assumption lets the common accumulator idiom
    /// (`total = total + (i + 3) * 2`, which depends on the sibling counter `i`)
    /// stay proven, while a counter fed from an `any` value is demoted. The
    /// fixpoint only ever demotes, so it terminates; a candidate that survives
    /// to the end is monomorphic under a self-consistent assignment.
    ///
    /// Candidates the analysis cannot prove are simply left unrecorded: the gate
    /// then drops their primitive fact and they fall back to the correct generic
    /// adaptive path. Soundness therefore never depends on the analysis being
    /// complete — only its reach affects how much code keeps the fast path.
    pub(super) fn record_monomorphic_var_bindings(&mut self, stmts: &[SNode]) {
        if !self.options.optimizations_enabled() {
            return;
        }

        // 1. Gather primitive-typed `let name = init` candidates declared at the
        //    top level of this scope. The first declaration of a name wins; a
        //    later same-name `let` in this block is a redeclaration we leave on
        //    the generic path (it would not be reached as a candidate here).
        //    (`let` is the mutable binding form; `const` is immutable and never
        //    a monomorphic-let candidate.)
        let mut order: Vec<String> = Vec::new();
        let mut tag: std::collections::HashMap<String, &'static str> =
            std::collections::HashMap::new();
        let mut span: std::collections::HashMap<String, harn_lexer::Span> =
            std::collections::HashMap::new();
        for sn in stmts {
            let Node::LetBinding {
                pattern: BindingPattern::Identifier(name),
                value,
                type_ann,
                ..
            } = &sn.node
            else {
                continue;
            };
            if tag.contains_key(name) {
                continue;
            }
            let declared = type_ann.clone().or_else(|| self.infer_expr_type(value));
            let Some(primitive) = declared.as_ref().and_then(|t| self.primitive_type_tag(t)) else {
                continue;
            };
            order.push(name.clone());
            tag.insert(name.clone(), primitive);
            span.insert(name.clone(), sn.span);
        }
        if order.is_empty() {
            return;
        }

        // 2. The set of names rebound anywhere in this subtree (an `Identifier`
        //    assignment target is the only rebind site — Harn has no
        //    destructuring assignment and values are immutable).
        let reassigned_names = Self::collect_reassigned_names(stmts);

        // 3. Seed map: bindings whose primitive type is stable across this
        //    subtree and can therefore be assumed while proving the candidates —
        //    immutable `const`, plus `let`/`for`-item bindings that are
        //    never reassigned here. This lets a candidate that depends on a
        //    sibling binding introduced later or in a nested scope (notably
        //    `for n in xs { sum = sum + n }`) still be proven monomorphic. A
        //    *reassigned* mutable binding is deliberately excluded: it cannot be
        //    assumed safe, so candidates depending on it are demoted to the
        //    generic path.
        let seeds = self.collect_primitive_seeds(stmts, &reassigned_names);

        // 4. Collect every reassignment `name = value` / `name op= value` to a
        //    candidate, anywhere in this scope's statement subtree.
        let candidate_names: std::collections::HashSet<&str> =
            order.iter().map(String::as_str).collect();
        let mut reassigns: std::collections::HashMap<String, Vec<(Option<String>, SNode)>> =
            std::collections::HashMap::new();
        for sn in stmts {
            harn_parser::visit::walk_node(sn, &mut |node| {
                if let Node::Assignment { target, value, op } = &node.node {
                    if let Node::Identifier(name) = &target.node {
                        if candidate_names.contains(name.as_str()) {
                            reassigns
                                .entry(name.clone())
                                .or_default()
                                .push((op.clone(), (**value).clone()));
                        }
                    }
                }
            });
        }

        // 5. Monotone fixpoint: assume every still-trusted candidate holds its
        //    initializer kind (on top of the stable seeds), then demote any whose
        //    reassignment does not yield that same kind under those assumptions.
        //    Repeat until stable.
        let mut demoted: std::collections::HashSet<String> = std::collections::HashSet::new();
        loop {
            let mut assumptions: std::collections::HashMap<String, TypeExpr> = seeds.clone();
            for name in &order {
                if !demoted.contains(name) {
                    assumptions.insert(name.clone(), TypeExpr::Named(tag[name].to_string()));
                }
            }
            self.type_scopes.push(assumptions);

            let mut newly_demoted: Vec<String> = Vec::new();
            for name in &order {
                if demoted.contains(name) {
                    continue;
                }
                let expected = tag[name];
                let preserved = reassigns
                    .get(name)
                    .into_iter()
                    .flatten()
                    .all(|(op, value)| {
                        self.reassignment_primitive_tag(expected, op.as_deref(), value)
                            == Some(expected)
                    });
                if !preserved {
                    newly_demoted.push(name.clone());
                }
            }

            self.type_scopes.pop();
            if newly_demoted.is_empty() {
                break;
            }
            demoted.extend(newly_demoted);
        }

        // 6. Record the survivors as monomorphic.
        for name in &order {
            if !demoted.contains(name) {
                let s = span[name];
                self.monomorphic_bindings.insert((s.start, s.end));
            }
        }
    }

    /// Names rebound by an assignment anywhere in `stmts`' subtree. Used to
    /// decide which mutable bindings are stable enough to seed the monomorphic
    /// fixpoint.
    fn collect_reassigned_names(stmts: &[SNode]) -> std::collections::HashSet<String> {
        let mut names = std::collections::HashSet::new();
        for sn in stmts {
            harn_parser::visit::walk_node(sn, &mut |node| {
                if let Node::Assignment { target, .. } = &node.node {
                    if let Node::Identifier(name) = &target.node {
                        names.insert(name.clone());
                    }
                }
            });
        }
        names
    }

    /// Build the seed assumptions for the monomorphic fixpoint: every binding in
    /// `stmts`' subtree whose value provably keeps a single primitive type here.
    /// Immutable `const` always qualifies; `let` and `for`-item bindings
    /// qualify only when their name is never reassigned in this subtree.
    fn collect_primitive_seeds(
        &self,
        stmts: &[SNode],
        reassigned: &std::collections::HashSet<String>,
    ) -> std::collections::HashMap<String, TypeExpr> {
        let mut seeds: std::collections::HashMap<String, TypeExpr> =
            std::collections::HashMap::new();
        for sn in stmts {
            harn_parser::visit::walk_node(sn, &mut |node| {
                let (name, declared) = match &node.node {
                    Node::ConstBinding {
                        pattern: BindingPattern::Identifier(name),
                        type_ann,
                        value,
                        ..
                    }
                    | Node::LetBinding {
                        pattern: BindingPattern::Identifier(name),
                        type_ann,
                        value,
                        ..
                    } => {
                        // A reassigned `let` is not stable; `const` is immutable so
                        // it always is, even though they share this arm.
                        if matches!(node.node, Node::LetBinding { .. }) && reassigned.contains(name)
                        {
                            return;
                        }
                        (
                            name,
                            type_ann.clone().or_else(|| self.infer_expr_type(value)),
                        )
                    }
                    Node::ForIn {
                        pattern: BindingPattern::Identifier(name),
                        iterable,
                        ..
                    } => {
                        if reassigned.contains(name) {
                            return;
                        }
                        (name, self.infer_for_item_type(iterable))
                    }
                    _ => return,
                };
                if let Some(t) = declared.as_ref().and_then(|t| self.primitive_type_tag(t)) {
                    seeds
                        .entry(name.clone())
                        .or_insert_with(|| TypeExpr::Named(t.to_string()));
                }
            });
        }
        seeds
    }

    /// Primitive tag a single reassignment to `name` (assumed to currently hold
    /// `expected`) would produce, or `None` when it is non-primitive or
    /// statically unknown. A plain `name = value` takes the value's type; a
    /// compound `name op= value` takes `name op value` with `name` held at
    /// `expected`. Candidate assumptions are supplied via a pushed type scope.
    fn reassignment_primitive_tag(
        &self,
        expected: &str,
        op: Option<&str>,
        value: &SNode,
    ) -> Option<&'static str> {
        match op {
            None => self
                .infer_expr_type(value)
                .as_ref()
                .and_then(|t| self.primitive_type_tag(t)),
            Some(op) => {
                let left = TypeExpr::Named(expected.to_string());
                let right = self.infer_expr_type(value);
                self.infer_binary_result_type(op, Some(&left), right.as_ref())
                    .as_ref()
                    .and_then(|t| self.primitive_type_tag(t))
            }
        }
    }

    /// Prove a single `for`-loop item binding is monomorphic over `body`: it is
    /// reassignable per iteration like a `let`, so the same gate applies. The
    /// item starts each iteration at `item_tag`; the binding stays monomorphic
    /// only if every reassignment in the loop body again yields `item_tag`.
    pub(super) fn for_item_binding_is_monomorphic(
        &mut self,
        name: &str,
        item_tag: &str,
        body: &[SNode],
    ) -> bool {
        if !self.options.optimizations_enabled() {
            return false;
        }
        let mut reassigns: Vec<(Option<String>, SNode)> = Vec::new();
        for sn in body {
            harn_parser::visit::walk_node(sn, &mut |node| {
                if let Node::Assignment { target, value, op } = &node.node {
                    if let Node::Identifier(target_name) = &target.node {
                        if target_name == name {
                            reassigns.push((op.clone(), (**value).clone()));
                        }
                    }
                }
            });
        }
        if reassigns.is_empty() {
            return true;
        }
        let mut assumptions = std::collections::HashMap::new();
        assumptions.insert(name.to_string(), TypeExpr::Named(item_tag.to_string()));
        self.type_scopes.push(assumptions);
        let preserved = reassigns.iter().all(|(op, value)| {
            self.reassignment_primitive_tag(item_tag, op.as_deref(), value) == Some(item_tag)
        });
        self.type_scopes.pop();
        preserved
    }

    fn primitive_kind(type_expr: &TypeExpr) -> Option<PrimitiveType> {
        match type_expr {
            TypeExpr::Named(name) => match name.as_str() {
                "int" => Some(PrimitiveType::Int),
                "float" => Some(PrimitiveType::Float),
                "bool" => Some(PrimitiveType::Bool),
                "string" => Some(PrimitiveType::String),
                "nil" => Some(PrimitiveType::Nil),
                _ => None,
            },
            TypeExpr::LitInt(_) => Some(PrimitiveType::Int),
            TypeExpr::LitString(_) => Some(PrimitiveType::String),
            _ => None,
        }
    }
}
