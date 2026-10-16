//! Call-site checks for builtin value methods, driven by the method registry
//! ([`crate::builtin_methods`]): unknown and removed spellings, arity, and
//! mutating methods called on something that is not a mutable place.

use super::*;

use crate::builtin_methods::{self as methods, MethodMigration, MethodReceiver, RemovedMethod};

impl TypeChecker {
    /// Check `object.method(args)` against the registry when `resolved` (the
    /// receiver's alias-resolved type) is a builtin value.
    pub(super) fn check_builtin_method_call(
        &mut self,
        object: &SNode,
        method: &str,
        args: &[SNode],
        scope: &TypeScope,
        span: Span,
        resolved: &TypeExpr,
    ) {
        let receiver = Self::builtin_method_receiver_type(resolved, method)
            .and_then(|ty| Self::builtin_method_receiver(&ty).map(|(kind, _)| kind));
        let Some(kind) = receiver else {
            // Optional chaining skips nil, but a concrete member of a union
            // must still support the call. Numbers and bools have no methods.
            if let Some(name) = Self::methodless_primitive_member(resolved) {
                self.emit_unknown_method(method, &format!("`{name}`"), &[], false, span);
                return;
            }
            match resolved {
                TypeExpr::Named(name) if is_gradual_type_name(name) => {
                    self.check_removed_method_on_unknown_receiver(object, method, args, span);
                }
                TypeExpr::Union(_) => {
                    self.check_removed_method_on_unknown_receiver(object, method, args, span);
                }
                _ => {}
            }
            return;
        };
        if let Some(removed) = methods::removed_method(kind, method) {
            self.report_removed_method(&[removed], object, args, span);
            return;
        }
        let mut bindings = std::collections::BTreeMap::new();
        let Some(row) = Self::builtin_method_row(kind, method, &mut bindings) else {
            // Dicts may carry a callable member, and iterators and generators
            // defer to runtime; the remaining receivers have a closed set.
            if matches!(
                kind,
                MethodReceiver::String | MethodReceiver::List | MethodReceiver::Set
            ) {
                let names = methods::method_names(kind);
                self.emit_unknown_method(
                    method,
                    &format!("`{}`", kind.type_name()),
                    &names,
                    true,
                    span,
                );
            }
            return;
        };
        // A dict may be a namespace or carry a callable member of the same
        // name, so only closed receivers are held to the signature's arity.
        if kind != MethodReceiver::Dict {
            self.check_builtin_method_arity(row, method, args, span);
        }
        if row.is_mutating() {
            self.check_mutating_method_receiver(object, method, kind, scope, span);
        }
    }

    fn methodless_primitive_member(ty: &TypeExpr) -> Option<&str> {
        match ty {
            TypeExpr::Named(name) if matches!(name.as_str(), "int" | "float" | "bool") => {
                Some(name)
            }
            TypeExpr::Union(members) => members.iter().find_map(Self::methodless_primitive_member),
            _ => None,
        }
    }

    /// Run [`Self::check_mutating_call_used_as_value`] on each value `node`
    /// consumes directly: call arguments, collection literal elements, and
    /// operator operands.
    pub(super) fn check_consumed_mutating_calls(&mut self, node: &SNode, scope: &TypeScope) {
        let consumed: Vec<&SNode> = match &node.node {
            Node::FunctionCall { args, .. }
            | Node::MethodCall { args, .. }
            | Node::OptionalMethodCall { args, .. } => args.iter().collect(),
            Node::ListLiteral(items) => items.iter().collect(),
            Node::DictLiteral(entries) => entries.iter().map(|entry| &entry.value).collect(),
            Node::BinaryOp { left, right, .. } => vec![&**left, &**right],
            Node::Ternary {
                true_expr,
                false_expr,
                ..
            } => vec![&**true_expr, &**false_expr],
            Node::Spread(inner) => vec![&**inner],
            _ => return,
        };
        for value in consumed {
            self.check_mutating_call_used_as_value(value, scope);
        }
    }

    /// A mutating method whose result is `nil` used as a value: under the
    /// old copy-returning meaning `const ys = xs.sort()` was a sorted copy,
    /// and now it sorts `xs` and binds `nil`. `sort`/`sort_by`/`reverse`
    /// repair to their copy-returning counterparts.
    pub(super) fn check_mutating_call_used_as_value(&mut self, value: &SNode, scope: &TypeScope) {
        let Node::MethodCall { object, method, .. } = &value.node else {
            return;
        };
        // Several value positions reach the same call (an argument is both
        // consumed by its call and checked against the parameter type).
        if self.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == Code::RemovedBuiltinMethod && diagnostic.span == Some(value.span)
        }) {
            return;
        }
        let receiver = self
            .infer_type(object, scope)
            .map(|ty| self.resolve_alias(&ty, scope));
        let kind = match &receiver {
            None => None,
            Some(TypeExpr::Named(name)) if is_gradual_type_name(name) => None,
            Some(ty) => match Self::builtin_method_receiver_type(ty, method)
                .and_then(|ty| Self::builtin_method_receiver(&ty).map(|(kind, _)| kind))
            {
                Some(kind) => Some(kind),
                // A struct, enum, or host handle owns its own methods.
                None => return,
            },
        };
        let rows: Vec<&methods::MethodDescriptor> = match kind {
            Some(kind) => methods::method(kind, method).into_iter().collect(),
            None => methods::BUILTIN_METHODS
                .iter()
                .filter(|row| row.name() == method)
                .collect(),
        };
        if rows.is_empty() || !rows.iter().all(|row| row.is_mutating()) {
            return;
        }
        // A known builtin temporary is already reported by the receiver
        // check (`[2, 1].sort()`).
        if kind.is_some() && methods::place_root(object).is_none() {
            return;
        }
        // `x = x.push(v)` is the old copy-returning alias spelled as an
        // update; the mutating call alone now does the update.
        if let Some(fix) = self.self_assignment_fix(object, value.span, method, method) {
            self.error_at_with_fix(
                Code::RemovedBuiltinMethod,
                format!(
                    "`{method}` now changes its receiver in place, so assigning its result back replaces the receiver with that result"
                ),
                value.span,
                fix,
            );
            return;
        }
        let returns_nil = rows
            .iter()
            .all(|row| matches!(row.signature.returns, harn_builtin_meta::Ty::Named("nil")));
        if !returns_nil {
            return;
        }
        match methods::copy_counterpart(method) {
            Some(copy) => self.report_copy_counterpart(object, method, copy, value.span),
            // Untyped receivers may be host objects with a value-returning
            // method of the same name; only a known builtin is certain.
            None if kind.is_some() => self.error_at(
                Code::RemovedBuiltinMethod,
                format!(
                    "`{method}` changes its receiver in place and returns nil, so this value is always nil"
                ),
                value.span,
            ),
            None => {}
        }
    }

    fn report_copy_counterpart(&mut self, object: &SNode, method: &str, copy: &str, span: Span) {
        let message = format!(
            "`{method}` now changes its receiver in place and returns nil — use `{copy}()` for a new list"
        );
        match self.method_name_location(object, span, method) {
            Some(location) => self.error_at_with_fix(
                Code::RemovedBuiltinMethod,
                message,
                span,
                vec![FixEdit {
                    span: location.name,
                    replacement: copy.to_string(),
                }],
            ),
            None => self.error_at(Code::RemovedBuiltinMethod, message, span),
        }
    }

    /// `xs?.push(v)` has no place to write the result back to: the optional
    /// chain yields a temporary.
    pub(super) fn check_optional_chained_mutation(
        &mut self,
        object: &SNode,
        method: &str,
        scope: &TypeScope,
        span: Span,
    ) {
        let Some(receiver) = self
            .infer_type(object, scope)
            .map(|ty| self.resolve_alias(&ty, scope))
            .and_then(|ty| Self::builtin_method_receiver_type(&ty, method))
            .and_then(|ty| Self::builtin_method_receiver(&ty).map(|(kind, _)| kind))
        else {
            return;
        };
        if methods::method(receiver, method).is_some_and(|row| row.is_mutating()) {
            self.error_at_with_help(
                Code::ImmutableAssignment,
                format!(
                    "`{}.{method}` mutates its receiver and cannot be called through `?.`",
                    receiver.type_name()
                ),
                span,
                "check the receiver against nil first, then call the method on the binding"
                    .to_string(),
            );
        }
    }

    /// A receiver whose type is unknown can still be named with a spelling no
    /// builtin accepts any more (`x.to_lower()`), which is stale whatever `x`
    /// turns out to be.
    pub(super) fn check_removed_method_on_unknown_receiver(
        &mut self,
        object: &SNode,
        method: &str,
        args: &[SNode],
        span: Span,
    ) {
        let removed = methods::removed_method_on_any_receiver(method);
        if !removed.is_empty() {
            self.report_removed_method(&removed, object, args, span);
        }
    }

    /// Report a removed spelling. `rows` holds one row when the receiver's
    /// type is known, and every receiver's row when it is not.
    fn report_removed_method(
        &mut self,
        rows: &[&RemovedMethod],
        object: &SNode,
        args: &[SNode],
        span: Span,
    ) {
        let Some(first) = rows.first() else {
            return;
        };
        // Rows that differ only in guidance text still share one repair.
        let same_repair = |a: &MethodMigration, b: &MethodMigration| match (a, b) {
            (
                MethodMigration::MutatingWhenSelfAssigned { mutating: x, .. },
                MethodMigration::MutatingWhenSelfAssigned { mutating: y, .. },
            ) => x == y,
            _ => a == b,
        };
        let agreed = rows.iter().all(|row| row.migration == first.migration);
        let repair_agreed = rows
            .iter()
            .all(|row| same_repair(&row.migration, &first.migration));
        let message = if agreed {
            let receivers: Vec<&str> = rows.iter().map(|row| row.receiver.type_name()).collect();
            format!(
                "method `{}` was removed from `{}` — {}",
                first.name,
                receivers.join("` and `"),
                first.repair_hint()
            )
        } else {
            let hints: Vec<String> = rows
                .iter()
                .map(|row| format!("on a {}, {}", row.receiver.type_name(), row.repair_hint()))
                .collect();
            format!("method `{}` was removed — {}", first.name, hints.join("; "))
        };
        let fix = match (repair_agreed, first.migration) {
            (true, MethodMigration::Rename(to)) => self
                .method_name_location(object, span, first.name)
                .map(|location| {
                    vec![FixEdit {
                        span: location.name,
                        replacement: to.to_string(),
                    }]
                }),
            (true, MethodMigration::AppendingToPush) => self.appending_fix(object, args, span),
            (true, MethodMigration::MutatingWhenSelfAssigned { mutating, .. }) => {
                self.self_assignment_fix(object, span, first.name, mutating)
            }
            _ => None,
        };
        match fix {
            Some(fix) => self.error_at_with_fix(Code::RemovedBuiltinMethod, message, span, fix),
            None => self.error_at(Code::RemovedBuiltinMethod, message, span),
        }
    }

    fn check_builtin_method_arity(
        &mut self,
        row: &methods::MethodDescriptor,
        method: &str,
        args: &[SNode],
        span: Span,
    ) {
        if args.iter().any(|arg| matches!(arg.node, Node::Spread(_))) {
            return;
        }
        let params = row.signature.params;
        let required = params.iter().filter(|param| !param.optional).count();
        let maximum = (!row.signature.has_rest).then_some(params.len());
        if args.len() >= required && maximum.is_none_or(|maximum| args.len() <= maximum) {
            return;
        }
        let expected = match maximum {
            Some(maximum) if maximum == required => required.to_string(),
            Some(maximum) => format!("{required}-{maximum}"),
            None => format!("at least {required}"),
        };
        let signature = row.signature.to_string();
        self.error_at_with_help(
            Code::BuiltinArity,
            format!(
                "`{}.{method}` expects {expected} argument{}, got {}",
                row.receiver.type_name(),
                if expected == "1" { "" } else { "s" },
                args.len()
            ),
            span,
            format!("signature: {signature}"),
        );
    }

    /// A mutating method rewrites its receiver, so the receiver must be a
    /// place reachable from a `let` binding.
    fn check_mutating_method_receiver(
        &mut self,
        object: &SNode,
        method: &str,
        kind: MethodReceiver,
        scope: &TypeScope,
        span: Span,
    ) {
        let receiver = kind.type_name();
        let Some(root) = methods::place_root(object) else {
            if let Some(copy) = methods::copy_counterpart(method) {
                // `xs.keys().sort()` only ever meant a sorted copy.
                self.report_copy_counterpart(object, method, copy, span);
                return;
            }
            self.error_at_with_help(
                Code::ImmutableAssignment,
                format!(
                    "`{receiver}.{method}` mutates its receiver, but the receiver is a temporary value"
                ),
                span,
                format!(
                    "bind the {receiver} with `let` first, then call `{method}` on the binding"
                ),
            );
            return;
        };
        if scope.get_var(root).is_some() && !scope.is_mutable(root) {
            self.error_at_with_help(
                Code::ImmutableAssignment,
                format!(
                    "`{receiver}.{method}` mutates '{root}', which is immutable (declared with 'const' or a parameter)"
                ),
                span,
                match methods::copy_counterpart(method) {
                    Some(copy) => format!(
                        "declare '{root}' with 'let', or call `{copy}()` for a new {receiver}"
                    ),
                    None => format!("declare '{root}' with 'let' so `{method}` can change it"),
                },
            );
        }
    }

    /// `x = x.old(args)` as a whole statement becomes `x.mutating(args)`.
    fn self_assignment_fix(
        &self,
        object: &SNode,
        span: Span,
        method: &str,
        mutating: &str,
    ) -> Option<Vec<FixEdit>> {
        let source = self.source.as_deref()?;
        let location = self.method_name_location(object, span, method)?;
        let receiver = source.get(span.start..location.dot)?.trim_end();
        let statement = self_assignment_start(source, receiver, span)?;
        let after_name = source.get(location.name.end..span.end)?;
        Some(vec![FixEdit {
            span: Span::with_offsets(statement, span.end, span.line, span.column),
            replacement: format!("{receiver}.{mutating}{after_name}"),
        }])
    }

    /// `xs = xs.appending(v)` is the in-place append `xs.push(v)`; any other
    /// `xs.appending(v)` is the new list `xs + [v]`.
    fn appending_fix(&self, object: &SNode, args: &[SNode], span: Span) -> Option<Vec<FixEdit>> {
        let [item] = args else {
            return None;
        };
        if matches!(item.node, Node::Spread(_)) {
            return None;
        }
        let source = self.source.as_deref()?;
        let location = self.method_name_location(object, span, "appending")?;
        let receiver = source.get(span.start..location.dot)?.trim_end();
        if !parens_balanced(receiver) {
            return None;
        }
        let item_text = source.get(item.span.start..item.span.end)?;
        if let Some(statement) = self_assignment_start(source, receiver, span) {
            return Some(vec![FixEdit {
                span: Span::with_offsets(statement, span.end, span.line, span.column),
                replacement: format!("{receiver}.push({item_text})"),
            }]);
        }
        let concat = format!("{receiver} + [{item_text}]");
        let replacement = if binds_tighter_than_plus(source, span) {
            format!("({concat})")
        } else {
            concat
        };
        Some(vec![FixEdit { span, replacement }])
    }

    /// Where the method name of `object.method(...)` sits in the source.
    fn method_name_location(
        &self,
        object: &SNode,
        call: Span,
        method: &str,
    ) -> Option<MethodNameLocation> {
        let source = self.source.as_deref()?;
        let bytes = source.as_bytes();
        let mut at = object.span.end;
        while at < call.end && matches!(bytes[at], b' ' | b'\t' | b'\n' | b'\r' | b')') {
            at += 1;
        }
        let dot = at;
        if bytes.get(at) == Some(&b'?') {
            at += 1;
        }
        if bytes.get(at) != Some(&b'.') {
            return None;
        }
        at += 1;
        while at < call.end && matches!(bytes[at], b' ' | b'\t' | b'\n' | b'\r') {
            at += 1;
        }
        let end = at + method.len();
        if source.get(at..end) != Some(method) {
            return None;
        }
        if bytes
            .get(end)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            return None;
        }
        Some(MethodNameLocation {
            dot,
            name: Span::with_offsets(at, end, call.line, call.column),
        })
    }
}

struct MethodNameLocation {
    /// Byte offset of the `.` (or `?.`) before the name.
    dot: usize,
    name: Span,
}

fn parens_balanced(text: &str) -> bool {
    let mut depth = 0i32;
    for byte in text.bytes() {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return false;
        }
    }
    depth == 0
}

/// When the call is the whole right-hand side of `receiver = receiver.m(...)`
/// as its own statement, the byte offset where that statement starts.
fn self_assignment_start(source: &str, receiver: &str, call: Span) -> Option<usize> {
    let before = source.get(..call.start)?.trim_end_matches([' ', '\t']);
    let before = before.strip_suffix('=')?;
    if before.ends_with([
        '=', '!', '<', '>', '+', '-', '*', '/', '%', '?', '&', '|', '^',
    ]) {
        return None;
    }
    let before = before.trim_end_matches([' ', '\t']);
    let target = before.strip_suffix(receiver)?;
    let line_start = target.trim_end_matches([' ', '\t']);
    if !(line_start.is_empty() || line_start.ends_with(['\n', '{', ';'])) {
        return None;
    }
    let after = source.get(call.end..)?.trim_start_matches([' ', '\t']);
    let ends_statement =
        after.is_empty() || after.starts_with(['\n', '\r', '}', ';']) || after.starts_with("//");
    ends_statement.then_some(target.len())
}

/// True when replacing the call with a `+` expression would let a
/// neighbouring operator bind to part of it.
fn binds_tighter_than_plus(source: &str, call: Span) -> bool {
    let after = source.get(call.end..).unwrap_or_default().trim_start();
    let before = source.get(..call.start).unwrap_or_default().trim_end();
    after.starts_with(['.', '[', '*', '/', '%'])
        || after.starts_with("?.")
        || after.starts_with("?[")
        || before.ends_with(['-', '!', '*', '/', '%'])
}
