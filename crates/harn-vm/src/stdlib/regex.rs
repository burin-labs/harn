use crate::value::VmDictExt;
use std::collections::BTreeMap;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

fn regex_error(error: String) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
        "Invalid regex: {error}"
    ))))
}

/// Read an optional trailing `flags` argument. An absent arg *and* an explicit
/// `nil` both mean "no flags": forwarding an unset optional (e.g.
/// `regex_captures(p, t, opts?.flags)`) yields `nil`, and without this guard
/// `Nil.as_str_cow()` stringifies to `"nil"`, which `build_regex` would then
/// reject as bogus flag letters. Applies to every regex builtin's flags slot.
fn optional_flags(args: &[VmValue], idx: usize) -> std::borrow::Cow<'_, str> {
    match args.get(idx) {
        None | Some(VmValue::Nil) => std::borrow::Cow::Borrowed(""),
        Some(v) => v.as_str_cow(),
    }
}

/// Read a required string argument (pattern/text/replacement). Returns
/// `None` when the slot is absent *or* explicitly `nil`, so each builtin
/// can take its missing-argument fallback. Without this guard
/// `Nil.as_str_cow()` stringifies to `"nil"`, and a forwarded unset
/// optional would silently be compiled — and matched — as the literal
/// pattern/text `nil`.
fn required_str(args: &[VmValue], idx: usize) -> Option<std::borrow::Cow<'_, str>> {
    match args.get(idx) {
        None | Some(VmValue::Nil) => None,
        Some(v) => Some(v.as_str_cow()),
    }
}

pub(crate) fn register_regex_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &REGEX_MATCH_IMPL_DEF,
    &REGEX_REPLACE_IMPL_DEF,
    &REGEX_CAPTURES_IMPL_DEF,
    &REGEX_SPLIT_IMPL_DEF,
];

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig_expr = harn_builtin_meta::signatures::PORTABLE_REGEX_MATCH,
    category = "regex"
)]
fn regex_match_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let (Some(pattern), Some(text)) = (required_str(args, 0), required_str(args, 1)) else {
        return Ok(VmValue::Nil);
    };
    let flags = optional_flags(args, 2);
    let matches: Vec<VmValue> = harn_kernel::pure::regex_matches(&pattern, &text, &flags)
        .map_err(regex_error)?
        .into_iter()
        .map(|matched| VmValue::String(arcstr::ArcStr::from(matched)))
        .collect();
    if matches.is_empty() {
        return Ok(VmValue::Nil);
    }
    Ok(VmValue::List(std::sync::Arc::new(matches)))
}

// Replace every match via the `regex` crate (supports `$1` and `${name}`
// backreferences).
#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig_expr = harn_builtin_meta::signatures::PORTABLE_REGEX_REPLACE,
    category = "regex"
)]
fn regex_replace_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let (Some(pattern), Some(replacement), Some(text)) = (
        required_str(args, 0),
        required_str(args, 1),
        required_str(args, 2),
    ) else {
        return Ok(VmValue::Nil);
    };
    let flags = optional_flags(args, 3);
    let replaced = harn_kernel::pure::regex_replace(&pattern, &replacement, &text, &flags)
        .map_err(regex_error)?;
    Ok(VmValue::String(arcstr::ArcStr::from(replaced)))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig_expr = harn_builtin_meta::signatures::PORTABLE_REGEX_CAPTURES,
    category = "regex"
)]
fn regex_captures_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let (Some(pattern), Some(text)) = (required_str(args, 0), required_str(args, 1)) else {
        return Ok(VmValue::List(std::sync::Arc::new(Vec::new())));
    };
    let flags = optional_flags(args, 2);
    let captures =
        harn_kernel::pure::regex_captures(&pattern, &text, &flags).map_err(regex_error)?;
    let mut results: Vec<VmValue> = Vec::with_capacity(captures.len());
    for capture in captures {
        let mut dict = BTreeMap::new();
        dict.put_str("match", &capture.full_match);
        let groups: Vec<VmValue> = capture
            .groups
            .into_iter()
            .map(|value| {
                value.map_or(VmValue::Nil, |value| {
                    VmValue::String(arcstr::ArcStr::from(value))
                })
            })
            .collect();
        dict.insert(
            "groups".to_string(),
            VmValue::List(std::sync::Arc::new(groups)),
        );
        dict.insert("start".to_string(), VmValue::Int(capture.start as i64));
        dict.insert("end".to_string(), VmValue::Int(capture.end as i64));
        dict.insert("line".to_string(), VmValue::Int(capture.line as i64));
        dict.extend(
            capture
                .named
                .into_iter()
                .map(|(name, value)| (name, VmValue::String(arcstr::ArcStr::from(value)))),
        );
        results.push(VmValue::dict(dict));
    }
    Ok(VmValue::List(std::sync::Arc::new(results)))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig_expr = harn_builtin_meta::signatures::PORTABLE_REGEX_SPLIT,
    category = "regex"
)]
fn regex_split_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let (Some(text), Some(pattern)) = (required_str(args, 0), required_str(args, 1)) else {
        return Ok(VmValue::Nil);
    };
    let flags = optional_flags(args, 2);
    let parts = harn_kernel::pure::regex_split(&pattern, &text, &flags).map_err(regex_error)?;
    Ok(VmValue::List(std::sync::Arc::new(
        parts
            .into_iter()
            .map(|part| VmValue::String(arcstr::ArcStr::from(part)))
            .collect(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::Vm;

    fn vm() -> Vm {
        let mut vm = Vm::new();
        register_regex_builtins(&mut vm);
        vm
    }

    fn call(vm: &mut Vm, name: &str, args: Vec<VmValue>) -> Result<VmValue, VmError> {
        let f = vm.builtins.get(name).unwrap().clone();
        let mut out = String::new();
        f(&args, &mut out)
    }

    fn s(v: &str) -> VmValue {
        VmValue::String(arcstr::ArcStr::from(v))
    }

    fn unwrap_list(v: &VmValue) -> &Vec<VmValue> {
        match v {
            VmValue::List(l) => l,
            _ => panic!("expected List, got {:?}", v.display()),
        }
    }

    #[test]
    fn match_basic() {
        let mut vm = vm();
        let result = call(
            &mut vm,
            "regex_match",
            vec![s(r"\d+"), s("abc 123 def 456")],
        )
        .unwrap();
        let list = unwrap_list(&result);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].display(), "123");
        assert_eq!(list[1].display(), "456");
    }

    #[test]
    fn match_no_match_returns_nil() {
        let mut vm = vm();
        let result = call(&mut vm, "regex_match", vec![s(r"\d+"), s("no digits here")]).unwrap();
        assert!(matches!(result, VmValue::Nil));
    }

    #[test]
    fn match_empty_pattern() {
        let mut vm = vm();
        let result = call(&mut vm, "regex_match", vec![s(""), s("abc")]).unwrap();
        let list = unwrap_list(&result);
        assert_eq!(list.len(), 4);
    }

    #[test]
    fn match_missing_args_returns_nil() {
        let mut vm = vm();
        let result = call(&mut vm, "regex_match", vec![s(r"\d+")]).unwrap();
        assert!(matches!(result, VmValue::Nil));
    }

    #[test]
    fn match_invalid_regex_errors() {
        let mut vm = vm();
        let result = call(&mut vm, "regex_match", vec![s(r"[invalid"), s("text")]);
        assert!(result.is_err());
    }

    #[test]
    fn match_unicode() {
        let mut vm = vm();
        let result = call(&mut vm, "regex_match", vec![s(r"\w+"), s("café résumé")]).unwrap();
        let list = unwrap_list(&result);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].display(), "café");
        assert_eq!(list[1].display(), "résumé");
    }

    #[test]
    fn replace_basic() {
        let mut vm = vm();
        let result = call(
            &mut vm,
            "regex_replace",
            vec![s(r"\d+"), s("NUM"), s("abc 123 def 456")],
        )
        .unwrap();
        assert_eq!(result.display(), "abc NUM def NUM");
    }

    #[test]
    fn replace_no_match_returns_original() {
        let mut vm = vm();
        let result = call(
            &mut vm,
            "regex_replace",
            vec![s(r"\d+"), s("NUM"), s("no digits")],
        )
        .unwrap();
        assert_eq!(result.display(), "no digits");
    }

    #[test]
    fn replace_with_backreference() {
        let mut vm = vm();
        let result = call(
            &mut vm,
            "regex_replace",
            vec![s(r"(\w+)\s(\w+)"), s("$2 $1"), s("hello world")],
        )
        .unwrap();
        assert_eq!(result.display(), "world hello");
    }

    #[test]
    fn replace_honors_optional_flags() {
        let mut vm = vm();
        let result = call(
            &mut vm,
            "regex_replace",
            vec![s("hello"), s("hi"), s("HeLLo HELLO"), s("i")],
        )
        .unwrap();
        assert_eq!(result.display(), "hi hi");
    }

    #[test]
    fn replace_missing_args_returns_nil() {
        let mut vm = vm();
        let result = call(&mut vm, "regex_replace", vec![s(r"\d+"), s("X")]).unwrap();
        assert!(matches!(result, VmValue::Nil));
    }

    #[test]
    fn captures_with_groups() {
        let mut vm = vm();
        let result = call(
            &mut vm,
            "regex_captures",
            vec![s(r"(\d+)-(\w+)"), s("123-abc 456-def")],
        )
        .unwrap();
        let list = unwrap_list(&result);
        assert_eq!(list.len(), 2);

        let first = list[0].as_dict().unwrap();
        assert_eq!(first.get("match").unwrap().display(), "123-abc");
        let groups = unwrap_list(first.get("groups").unwrap());
        assert_eq!(groups[0].display(), "123");
        assert_eq!(groups[1].display(), "abc");
    }

    #[test]
    fn captures_named_groups() {
        let mut vm = vm();
        let result = call(
            &mut vm,
            "regex_captures",
            vec![s(r"(?P<year>\d{4})-(?P<month>\d{2})"), s("2024-01")],
        )
        .unwrap();
        let list = unwrap_list(&result);
        assert_eq!(list.len(), 1);
        let cap = list[0].as_dict().unwrap();
        assert_eq!(cap.get("year").unwrap().display(), "2024");
        assert_eq!(cap.get("month").unwrap().display(), "01");
    }

    #[test]
    fn captures_no_match_returns_empty_list() {
        let mut vm = vm();
        let result = call(&mut vm, "regex_captures", vec![s(r"\d+"), s("no digits")]).unwrap();
        let list = unwrap_list(&result);
        assert!(list.is_empty());
    }

    #[test]
    fn captures_optional_group_nil() {
        let mut vm = vm();
        let result = call(
            &mut vm,
            "regex_captures",
            vec![s(r"(\d+)(?:-(\w+))?"), s("123")],
        )
        .unwrap();
        let list = unwrap_list(&result);
        assert_eq!(list.len(), 1);
        let groups = unwrap_list(list[0].as_dict().unwrap().get("groups").unwrap());
        assert_eq!(groups[0].display(), "123");
        assert!(matches!(groups[1], VmValue::Nil));
    }

    #[test]
    fn captures_expose_offsets_and_line() {
        let mut vm = vm();
        // Two matches across three lines; the second starts on line 3.
        let result = call(
            &mut vm,
            "regex_captures",
            vec![s(r"(\w+)=(\d+)"), s("a=1\n\nb=22\n")],
        )
        .unwrap();
        let list = unwrap_list(&result);
        assert_eq!(list.len(), 2);

        let first = list[0].as_dict().unwrap();
        assert_eq!(first.get("start").unwrap().as_int(), Some(0));
        assert_eq!(first.get("end").unwrap().as_int(), Some(3));
        assert_eq!(first.get("line").unwrap().as_int(), Some(1));

        let second = list[1].as_dict().unwrap();
        // "a=1\n\n" is 5 bytes, so the second match starts at offset 5 on line 3.
        assert_eq!(second.get("start").unwrap().as_int(), Some(5));
        assert_eq!(second.get("end").unwrap().as_int(), Some(9));
        assert_eq!(second.get("line").unwrap().as_int(), Some(3));
    }

    #[test]
    fn captures_line_counts_multibyte_prefix() {
        let mut vm = vm();
        // "café\nX": é is 2 bytes but 1 char. The match `X` is the 6th code
        // point (char offset 5) on line 2 — offsets are char-based, not byte.
        let result = call(&mut vm, "regex_captures", vec![s(r"X"), s("café\nX")]).unwrap();
        let list = unwrap_list(&result);
        assert_eq!(list.len(), 1);
        let cap = list[0].as_dict().unwrap();
        assert_eq!(cap.get("start").unwrap().as_int(), Some(5));
        assert_eq!(cap.get("end").unwrap().as_int(), Some(6));
        assert_eq!(cap.get("line").unwrap().as_int(), Some(2));
    }

    #[test]
    fn captures_accepts_flags() {
        let mut vm = vm();
        // Case-insensitive flag parity with regex_match/replace/split.
        let result = call(
            &mut vm,
            "regex_captures",
            vec![s("foo"), s("FOO foo"), s("i")],
        )
        .unwrap();
        let list = unwrap_list(&result);
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn captures_inline_and_trailing_flags_span_newlines() {
        let mut vm = vm();
        let text = "before\n<Tool_Result id=\"42\">\nAlpha\nbeta\n</Tool_Result>\nafter";

        let inline = call(
            &mut vm,
            "regex_captures",
            vec![
                s(r"(?is)<tool_result\b([^>]*)>(.*?)</tool_result>"),
                s(text),
            ],
        )
        .unwrap();
        let inline_list = unwrap_list(&inline);
        assert_eq!(inline_list.len(), 1);
        let inline_cap = inline_list[0].as_dict().unwrap();
        assert_eq!(
            inline_cap.get("match").unwrap().display(),
            "<Tool_Result id=\"42\">\nAlpha\nbeta\n</Tool_Result>"
        );
        let inline_groups = unwrap_list(inline_cap.get("groups").unwrap());
        assert_eq!(inline_groups.len(), 2);
        assert_eq!(inline_groups[0].display(), " id=\"42\"");
        assert_eq!(inline_groups[1].display(), "\nAlpha\nbeta\n");
        assert_eq!(inline_cap.get("start").unwrap().as_int(), Some(7));
        assert_eq!(inline_cap.get("end").unwrap().as_int(), Some(54));
        assert_eq!(inline_cap.get("line").unwrap().as_int(), Some(2));

        let trailing = call(
            &mut vm,
            "regex_captures",
            vec![
                s(r"<tool_result\b([^>]*)>(.*?)</tool_result>"),
                s(text),
                s("is"),
            ],
        )
        .unwrap();
        assert_eq!(trailing.display(), inline.display());

        let class_any = call(
            &mut vm,
            "regex_captures",
            vec![
                s(r"(?i)<tool_result\b([^>]*)>([\s\S]*?)</tool_result>"),
                s(text),
            ],
        )
        .unwrap();
        assert_eq!(class_any.display(), inline.display());
    }

    #[test]
    fn nil_flags_arg_means_no_flags() {
        // Forwarding an unset optional (`opts?.flags`) lands a literal `nil` in
        // the flags slot; it must behave like an absent arg, not stringify to
        // "nil" and get rejected as bogus flag letters. Covers all four regex
        // builtins, since they share `optional_flags`.
        let mut vm = vm();
        let caps = call(
            &mut vm,
            "regex_captures",
            vec![s(r"\d+"), s("a1 b2"), VmValue::Nil],
        )
        .unwrap();
        assert_eq!(unwrap_list(&caps).len(), 2);

        let m = call(
            &mut vm,
            "regex_match",
            vec![s(r"\d+"), s("a1 b2"), VmValue::Nil],
        )
        .unwrap();
        assert_eq!(unwrap_list(&m).len(), 2);

        let split = call(&mut vm, "regex_split", vec![s("a,b"), s(","), VmValue::Nil]).unwrap();
        assert_eq!(unwrap_list(&split).len(), 2);

        let replaced = call(
            &mut vm,
            "regex_replace",
            vec![s(r"\d"), s("#"), s("a1b2"), VmValue::Nil],
        )
        .unwrap();
        assert_eq!(replaced.display(), "a#b#");
    }

    #[test]
    fn nil_required_args_take_missing_arg_fallback() {
        // A nil pattern/text must NOT stringify to the literal "nil"
        // (which would compile — and match — as a real regex). Each
        // builtin takes the same fallback as a missing argument.
        let mut vm = vm();

        // Would previously compile pattern "nil" and find it in the text.
        let m = call(&mut vm, "regex_match", vec![VmValue::Nil, s("a nil value")]).unwrap();
        assert!(matches!(m, VmValue::Nil), "got {:?}", m.display());
        let m = call(&mut vm, "regex_match", vec![s("nil"), VmValue::Nil]).unwrap();
        assert!(matches!(m, VmValue::Nil), "got {:?}", m.display());

        let c = call(
            &mut vm,
            "regex_captures",
            vec![VmValue::Nil, s("a nil value")],
        )
        .unwrap();
        assert!(unwrap_list(&c).is_empty());

        let r = call(
            &mut vm,
            "regex_replace",
            vec![VmValue::Nil, s("#"), s("a1b2")],
        )
        .unwrap();
        assert!(matches!(r, VmValue::Nil), "got {:?}", r.display());

        let sp = call(&mut vm, "regex_split", vec![VmValue::Nil, s(",")]).unwrap();
        assert!(matches!(sp, VmValue::Nil), "got {:?}", sp.display());
    }

    #[test]
    fn cache_eviction_still_works() {
        for i in 0..70 {
            let pattern = format!("pat{i}");
            let _ = harn_kernel::pure::regex_matches(&pattern, "", "");
        }
        assert_eq!(
            harn_kernel::pure::regex_matches("pat0", "pat0", "").unwrap(),
            ["pat0"]
        );
    }
}
