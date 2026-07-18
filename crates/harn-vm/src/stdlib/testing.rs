use crate::value::VmDictExt;
use std::collections::BTreeMap;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{
    error_to_category, render_diff, repr, values_equal, ErrorCategory, VmError, VmValue,
};
use crate::vm::Vm;

pub(crate) fn register_testing_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &TESTING_CALL_BODY_IMPL_DEF,
    &ASSERT_IMPL_DEF,
    &ASSERT_EQ_IMPL_DEF,
    &ASSERT_NE_IMPL_DEF,
    &ASSERT_APPROX_IMPL_DEF,
    &ASSERT_MATCHES_IMPL_DEF,
    &VALUE_DIFF_IMPL_DEF,
    &ERROR_CATEGORY_IMPL_DEF,
    &THROW_ERROR_IMPL_DEF,
    &IS_TIMEOUT_IMPL_DEF,
    &IS_RATE_LIMITED_IMPL_DEF,
    &ERROR_IS_IMPL_DEF,
    &ERROR_IS_TRANSIENT_IMPL_DEF,
];

#[harn_builtin(
    sig = "__testing_call_body(body: any) -> any",
    kind = "async",
    category = "testing"
)]
async fn testing_call_body_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let body = args
        .first()
        .cloned()
        .ok_or_else(|| VmError::Runtime("__testing_call_body: body is required".to_string()))?;
    if !Vm::is_callable_value(&body) {
        return Err(VmError::TypeError(format!(
            "__testing_call_body: body must be callable, got {}",
            body.type_name()
        )));
    }

    let call_args = match &body {
        VmValue::Closure(closure) => {
            let required = closure.func.required_param_count();
            if required == 0 {
                Vec::new()
            } else if required == 1 {
                vec![VmValue::Nil]
            } else {
                return Err(VmError::Runtime(format!(
                    "__testing_call_body: body expects {required} required argument(s); scoped mock helpers pass at most one context value"
                )));
            }
        }
        _ => Vec::new(),
    };

    let mut vm = ctx.child_vm();
    let result = vm.call_callable_owned(&body, call_args).await;
    ctx.forward_output(&vm.take_output());
    result
}

/// A message argument that stringifies to nil's own JSON encoding, to nil's
/// own `display()`, or to nothing at all carries the same "no message"
/// signal as omitting the argument outright. The dominant way test authors
/// hit this is `assert(cond, json_stringify(maybe_nil_value))`: when the
/// dumped value turns out to be nil, `json_stringify` is *correct* to return
/// `"null"` (that's the JSON encoding of nil), but forwarding it verbatim
/// makes the thrown message read as if "null" were meaningful diagnostic
/// content rather than the absence of any. Treat that value the same as an
/// omitted message so the fallback default (which names the failed
/// assertion) takes over instead.
fn is_uninformative_message(value: &VmValue) -> bool {
    matches!(value, VmValue::Nil)
        || matches!(value, VmValue::String(s) if s.trim().is_empty() || s.as_str() == "null")
}

/// Resolves the message argument at `index`, falling back to `default` when
/// the argument is absent or [`is_uninformative_message`].
fn assert_message(args: &[VmValue], index: usize, default: impl FnOnce() -> String) -> String {
    match args.get(index) {
        Some(value) if !is_uninformative_message(value) => value.display(),
        _ => default(),
    }
}

#[harn_builtin(
    sig = "assert(condition: any, message?: string) -> nil",
    category = "testing"
)]
fn assert_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let condition = args.first().unwrap_or(&VmValue::Nil);
    if !condition.is_truthy() {
        let msg = assert_message(args, 1, || "Assertion failed".to_string());
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(msg))));
    }
    Ok(VmValue::Nil)
}

/// Throws a Harn-level error carrying `text` — the shape every assertion
/// failure takes, so `try`/`catch` sees a plain string it can match on.
fn fail(text: String) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(text)))
}

/// The two values under comparison, in the surface's fixed `(actual,
/// expected)` order, plus the caller's optional override message.
///
/// Every equality-shaped builtin funnels through this so the argument order is
/// stated once. Getting it wrong silently inverts every diff, so it is not a
/// thing to re-derive at four call sites.
struct Comparison<'a> {
    actual: &'a VmValue,
    expected: &'a VmValue,
    message: Option<String>,
}

impl<'a> Comparison<'a> {
    fn parse(name: &str, args: &'a [VmValue]) -> Result<Self, VmError> {
        let (Some(actual), Some(expected)) = (args.first(), args.get(1)) else {
            return Err(fail(format!(
                "{name} needs two values to compare: {name}(actual, expected)"
            )));
        };
        Ok(Self {
            actual,
            expected,
            // An uninformative override (nil, blank, or the bare string
            // `"null"` that `json_stringify(nil)` produces) carries no signal,
            // so it falls back to the diff the same way an omitted message
            // does — see `is_uninformative_message`.
            message: args
                .get(2)
                .filter(|m| !is_uninformative_message(m))
                .map(|m| m.display()),
        })
    }

    /// The caller's message when they gave one, else `default`. An explicit
    /// message is a deliberate choice to say something the diff cannot, so it
    /// replaces the diff rather than decorating it.
    fn or_default(&self, default: impl FnOnce() -> String) -> VmError {
        fail(self.message.clone().unwrap_or_else(default))
    }
}

#[harn_builtin(
    sig = "assert_eq(actual: any, expected: any, message?: string) -> nil",
    category = "testing"
)]
fn assert_eq_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let cmp = Comparison::parse("assert_eq", args)?;
    match render_diff(Some("assert_eq failed"), cmp.actual, cmp.expected) {
        None => Ok(VmValue::Nil),
        Some(diff) => Err(cmp.or_default(|| diff)),
    }
}

#[harn_builtin(
    sig = "assert_ne(actual: any, expected: any, message?: string) -> nil",
    category = "testing"
)]
fn assert_ne_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let cmp = Comparison::parse("assert_ne", args)?;
    if !values_equal(cmp.actual, cmp.expected) {
        return Ok(VmValue::Nil);
    }
    Err(cmp.or_default(|| format!("assert_ne failed: both values are {}.", repr(cmp.actual))))
}

/// Default tolerance for `assert_approx`: tight enough to catch a real error,
/// loose enough to absorb the rounding of a few chained float operations.
const DEFAULT_APPROX_TOLERANCE: f64 = 1e-9;

#[harn_builtin(
    sig = "assert_approx(actual: any, expected: any, tolerance?: float, message?: string) -> nil",
    category = "testing"
)]
fn assert_approx_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let (Some(actual), Some(expected)) = (args.first(), args.get(1)) else {
        return Err(fail(
            "assert_approx needs two values to compare: assert_approx(actual, expected, tolerance?)"
                .to_string(),
        ));
    };
    let numeric = |v: &VmValue| -> Option<f64> {
        match v {
            VmValue::Int(n) => Some(*n as f64),
            VmValue::Float(n) => Some(*n),
            _ => None,
        }
    };
    let (Some(a), Some(e)) = (numeric(actual), numeric(expected)) else {
        return Err(fail(format!(
            "assert_approx compares numbers, but was given {} and {}.",
            actual.type_name(),
            expected.type_name()
        )));
    };
    let tolerance = match args.get(2) {
        None | Some(VmValue::Nil) => DEFAULT_APPROX_TOLERANCE,
        Some(v) => numeric(v).ok_or_else(|| {
            fail(format!(
                "assert_approx: tolerance must be a number, got {}.",
                v.type_name()
            ))
        })?,
    };
    if tolerance < 0.0 {
        return Err(fail(format!(
            "assert_approx: tolerance must not be negative, got {tolerance}."
        )));
    }
    // NaN is never within tolerance of anything, including itself — the one
    // case where `<=` would quietly report success by returning false.
    let gap = (a - e).abs();
    if gap <= tolerance {
        return Ok(VmValue::Nil);
    }
    let message = args
        .get(3)
        .filter(|m| !matches!(m, VmValue::Nil))
        .map(|m| m.display());
    Err(fail(message.unwrap_or_else(|| {
        format!(
            "assert_approx failed.\n    expected  {e} ± {tolerance}\n    actual    {a}\n    \
             These differ by {gap:e}, which is outside the tolerance."
        )
    })))
}

#[harn_builtin(
    sig = "assert_matches(actual: any, pattern: string, message?: string) -> string",
    category = "testing"
)]
fn assert_matches_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let (Some(actual), Some(pattern)) = (args.first(), args.get(1)) else {
        return Err(fail(
            "assert_matches needs a value and a pattern: assert_matches(actual, pattern)"
                .to_string(),
        ));
    };
    // Only text can match a pattern. Coercing here would let `assert_matches(42,
    // "4")` pass, which is exactly the kind of quiet success an assertion must
    // never grant.
    let VmValue::String(text) = actual else {
        return Err(fail(format!(
            "assert_matches expects text to match against, but was given {} ({}).",
            repr(actual),
            actual.type_name()
        )));
    };
    let VmValue::String(pattern) = pattern else {
        return Err(fail(format!(
            "assert_matches: the pattern must be a string, got {}.",
            pattern.type_name()
        )));
    };
    let regex = crate::stdlib::regex::get_cached_regex(pattern, "")?;
    if regex.is_match(text) {
        return Ok(actual.clone());
    }
    let message = args
        .get(2)
        .filter(|m| !matches!(m, VmValue::Nil))
        .map(|m| m.display());
    Err(fail(message.unwrap_or_else(|| {
        format!(
            "assert_matches failed.\n    pattern   /{pattern}/\n    actual    {}\n    \
             The pattern did not match anywhere in the text.",
            repr(actual)
        )
    })))
}

#[harn_builtin(
    sig = "value_diff(actual: any, expected: any) -> string | nil",
    category = "testing"
)]
fn value_diff_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let cmp = Comparison::parse("value_diff", args)?;
    Ok(match render_diff(None, cmp.actual, cmp.expected) {
        None => VmValue::Nil,
        Some(diff) => VmValue::String(arcstr::ArcStr::from(diff)),
    })
}

#[harn_builtin(sig = "error_category(error: any) -> string", category = "testing")]
fn error_category_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().unwrap_or(&VmValue::Nil);
    match val {
        VmValue::Dict(d) => {
            let cat = d
                .get("category")
                .map(|v| v.display())
                .unwrap_or_else(|| "generic".to_string());
            Ok(VmValue::String(arcstr::ArcStr::from(cat)))
        }
        VmValue::String(s) => {
            let err = VmError::Runtime(s.to_string());
            Ok(VmValue::String(arcstr::ArcStr::from(
                error_to_category(&err).as_str(),
            )))
        }
        _ => Ok(VmValue::String(arcstr::ArcStr::from("generic"))),
    }
}

#[harn_builtin(
    sig = "throw_error(message: string, category?: string) -> never",
    category = "testing"
)]
fn throw_error_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let message = args.first().map(|a| a.display()).unwrap_or_default();
    let category = args
        .get(1)
        .map(|a| ErrorCategory::parse(&a.display()))
        .unwrap_or(ErrorCategory::Generic);

    let mut err_dict = BTreeMap::new();
    err_dict.put_str("message", message.as_str());
    err_dict.put_str("category", category.as_str());
    Err(VmError::Thrown(VmValue::dict(err_dict)))
}

#[harn_builtin(sig = "is_timeout(error: any) -> bool", category = "testing")]
fn is_timeout_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::Bool(error_has_category(
        args.first().unwrap_or(&VmValue::Nil),
        ErrorCategory::Timeout,
    )))
}

#[harn_builtin(sig = "is_rate_limited(error: any) -> bool", category = "testing")]
fn is_rate_limited_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::Bool(error_has_category(
        args.first().unwrap_or(&VmValue::Nil),
        ErrorCategory::RateLimit,
    )))
}

/// Parameterized over the whole `ErrorCategory` taxonomy — `is_timeout` /
/// `is_rate_limited` are just the two pre-wired spellings of this. A harness
/// author can assert any category (`cancelled`, `budget_exceeded`,
/// `server_error`, ...) without the VM hand-wiring a predicate per variant.
#[harn_builtin(
    sig = "error_is(error: any, category: string) -> bool",
    category = "testing"
)]
fn error_is_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let category_str = args.get(1).map(|a| a.display()).unwrap_or_default();
    let category = ErrorCategory::parse(&category_str);
    // `parse` is total (unknown → Generic); reject typos loudly rather than
    // silently asserting against `generic`.
    if category == ErrorCategory::Generic && category_str != "generic" {
        return Err(VmError::Runtime(format!(
            "error_is: unknown error category {category_str:?}"
        )));
    }
    Ok(VmValue::Bool(error_has_category(
        args.first().unwrap_or(&VmValue::Nil),
        category,
    )))
}

/// Whether the error's category is one the agent loop treats as a transient,
/// worth-retrying provider failure — the exact `ErrorCategory::is_transient`
/// oracle, surfaced so tests can assert the retry decision directly.
#[harn_builtin(sig = "error_is_transient(error: any) -> bool", category = "testing")]
fn error_is_transient_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::Bool(
        error_category_of(args.first().unwrap_or(&VmValue::Nil))
            .map(|category| category.is_transient())
            .unwrap_or(false),
    ))
}

/// The category carried by an error value: a structured `{category}` dict's
/// field, or the classification of a raw error string. The dict field is run
/// through `ErrorCategory::parse` so comparisons are by canonical variant (any
/// non-taxonomy string normalizes to `Generic`), matching how `error_is`
/// resolves its argument.
fn error_category_of(val: &VmValue) -> Option<ErrorCategory> {
    match val {
        VmValue::Dict(d) => d
            .get("category")
            .map(|v| ErrorCategory::parse(&v.display())),
        VmValue::String(s) => Some(error_to_category(&VmError::Runtime(s.to_string()))),
        _ => None,
    }
}

fn error_has_category(val: &VmValue, category: ErrorCategory) -> bool {
    error_category_of(val) == Some(category)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict_err(category: &str) -> VmValue {
        VmValue::dict(std::collections::BTreeMap::from([(
            "category".to_string(),
            VmValue::String(arcstr::ArcStr::from(category)),
        )]))
    }

    fn as_bool(result: Result<VmValue, VmError>) -> bool {
        match result.unwrap() {
            VmValue::Bool(matched) => matched,
            other => panic!("expected bool, got {other:?}"),
        }
    }

    fn error_is(error: VmValue, category: &str) -> Result<VmValue, VmError> {
        let mut out = String::new();
        let args = [error, VmValue::String(arcstr::ArcStr::from(category))];
        error_is_impl(&args, &mut out)
    }

    fn thrown_message(result: Result<VmValue, VmError>) -> String {
        match result.unwrap_err() {
            VmError::Thrown(VmValue::String(s)) => s.to_string(),
            other => panic!("expected Thrown(String), got {other:?}"),
        }
    }

    #[test]
    fn assert_with_no_message_uses_default() {
        let mut out = String::new();
        let args = [VmValue::Bool(false)];
        assert_eq!(
            thrown_message(assert_impl(&args, &mut out)),
            "Assertion failed"
        );
    }

    #[test]
    fn assert_with_nil_message_uses_default_instead_of_literal_nil() {
        let mut out = String::new();
        let args = [VmValue::Bool(false), VmValue::Nil];
        assert_eq!(
            thrown_message(assert_impl(&args, &mut out)),
            "Assertion failed"
        );
    }

    #[test]
    fn assert_with_empty_string_message_uses_default() {
        let mut out = String::new();
        let args = [
            VmValue::Bool(false),
            VmValue::String(arcstr::ArcStr::from("")),
        ];
        assert_eq!(
            thrown_message(assert_impl(&args, &mut out)),
            "Assertion failed"
        );
    }

    /// The realistic footgun this guards: `assert(cond, json_stringify(x))`
    /// where `x` turns out to be nil. `json_stringify(nil)` correctly
    /// returns the string `"null"` (issue is NOT with `json_stringify`);
    /// forwarding it verbatim as the assertion message must not surface the
    /// bare word `null` as though it were real diagnostic content.
    #[test]
    fn assert_with_message_equal_to_json_null_uses_default() {
        let mut out = String::new();
        let args = [
            VmValue::Bool(false),
            VmValue::String(arcstr::ArcStr::from("null")),
        ];
        assert_eq!(
            thrown_message(assert_impl(&args, &mut out)),
            "Assertion failed"
        );
    }

    #[test]
    fn assert_with_real_message_is_preserved() {
        let mut out = String::new();
        let args = [
            VmValue::Bool(false),
            VmValue::String(arcstr::ArcStr::from("receipt was missing a field")),
        ];
        assert_eq!(
            thrown_message(assert_impl(&args, &mut out)),
            "receipt was missing a field"
        );
    }

    /// The uninformative-message guard ported into `Comparison::parse`: a
    /// message that stringifies to the bare `"null"` that `json_stringify(nil)`
    /// produces carries no signal, so `assert_eq` shows the structural diff
    /// instead of surfacing the literal word `null` as if it were diagnostic.
    #[test]
    fn assert_eq_with_json_null_message_falls_back_to_the_diff() {
        assert_eq!(
            failure_text(
                assert_eq_impl,
                &[VmValue::Int(1), VmValue::Int(2), s("null")]
            ),
            "assert_eq failed.\n    expected  2\n    actual    1"
        );
    }

    /// An empty (or whitespace-only) override is likewise uninformative and
    /// falls back to the diff rather than throwing a blank message.
    #[test]
    fn assert_eq_with_empty_message_falls_back_to_the_diff() {
        assert_eq!(
            failure_text(assert_eq_impl, &[VmValue::Int(1), VmValue::Int(2), s("")]),
            "assert_eq failed.\n    expected  2\n    actual    1"
        );
    }

    /// The same ported guard governs `assert_ne`: nil, blank, and the bare
    /// `"null"` string all fall back to the synthesized default rather than
    /// replacing it with an empty or misleading message.
    #[test]
    fn assert_ne_with_uninformative_message_falls_back_to_the_default() {
        for message in [VmValue::Nil, s(""), s("null")] {
            assert_eq!(
                failure_text(assert_ne_impl, &[VmValue::Int(5), VmValue::Int(5), message]),
                "assert_ne failed: both values are 5."
            );
        }
    }

    #[test]
    fn error_is_matches_any_category_and_subsumes_the_legacy_predicates() {
        assert!(as_bool(error_is(dict_err("cancelled"), "cancelled")));
        assert!(as_bool(error_is(
            dict_err("budget_exceeded"),
            "budget_exceeded"
        )));
        assert!(!as_bool(error_is(dict_err("timeout"), "rate_limit")));
        let mut out = String::new();
        assert!(as_bool(is_timeout_impl(&[dict_err("timeout")], &mut out)));
    }

    #[test]
    fn error_is_rejects_unknown_categories() {
        assert!(error_is(dict_err("timeout"), "not_a_category").is_err());
    }

    #[test]
    fn error_is_transient_uses_the_retry_oracle() {
        let mut out = String::new();
        assert!(as_bool(error_is_transient_impl(
            &[dict_err("rate_limit")],
            &mut out
        )));
        assert!(!as_bool(error_is_transient_impl(
            &[dict_err("auth")],
            &mut out
        )));
    }

    // ---------------------------------------------------------------------------------------------
    // Assertion builtins
    // ---------------------------------------------------------------------------------------------

    fn s(text: &str) -> VmValue {
        VmValue::String(arcstr::ArcStr::from(text))
    }

    /// The text an assertion throws, or a panic if it unexpectedly passed.
    fn failure_text(
        builtin: fn(&[VmValue], &mut String) -> Result<VmValue, VmError>,
        args: &[VmValue],
    ) -> String {
        let mut out = String::new();
        match builtin(args, &mut out) {
            Err(VmError::Thrown(VmValue::String(text))) => text.to_string(),
            other => panic!("expected a thrown failure, got {other:?}"),
        }
    }

    fn passes(
        builtin: fn(&[VmValue], &mut String) -> Result<VmValue, VmError>,
        args: &[VmValue],
    ) -> bool {
        let mut out = String::new();
        builtin(args, &mut out).is_ok()
    }

    /// The load-bearing convention: argument one is the value under test.
    /// If this ever flips, every diff in the language silently inverts, so it
    /// is pinned by the rendered text rather than by a comment.
    #[test]
    fn assert_eq_reads_argument_one_as_actual_and_two_as_expected() {
        assert_eq!(
            failure_text(assert_eq_impl, &[VmValue::Int(1), VmValue::Int(2)]),
            "assert_eq failed.\n    expected  2\n    actual    1"
        );
    }

    #[test]
    fn assert_eq_passes_on_equal_values() {
        assert!(passes(assert_eq_impl, &[VmValue::Int(1), VmValue::Int(1)]));
        // Cross-type numeric equality follows the language's `==`, not a
        // stricter rule invented here.
        assert!(passes(
            assert_eq_impl,
            &[VmValue::Int(1), VmValue::Float(1.0)]
        ));
    }

    #[test]
    fn a_caller_message_replaces_the_diff() {
        assert_eq!(
            failure_text(
                assert_eq_impl,
                &[VmValue::Int(1), VmValue::Int(2), s("ids must line up")]
            ),
            "ids must line up"
        );
    }

    /// `message = nil` is what a Harn wrapper forwards when its own optional
    /// message was omitted. Treating that as "the message is the text `nil`"
    /// would replace every diff with the word `nil`.
    #[test]
    fn an_explicitly_nil_message_still_yields_the_diff() {
        assert!(failure_text(
            assert_eq_impl,
            &[VmValue::Int(1), VmValue::Int(2), VmValue::Nil]
        )
        .starts_with("assert_eq failed."));
    }

    #[test]
    fn assert_eq_needs_two_values() {
        assert_eq!(
            failure_text(assert_eq_impl, &[VmValue::Int(1)]),
            "assert_eq needs two values to compare: assert_eq(actual, expected)"
        );
    }

    #[test]
    fn assert_ne_fails_only_when_the_values_match() {
        assert!(passes(assert_ne_impl, &[VmValue::Int(1), VmValue::Int(2)]));
        assert_eq!(
            failure_text(assert_ne_impl, &[s("x"), s("x")]),
            "assert_ne failed: both values are \"x\"."
        );
    }

    #[test]
    fn assert_approx_absorbs_float_rounding_but_not_real_error() {
        assert!(passes(
            assert_approx_impl,
            &[VmValue::Float(0.1 + 0.2), VmValue::Float(0.3)]
        ));
        assert_eq!(
            failure_text(
                assert_approx_impl,
                &[VmValue::Float(1.0), VmValue::Float(2.0)]
            ),
            "assert_approx failed.\n    expected  2 ± 0.000000001\n    actual    1\n    \
             These differ by 1e0, which is outside the tolerance."
        );
    }

    #[test]
    fn assert_approx_honours_an_explicit_tolerance() {
        assert!(passes(
            assert_approx_impl,
            &[
                VmValue::Float(1.0),
                VmValue::Float(1.4),
                VmValue::Float(0.5)
            ]
        ));
        assert!(!passes(
            assert_approx_impl,
            &[
                VmValue::Float(1.0),
                VmValue::Float(1.6),
                VmValue::Float(0.5)
            ]
        ));
    }

    /// NaN compares false against everything, so a tolerance check written as
    /// `gap <= tolerance` correctly rejects it — but only by accident of IEEE
    /// semantics, which is exactly the kind of thing that gets "simplified"
    /// into a bug later.
    #[test]
    fn assert_approx_never_passes_on_nan() {
        assert!(!passes(
            assert_approx_impl,
            &[VmValue::Float(f64::NAN), VmValue::Float(f64::NAN)]
        ));
        assert!(!passes(
            assert_approx_impl,
            &[VmValue::Float(f64::NAN), VmValue::Float(1.0)]
        ));
    }

    #[test]
    fn assert_approx_rejects_values_it_cannot_compare_numerically() {
        assert_eq!(
            failure_text(assert_approx_impl, &[s("1.0"), VmValue::Float(1.0)]),
            "assert_approx compares numbers, but was given string and float."
        );
        assert_eq!(
            failure_text(
                assert_approx_impl,
                &[VmValue::Float(1.0), VmValue::Float(1.0), s("loose")]
            ),
            "assert_approx: tolerance must be a number, got string."
        );
        assert_eq!(
            failure_text(
                assert_approx_impl,
                &[
                    VmValue::Float(1.0),
                    VmValue::Float(9.0),
                    VmValue::Float(-1.0)
                ]
            ),
            "assert_approx: tolerance must not be negative, got -1."
        );
    }

    #[test]
    fn assert_matches_tests_a_pattern_against_text() {
        assert!(passes(
            assert_matches_impl,
            &[s("order-1234"), s("^order-\\d+$")]
        ));
        assert_eq!(
            failure_text(assert_matches_impl, &[s("order-abc"), s("^order-\\d+$")]),
            "assert_matches failed.\n    pattern   /^order-\\d+$/\n    actual    \"order-abc\"\n    \
             The pattern did not match anywhere in the text."
        );
    }

    /// A pattern is a claim about text. Stringifying the subject first would
    /// let `assert_matches(1234, "\\d+")` pass, which tests the renderer
    /// rather than the code.
    #[test]
    fn assert_matches_refuses_a_non_string_subject() {
        assert_eq!(
            failure_text(assert_matches_impl, &[VmValue::Int(1234), s("\\d+")]),
            "assert_matches expects text to match against, but was given 1234 (int)."
        );
    }

    #[test]
    fn value_diff_is_nil_when_equal_and_headline_free_when_not() {
        let mut out = String::new();
        assert!(matches!(
            value_diff_impl(&[VmValue::Int(1), VmValue::Int(1)], &mut out),
            Ok(VmValue::Nil)
        ));
        let VmValue::String(rendered) =
            value_diff_impl(&[VmValue::Int(1), VmValue::Int(2)], &mut out).unwrap()
        else {
            panic!("expected the rendered diff");
        };
        assert_eq!(rendered.as_str(), "    expected  2\n    actual    1");
    }
}
