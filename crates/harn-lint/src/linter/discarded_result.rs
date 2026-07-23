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

/// Methods on `list` / `dict` / `set` / `string` that compute a new value and
/// leave the receiver untouched. Every method on Harn's built-in collections
/// is pure — they are persistent, copy-on-write values — so this list is a
/// *precision* filter rather than a semantic one: it holds the names whose
/// results are worth flagging when dropped, excluding names common enough to
/// collide with an effectful host or connector method of the same name
/// (`get`, `add`, `remove`, `merge`, `count`, `find`, …).
///
/// Keep sorted.
const PURE_COLLECTION_METHODS: &[&str] = &[
    "adding",
    "appending",
    "char_at",
    "chars",
    "chunk",
    "compact",
    "dropping_last",
    "each_cons",
    "each_slice",
    "enumerate",
    "flatten",
    "lines",
    "lower",
    "lowercase",
    "map_keys",
    "map_values",
    "merging",
    "pad_left",
    "pad_right",
    "partition",
    "rekeyed",
    "removing",
    "repeat",
    "reversed",
    "skip",
    "slice",
    "sliding_window",
    "sorted",
    "sorted_by",
    "split",
    "substring",
    "tally",
    "to_dict",
    "to_list",
    "to_set",
    "trim",
    "trim_end",
    "trim_start",
    "unique",
    "upper",
    "uppercase",
    "window",
    "zip",
];

fn is_pure_collection_method(method: &str) -> bool {
    PURE_COLLECTION_METHODS.binary_search(&method).is_ok()
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
    /// (`l.appending(1)` as a statement).
    ///
    /// `appending` clones the receiver, appends, and returns a *new* list rather
    /// than mutating `l`, so dropping the return value drops the entire point
    /// of the call and leaves `l` unchanged — a silent no-op that otherwise
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
                     as in `{name} = {name}.{method}(...)`, which needs `{name}` declared `let`. \
                     To call `{method}` purely for its errors, discard the result explicitly with \
                     `const _ = {name}.{method}(...)`"
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
        // The approval primitive is recognized in either form: the
        // legacy `request_approval(...)` function-call shape (still
        // valid for back-compat) and the first-class `HitlExpr` form
        // produced by the reserved-keyword parser.
        let name = match &node.node {
            Node::FunctionCall { name, .. } if Self::is_approval_record_builtin(name) => {
                name.as_str()
            }
            Node::HitlExpr {
                kind: harn_parser::HitlKind::RequestApproval,
                ..
            } => "request_approval",
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
    fn flags_discarded_appending_in_statement_position() {
        assert_eq!(
            flagged("fn main(h: Harness) {\n  const l = []\n  l.appending(1)\n  log(l)\n}"),
            vec!["appending"]
        );
    }

    #[test]
    fn flags_discarded_appending_as_the_tail_of_a_fn_body() {
        // Harn has no implicit block return, so a `fn` tail is discarded too.
        assert_eq!(
            flagged("fn build(l: list<int>) {\n  l.appending(1)\n}"),
            vec!["appending"]
        );
    }

    #[test]
    fn flags_discarded_appending_in_a_loop_body() {
        // The classic builder bug: the tail of a `for` body is not a value.
        assert_eq!(
            flagged(
                "fn main(h: Harness) {\n  let out = []\n  for i in [1] {\n    out.appending(i)\n  }\n}"
            ),
            vec!["appending"]
        );
    }

    #[test]
    fn allows_the_result_being_assigned_back() {
        assert!(flagged("fn main(h: Harness) {\n  let l = []\n  l = l.appending(1)\n}").is_empty());
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
            "struct Basket { items: list<int> }\nimpl Basket {\n  fn appending(self, i: int) -> Basket { return self }\n}\nfn main(h: Harness) {\n  const b = Basket { items: [] }\n  b.appending(1)\n}"
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
    fn pure_collection_methods_are_sorted_and_unique() {
        let mut sorted = PURE_COLLECTION_METHODS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.as_slice(),
            PURE_COLLECTION_METHODS,
            "PURE_COLLECTION_METHODS must stay sorted and duplicate-free for binary_search"
        );
    }

    #[test]
    fn ambiguous_host_verbs_are_not_listed() {
        // These are pure on the built-in collections but collide with
        // plausible effectful methods on other receivers, so they stay out.
        for name in ["get", "add", "remove", "delete", "merge", "count", "find"] {
            assert!(
                !is_pure_collection_method(name),
                "`{name}` is too ambiguous to flag as a discarded pure result"
            );
        }
    }
}
