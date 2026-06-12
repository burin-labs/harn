use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use crate::runtime_limits::RuntimeLimits;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const REGEX_CACHE_LIMIT: usize = RuntimeLimits::DEFAULT.max_regex_cache_entries;

thread_local! {
    // `regex::Regex` is stored behind an `Rc` because `Regex::clone` deep-copies
    // the compiled program and its match-cache pool — ~3.5us per call, which
    // dominated repeated `regex_match` over a file's lines. Handing callers an
    // `Rc<Regex>` makes each lookup a refcount bump instead.
    static REGEX_CACHE: RefCell<HashMap<String, Rc<regex::Regex>>> = RefCell::new(HashMap::new());
    // Scan loops almost always reuse the same pattern (e.g. `regex_match(p, line)`
    // across every line of a file). This single-slot memo short-circuits those
    // calls before the cache-key allocation and the HashMap hash, leaving only a
    // string compare plus the `Rc` bump.
    static LAST_REGEX: RefCell<Option<(String, String, Rc<regex::Regex>)>> =
        const { RefCell::new(None) };
}

fn get_cached_regex(pattern: &str, flags: &str) -> Result<Rc<regex::Regex>, VmError> {
    if let Some(re) = LAST_REGEX.with(|slot| {
        slot.borrow()
            .as_ref()
            .filter(|(p, f, _)| p == pattern && f == flags)
            .map(|(_, _, re)| Rc::clone(re))
    }) {
        return Ok(re);
    }

    let re = REGEX_CACHE.with(|cache| -> Result<Rc<regex::Regex>, VmError> {
        let cache_key = format!("{flags}\0{pattern}");
        let mut cache = cache.borrow_mut();
        if let Some(re) = cache.get(&cache_key) {
            return Ok(Rc::clone(re));
        }
        let re = Rc::new(build_regex(pattern, flags).map_err(|e| {
            VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
                "Invalid regex: {e}"
            ))))
        })?);
        if cache.len() >= REGEX_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(cache_key, Rc::clone(&re));
        Ok(re)
    })?;

    LAST_REGEX.with(|slot| {
        *slot.borrow_mut() = Some((pattern.to_string(), flags.to_string(), Rc::clone(&re)));
    });
    Ok(re)
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

fn build_regex(pattern: &str, flags: &str) -> Result<regex::Regex, String> {
    let mut builder = regex::RegexBuilder::new(pattern);
    for flag in flags.chars() {
        match flag {
            'i' => builder.case_insensitive(true),
            'm' => builder.multi_line(true),
            's' => builder.dot_matches_new_line(true),
            'x' => builder.ignore_whitespace(true),
            _ => {
                return Err(format!(
                    "unsupported regex flag '{flag}', expected one of i/m/s/x"
                ));
            }
        };
    }
    builder.build().map_err(|e| e.to_string())
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
    sig = "regex_match(pattern: string?, text: string?, flags?: string) -> list | nil",
    category = "regex"
)]
fn regex_match_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let (Some(pattern), Some(text)) = (required_str(args, 0), required_str(args, 1)) else {
        return Ok(VmValue::Nil);
    };
    let flags = optional_flags(args, 2);
    let re = get_cached_regex(&pattern, &flags)?;
    let matches: Vec<VmValue> = re
        .find_iter(&text)
        .map(|m| VmValue::String(std::sync::Arc::from(m.as_str())))
        .collect();
    if matches.is_empty() {
        return Ok(VmValue::Nil);
    }
    Ok(VmValue::List(std::sync::Arc::new(matches)))
}

// Both `regex_replace` and `regex_replace_all` replace every match via the
// `regex` crate (supports `$1`, `${name}` backrefs). The `_all` spelling is
// a discoverability alias on the same implementation.
#[harn_builtin(
    sig = "regex_replace(pattern: string?, replacement: string?, text: string?, flags?: string) -> string",
    aliases = ["regex_replace_all"],
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
    let re = get_cached_regex(&pattern, &flags)?;
    Ok(VmValue::String(std::sync::Arc::from(
        re.replace_all(&text, replacement.as_ref()).into_owned(),
    )))
}

#[harn_builtin(
    sig = "regex_captures(pattern: string?, text: string?, flags?: string) -> list",
    category = "regex"
)]
fn regex_captures_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let (Some(pattern), Some(text)) = (required_str(args, 0), required_str(args, 1)) else {
        return Ok(VmValue::List(std::sync::Arc::new(Vec::new())));
    };
    let flags = optional_flags(args, 2);
    let re = get_cached_regex(&pattern, &flags)?;

    // `start`/`end` are *character* (code-point) offsets, not byte offsets, so
    // they compose with Harn's char-based `substring`/`index_of`/`len` (and
    // match Python's `re` offsets). `line` is the 1-based line of the match
    // start — the equivalent of Python's `text.count("\n", 0, m.start()) + 1` —
    // so callers can report diagnostics positionally without re-scanning the
    // input in Harn.
    //
    // The `regex` crate reports byte offsets, so we walk a cursor forward over
    // the gaps between successive matches (which arrive in ascending order),
    // accumulating the char count and newline count of each gap. That keeps the
    // whole pass O(n) rather than O(n * matches).
    let mut scanned_byte: usize = 0;
    let mut chars_before: usize = 0;
    let mut newlines_before: usize = 0;

    let mut results: Vec<VmValue> = Vec::new();
    for caps in re.captures_iter(&text) {
        let mut dict = BTreeMap::new();

        dict.insert(
            "match".to_string(),
            VmValue::String(std::sync::Arc::from(caps.get(0).map_or("", |m| m.as_str()))),
        );

        let groups: Vec<VmValue> = (1..caps.len())
            .map(|i| match caps.get(i) {
                Some(m) => VmValue::String(std::sync::Arc::from(m.as_str())),
                None => VmValue::Nil,
            })
            .collect();
        dict.insert(
            "groups".to_string(),
            VmValue::List(std::sync::Arc::new(groups)),
        );

        if let Some(whole) = caps.get(0) {
            let (start_byte, end_byte) = (whole.start(), whole.end());
            // Advance the cursor to the match start, tallying chars + newlines.
            // `captures_iter` never yields a start before a previous one, so the
            // cursor only moves forward.
            let gap = &text[scanned_byte..start_byte];
            chars_before += gap.chars().count();
            newlines_before += gap.bytes().filter(|&b| b == b'\n').count();
            let start_char = chars_before;
            let line = newlines_before + 1;
            // Tally the match interior too, so a match that spans newlines does
            // not throw off the line number of subsequent matches.
            let matched = whole.as_str();
            chars_before += matched.chars().count();
            newlines_before += matched.bytes().filter(|&b| b == b'\n').count();
            let end_char = chars_before;
            scanned_byte = end_byte;

            dict.insert("start".to_string(), VmValue::Int(start_char as i64));
            dict.insert("end".to_string(), VmValue::Int(end_char as i64));
            dict.insert("line".to_string(), VmValue::Int(line as i64));
        }

        for name in re.capture_names().flatten() {
            if let Some(m) = caps.name(name) {
                dict.insert(
                    name.to_string(),
                    VmValue::String(std::sync::Arc::from(m.as_str())),
                );
            }
        }

        results.push(VmValue::Dict(std::sync::Arc::new(dict)));
    }
    Ok(VmValue::List(std::sync::Arc::new(results)))
}

#[harn_builtin(
    sig = "regex_split(text: string?, pattern: string?, flags?: string) -> list",
    category = "regex"
)]
fn regex_split_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let (Some(text), Some(pattern)) = (required_str(args, 0), required_str(args, 1)) else {
        return Ok(VmValue::Nil);
    };
    let flags = optional_flags(args, 2);
    let re = get_cached_regex(&pattern, &flags)?;
    Ok(VmValue::List(std::sync::Arc::new(
        re.split(&text)
            .map(|part| VmValue::String(std::sync::Arc::from(part)))
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
        VmValue::String(std::sync::Arc::from(v))
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
    fn cache_returns_consistent_results() {
        let mut vm = vm();
        let a = call(&mut vm, "regex_match", vec![s(r"\d+"), s("42")]).unwrap();
        let b = call(&mut vm, "regex_match", vec![s(r"\d+"), s("42")]).unwrap();
        assert_eq!(a.display(), b.display());
    }

    #[test]
    fn cache_eviction_still_works() {
        for i in 0..70 {
            let pattern = format!("pat{i}");
            let _ = get_cached_regex(&pattern, "");
        }
        let re = get_cached_regex("pat0", "").unwrap();
        assert!(re.is_match("pat0"));
    }

    // A repeated lookup must hand back the *same* compiled regex, not a deep
    // clone. `regex::Regex::clone` re-copies the compiled program and match
    // cache (~3.5us/call), so cloning per call is what made line-oriented
    // scans slow (#2796). Sharing via `Rc` keeps the hot path a refcount bump;
    // this guards against a regression back to per-call cloning.
    #[test]
    fn cache_returns_shared_instance() {
        let first = get_cached_regex("shared_pattern", "").unwrap();
        let second = get_cached_regex("shared_pattern", "").unwrap();
        assert!(Rc::ptr_eq(&first, &second));
    }
}
