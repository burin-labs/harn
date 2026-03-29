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
