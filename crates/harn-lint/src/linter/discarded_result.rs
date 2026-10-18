//! Checks for results that are computed and then dropped on the floor.
//!
//! Harn has no implicit block return, so a bare expression statement is a
//! discarded value everywhere except the tail of a value-producing block
//! (closure body, `match` arm, `if`/`else` branch, `try`, `block { … }`) —
//! see [`BlockKind`].

use harn_parser::{DiagnosticCode as Code, Node, SNode};

use super::Linter;
use crate::diagnostic::{LintDiagnostic, LintSeverity};

/// Whether a block's trailing expression is its value or just its last
/// statement. Harn `fn` / `for` / `while` bodies do **not** implicitly return
/// their tail, so a trailing expression there is discarded like any other
/// statement; a closure body or `match` arm *does* yield its tail.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BlockKind {
    /// Tail expression is discarded (`fn`, `for`, `while`, `finally`, …).
    Statement,
    /// Tail expression is the block's value (closure, `match` arm, `if`,
    /// `try`, `block { … }`).
    Value,
}

/// Pure builtin methods that read like effectful host or connector methods
/// (`store.get(k)`, `queue.find(...)`, `cache.has(k)`, `client.merge(...)`).
/// Discarding their result is just as pointless on a builtin value, but a
/// same-named method on some other receiver may exist for its effects, so
/// HARN-LNT-066 leaves them alone. Every name here must be a pure registry
/// method; the rest of the registry's pure methods on strings, lists, sets,
/// and dicts are flagged.
///
/// Keep sorted.
const HOST_METHOD_LOOKALIKES: &[&str] = &[
    "all",
    "any",
    "contains",
    "count_by",
    "difference",
    "drop_while",
    "empty",
    "ends_with",
    "entries",
    "filter",
    "find",
    "find_index",
    "first",
    "flat_map",
    "get",
    "group_by",
    "has",
    "index_of",
    "intersection",
    "is_disjoint",
    "is_subset",
    "is_superset",
    "iter",
    "join",
    "keys",
    "last",
    "last_index_of",
    "len",
    "map",
    "max",
    "max_by",
    "merge",
    "min",
    "min_by",
    "none",
    "reduce",
    "replace",
    "starts_with",
    "sum",
    "symmetric_difference",
    "take",
    "take_while",
    "union",
    "values",
];

/// A method that computes a new value from a builtin string, list, set, or
/// dict and changes nothing, per the method registry.
fn is_pure_collection_method(method: &str) -> bool {
    use harn_parser::builtin_methods::{self as methods, MethodReceiver};
    if HOST_METHOD_LOOKALIKES.binary_search(&method).is_ok()
        || methods::is_mutating_method_name(method)
    {
        return false;
    }
    [
        MethodReceiver::String,
        MethodReceiver::List,
        MethodReceiver::Set,
        MethodReceiver::Dict,
    ]
    .into_iter()
    .any(|receiver| methods::method(receiver, method).is_some())
}

impl Linter<'_> {
    /// Record every method name declared in an `impl` block so
    /// [`Self::check_discarded_pure_result`] can stand down on a receiver that
    /// might be a user type with a same-named, effectful method. Runs as a
    /// pre-pass because a call can precede the `impl` that defines it.
    pub(super) fn collect_impl_method_names(&mut self, nodes: &[SNode]) {
        for node in nodes {
            let inner = match &node.node {
                Node::AttributedDecl { inner, .. } => &inner.node,
                other => other,
            };
            let Node::ImplBlock { methods, .. } = inner else {
                continue;
            };
            for method in methods {
                if let Node::FnDecl { name, .. } = &method.node {
                    self.impl_method_names.insert(name.clone());
                }
            }
        }
    }

    /// Flag a pure collection/string method call whose result is discarded
    /// (`l.sorted()` as a statement).
    ///
    /// `sorted` returns a *new* list rather than sorting `l` (that is
    /// `l.sort()`), so dropping the return value drops the entire point of the
    /// call and leaves `l` unchanged — a silent no-op that otherwise
    /// typechecks clean. This is an error rather than a warning because there
    /// is no legitimate reason to call one of these and discard the result.
    ///
    /// Three guards keep it exact:
    /// - a closure argument (`.map({ … })`) can perform effects, so the
    ///   statement is not provably inert;
    /// - a receiver rooted at `harness` is a host method, which exists for its
    ///   effects;
    /// - a name the file also declares in an `impl` block may be a
    ///   user-defined method with effects of its own.
    pub(super) fn check_discarded_pure_result(&mut self, node: &SNode) {
        let (object, method, args) = match &node.node {
            Node::MethodCall {
                object,
                method,
                args,
            }
            | Node::OptionalMethodCall {
                object,
                method,
                args,
            } => (object, method, args),
            _ => return,
        };
        if !is_pure_collection_method(method)
            || args.iter().any(|a| matches!(&a.node, Node::Closure { .. }))
            || self.impl_method_names.contains(method.as_str())
        {
            return;
        }
        let receiver = Self::method_receiver_root(object);
        if receiver == Some("harness") {
            return;
        }
        // Name the receiver only when it is a plain binding that could be
        // assigned back to. `[1, 2].tally()` has no name to suggest.
        let (message, suggestion) = match receiver {
            Some(name) => (
                format!(
                    "the result of `{method}` is discarded, so this statement leaves `{name}` unchanged"
                ),
                format!(
                    "`{method}` returns a new value instead of modifying `{name}` — assign it back, \
                     as in `{name} = {name}.{method}(...)`, which needs `{name}` declared `let`{}. \
                     To call `{method}` purely for its errors, discard the result explicitly with \
                     `const _ = {name}.{method}(...)`",
                    harn_parser::builtin_methods::in_place_counterpart(method)
                        .map(|m| format!(", or call `{name}.{m}()` to change it in place"))
                        .unwrap_or_default()
                ),
            ),
            None => (
                format!("the result of `{method}` is discarded, so this statement has no effect"),
                format!(
                    "`{method}` returns a new value rather than modifying its receiver — bind the \
                     result, or discard it explicitly with `const _ = ...{method}(...)` if the call \
                     is made only for its errors"
                ),
            ),
        };
        self.diagnostics.push(LintDiagnostic {
            code: Code::LintDiscardedPureResult,
            rule: "discarded-pure-result".into(),
            message,
            span: node.span,
            severity: LintSeverity::Error,
            suggestion: Some(suggestion),
            fix: None,
        });
    }

    /// `xs.push(v)` rewrites `xs` like `xs[i] = v` does, so it counts as a
    /// reassignment for `mutable-never-reassigned`.
    pub(super) fn record_mutating_call_assignment(&mut self, node: &SNode) {
        if let Some(root) = harn_parser::builtin_methods::mutating_call_place(node)
            .and_then(harn_parser::builtin_methods::place_root)
        {
            self.assignments.insert(root.to_string());
        }
    }

    /// The root identifier of a method receiver: `a.b.c` and `a[0]` both root
    /// at `a`. `None` when the receiver is not rooted in a plain name.
    fn method_receiver_root(object: &SNode) -> Option<&str> {
        match &object.node {
            Node::Identifier(name) => Some(name.as_str()),
            Node::PropertyAccess { object, .. }
            | Node::OptionalPropertyAccess { object, .. }
            | Node::SubscriptAccess { object, .. }
            | Node::MethodCall { object, .. }
            | Node::OptionalMethodCall { object, .. } => Self::method_receiver_root(object),
            _ => None,
        }
    }

    pub(super) fn check_discarded_approval_result(&mut self, node: &SNode) {
        // The canonical shape is the capability method
        // `harness.interaction.request_approval(...)`. The bare
        // `request_approval(...)` call is the removed ambient builtin: it is
        // still matched so that a script mid-migration keeps this warning
        // alongside the HARN-LNT-071 that tells it to move, rather than going
        // quiet on the way from one spelling to the other.
        let name = match &node.node {
            Node::FunctionCall { name, .. } if Self::is_approval_record_builtin(name) => {
                name.as_str()
            }
            Node::MethodCall { object, method, .. }
            | Node::OptionalMethodCall { object, method, .. }
                if Self::is_approval_record_builtin(method)
                    && self.harness_capability_of(object) == Some("interaction") =>
            {
                method.as_str()
            }
            _ => return,
        };
        self.diagnostics.push(LintDiagnostic {
            code: Code::LintUnhandledApprovalResult,
            rule: "unhandled-approval-result".into(),
            message: format!("approval result from `{name}` is discarded"),
            span: node.span,
            severity: LintSeverity::Warning,
            suggestion: Some(
                "bind the result, inspect its signed approver receipts, or explicitly assign it to `_`"
                    .to_string(),
            ),
            fix: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_lexer::Lexer;
    use harn_parser::Parser;

    /// The methods HARN-LNT-066 flags in `source`, in order.
    fn flagged(source: &str) -> Vec<String> {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let program = Parser::new(tokens).parse().expect("parse");
        crate::lint(&program)
            .into_iter()
            .filter(|d| d.code == Code::LintDiscardedPureResult)
            .map(|d| {
                assert_eq!(d.severity, LintSeverity::Error, "LNT-066 is error-grade");
                // Messages lead with "the result of `<method>`".
                d.message
                    .split('`')
                    .nth(1)
                    .expect("method name in backticks")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn flags_discarded_sorted_in_statement_position() {
        assert_eq!(
            flagged("fn main(h: Harness) {\n  const l = [2, 1]\n  l.sorted()\n  log(l)\n}"),
            vec!["sorted"]
        );
    }

    #[test]
    fn flags_discarded_result_as_the_tail_of_a_fn_body() {
        // Harn has no implicit block return, so a `fn` tail is discarded too.
        assert_eq!(
            flagged("fn build(s: string) {\n  s.lower()\n}"),
            vec!["lower"]
        );
    }

    #[test]
    fn flags_discarded_result_in_a_loop_body() {
        // The classic builder bug: the tail of a `for` body is not a value.
        assert_eq!(
            flagged(
                "fn main(h: Harness) {\n  let out = [1]\n  for i in [1] {\n    out.reversed()\n  }\n}"
            ),
            vec!["reversed"]
        );
    }

    #[test]
    fn allows_mutating_methods_in_statement_position() {
        // `push` and friends change their receiver; the statement is the point.
        assert!(flagged(
            "fn main(h: Harness) {\n  let l = []\n  l.push(1)\n  l.sort()\n  l.reverse()\n  log(l)\n}"
        )
        .is_empty());
    }

    #[test]
    fn allows_the_result_being_assigned_back() {
        assert!(flagged("fn main(h: Harness) {\n  let l = [2, 1]\n  l = l.sorted()\n}").is_empty());
    }

    #[test]
    fn allows_an_explicit_discard() {
        assert!(flagged("fn main(h: Harness) {\n  const _ = [1].sorted()\n}").is_empty());
    }

    #[test]
    fn allows_a_call_taking_a_closure() {
        // The closure may carry effects, so the statement is not provably inert.
        assert!(flagged(
            "fn main(h: Harness) {\n  const l = [1]\n  l.sorted_by({ a, b -> a - b })\n  log(l)\n}"
        )
        .is_empty());
    }

    #[test]
    fn allows_a_same_named_user_impl_method() {
        // A user method may exist for its effects, unlike the built-ins.
        assert!(flagged(
            "struct Basket { items: list<int> }\nimpl Basket {\n  fn sorted(self) -> Basket { return self }\n}\nfn main(h: Harness) {\n  const b = Basket { items: [] }\n  b.sorted()\n}"
        )
        .is_empty());
    }

    #[test]
    fn allows_a_harness_rooted_receiver() {
        // Host methods exist for their effects.
        assert!(flagged("fn main(h: Harness) {\n  harness.stdio.split(\"a\")\n}").is_empty());
    }

    #[test]
    fn allows_the_tail_of_every_value_producing_block() {
        // Closure body, `match` arm, `if` branch, `try`, and `block { … }` all
        // yield their tail, so the tail is a result rather than a discard.
        for source in [
            "fn main(h: Harness) {\n  const f = { -> [1].sorted() }\n  log(f())\n}",
            "fn main(h: Harness) {\n  const m = match 1 {\n    1 -> { [1].sorted() }\n    _ -> { [] }\n  }\n  log(m)\n}",
            "fn main(h: Harness) {\n  const i = if true { [1].sorted() } else { [] }\n  log(i)\n}",
            "fn main(h: Harness) {\n  const t = try { [1].sorted() } catch (e) { [] }\n  log(t)\n}",
        ] {
            assert!(
                flagged(source).is_empty(),
                "value-block tail must not be flagged: {source}"
            );
        }
    }

    #[test]
    fn flags_a_non_tail_statement_inside_a_value_block() {
        // Only the *tail* of a value block is a result; earlier statements in
        // it are discarded like any other.
        assert_eq!(
            flagged("fn main(h: Harness) {\n  const f = { -> \n    [1].sorted()\n    42\n  }\n  log(f())\n}"),
            vec!["sorted"]
        );
    }

    #[test]
    fn host_method_lookalikes_are_sorted_pure_registry_methods() {
        let mut sorted = HOST_METHOD_LOOKALIKES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.as_slice(),
            HOST_METHOD_LOOKALIKES,
            "HOST_METHOD_LOOKALIKES must stay sorted and duplicate-free for binary_search"
        );
        for name in HOST_METHOD_LOOKALIKES {
            assert!(
                harn_parser::builtin_methods::is_pure_on_every_receiver(name),
                "`{name}` is not a pure registry method; drop it from HOST_METHOD_LOOKALIKES"
            );
        }
    }

    #[test]
    fn ambiguous_host_verbs_are_not_flagged() {
        // Pure on the built-in collections but colliding with plausible
        // effectful methods on other receivers, or mutating.
        for name in ["get", "add", "remove", "push", "merge", "len", "find"] {
            assert!(
                !is_pure_collection_method(name),
                "`{name}` is too ambiguous to flag as a discarded pure result"
            );
        }
        for name in ["sorted", "lower", "windows", "map_keys", "split"] {
            assert!(
                is_pure_collection_method(name),
                "`{name}` should be flagged"
            );
        }
    }

    /// The spans HARN-LNT-013 flags in `source`, as the names it names.
    fn approvals_flagged(source: &str) -> Vec<String> {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let program = Parser::new(tokens).parse().expect("parse");
        crate::lint(&program)
            .into_iter()
            .filter(|d| d.code == Code::LintUnhandledApprovalResult)
            .map(|d| {
                d.message
                    .split('`')
                    .nth(1)
                    .expect("approval name in backticks")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn flags_a_discarded_approval_on_the_canonical_harness_path() {
        // The regression this check exists to prevent: HARN-LNT-071 tells
        // callers to move from `request_approval(...)` to the capability
        // method, so the guard has to survive the move. A silent approval
        // lint is worse than none — it reads as "this code was checked".
        assert_eq!(
            approvals_flagged(
                "fn main(harness: Harness) {\n  harness.interaction.request_approval(\"ship\", {})\n}"
            ),
            vec!["request_approval"]
        );
    }

    #[test]
    fn still_flags_the_legacy_ambient_call() {
        // Mid-migration scripts keep both diagnostics rather than trading
        // one for the other.
        assert_eq!(
            approvals_flagged("fn main(harness: Harness) {\n  request_approval(\"ship\", {})\n}"),
            vec!["request_approval"]
        );
    }

    #[test]
    fn allows_a_bound_approval_result() {
        assert!(
            approvals_flagged(
                "fn main(harness: Harness) {\n  const r = harness.interaction.request_approval(\"ship\", {})\n  log(r)\n}"
            )
            .is_empty()
        );
    }

    #[test]
    fn ignores_request_approval_on_a_receiver_that_is_not_the_harness() {
        // A user-defined object may legitimately own a method by this name;
        // only the host capability carries the approver receipts.
        assert!(
            approvals_flagged(
                "fn main(harness: Harness) {\n  const q = my_queue()\n  q.interaction.request_approval(\"ship\", {})\n}"
            )
            .is_empty()
        );
    }
}
