use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

thread_local! {
    static REGEX_CACHE: RefCell<Vec<(String, regex::Regex)>> = const { RefCell::new(Vec::new()) };
}

fn get_cached_regex(pattern: &str) -> Result<regex::Regex, VmError> {
    REGEX_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(pos) = cache.iter().position(|(p, _)| p == pattern) {
            return Ok(cache[pos].1.clone());
        }
        let re = regex::Regex::new(pattern).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!("Invalid regex: {e}"))))
        })?;
        cache.push((pattern.to_string(), re.clone()));
        if cache.len() > 64 {
            cache.remove(0);
        }
        Ok(re)
    })
}

pub(crate) fn register_regex_builtins(vm: &mut Vm) {
    vm.register_builtin("regex_match", |args, _out| {
        if args.len() >= 2 {
            let pattern = args[0].display();
            let text = args[1].display();
            let re = get_cached_regex(&pattern)?;
            let matches: Vec<VmValue> = re
                .find_iter(&text)
                .map(|m| VmValue::String(Rc::from(m.as_str())))
                .collect();
            if matches.is_empty() {
                return Ok(VmValue::Nil);
            }
            return Ok(VmValue::List(Rc::new(matches)));
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("regex_replace", |args, _out| {
        if args.len() >= 3 {
            let pattern = args[0].display();
            let replacement = args[1].display();
            let text = args[2].display();
            let re = get_cached_regex(&pattern)?;
            return Ok(VmValue::String(Rc::from(
                re.replace_all(&text, replacement.as_str()).into_owned(),
            )));
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("regex_captures", |args, _out| {
        if args.len() < 2 {
            return Ok(VmValue::List(Rc::new(Vec::new())));
        }
        let pattern = args[0].display();
        let text = args[1].display();
        let re = get_cached_regex(&pattern)?;

        let mut results: Vec<VmValue> = Vec::new();
        for caps in re.captures_iter(&text) {
            let mut dict = BTreeMap::new();

            dict.insert(
                "match".to_string(),
                VmValue::String(Rc::from(caps.get(0).map_or("", |m| m.as_str()))),
            );

            let groups: Vec<VmValue> = (1..caps.len())
                .map(|i| match caps.get(i) {
                    Some(m) => VmValue::String(Rc::from(m.as_str())),
                    None => VmValue::Nil,
                })
                .collect();
            dict.insert("groups".to_string(), VmValue::List(Rc::new(groups)));

            for name in re.capture_names().flatten() {
                if let Some(m) = caps.name(name) {
                    dict.insert(name.to_string(), VmValue::String(Rc::from(m.as_str())));
                }
            }

            results.push(VmValue::Dict(Rc::new(dict)));
        }
        Ok(VmValue::List(Rc::new(results)))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::Vm;
    use std::rc::Rc;

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
        VmValue::String(Rc::from(v))
    }

    fn unwrap_list(v: &VmValue) -> &Vec<VmValue> {
        match v {
            VmValue::List(l) => l,
            _ => panic!("expected List, got {:?}", v.display()),
        }
    }

    // ---- regex_match ----

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
        assert_eq!(list.len(), 4); // matches before a, b, c, and end
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

    // ---- regex_replace ----

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
    fn replace_missing_args_returns_nil() {
        let mut vm = vm();
        let result = call(&mut vm, "regex_replace", vec![s(r"\d+"), s("X")]).unwrap();
        assert!(matches!(result, VmValue::Nil));
    }

    // ---- regex_captures ----

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

    // ---- cache ----

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
            let _ = get_cached_regex(&pattern);
        }
        let re = get_cached_regex("pat0").unwrap();
        assert!(re.is_match("pat0"));
    }
}
