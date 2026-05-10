use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::rc::Rc;

use crate::value::{value_structural_hash_key, VmError, VmValue};
use crate::vm::Vm;

fn dict_arg(value: &VmValue, builtin: &str) -> Result<Rc<BTreeMap<String, VmValue>>, VmError> {
    match value {
        VmValue::Dict(d) => Ok(Rc::clone(d)),
        VmValue::Nil => Ok(Rc::new(BTreeMap::new())),
        other => Err(VmError::TypeError(format!(
            "{builtin}: expected dict, got {}",
            other.type_name()
        ))),
    }
}

fn keep_filter_nil(value: &VmValue) -> bool {
    match value {
        VmValue::Nil => false,
        VmValue::String(s) => !s.is_empty() && s.as_ref() != "null",
        _ => true,
    }
}

fn key_list_arg<'a>(value: &'a VmValue, builtin: &str) -> Result<&'a [VmValue], VmError> {
    match value {
        VmValue::List(items) | VmValue::Set(items) => Ok(items.as_slice()),
        other => Err(VmError::TypeError(format!(
            "{builtin}: keys argument must be a list or set, got {}",
            other.type_name()
        ))),
    }
}

fn current_async_vm(builtin: &str) -> Result<Vm, VmError> {
    crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
        VmError::Runtime(format!("{builtin}: builtin requires VM execution context"))
    })
}

fn list_arg<'a>(args: &'a [VmValue], builtin: &str) -> Result<&'a Rc<Vec<VmValue>>, VmError> {
    match args.first() {
        Some(VmValue::List(items)) => Ok(items),
        Some(other) => Err(VmError::TypeError(format!(
            "{builtin}: first argument must be a list, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!(
            "{builtin}: first argument must be a list"
        ))),
    }
}

fn positive_usize_arg(args: &[VmValue], index: usize, default: usize, _builtin: &str) -> usize {
    args.get(index)
        .and_then(VmValue::as_int)
        .unwrap_or(default as i64)
        .max(1) as usize
}

pub(crate) fn register_collection_builtins(vm: &mut Vm) {
    vm.register_async_builtin("chunk", |args| async move {
        let items = list_arg(&args, "chunk")?;
        let size = positive_usize_arg(&args, 1, 1, "chunk");
        Ok(VmValue::List(Rc::new(
            items
                .chunks(size)
                .map(|chunk| VmValue::List(Rc::new(chunk.to_vec())))
                .collect(),
        )))
    });

    vm.register_async_builtin("window", |args| async move {
        let items = list_arg(&args, "window")?;
        let size = positive_usize_arg(&args, 1, 2, "window");
        let step = positive_usize_arg(&args, 2, 1, "window");
        if size > items.len() {
            return Ok(VmValue::List(Rc::new(Vec::new())));
        }
        let mut windows = Vec::new();
        let mut start = 0;
        while start + size <= items.len() {
            windows.push(VmValue::List(Rc::new(items[start..start + size].to_vec())));
            start += step;
        }
        Ok(VmValue::List(Rc::new(windows)))
    });

    vm.register_async_builtin("group_by", |args| async move {
        let items = list_arg(&args, "group_by")?;
        let callable = args
            .get(1)
            .ok_or_else(|| VmError::Runtime("group_by: callback is required".to_string()))?;
        if !Vm::is_callable_value(callable) {
            return Err(VmError::TypeError(format!(
                "group_by: callback must be callable, got {}",
                callable.type_name()
            )));
        }
        let mut vm = current_async_vm("group_by")?;
        let mut groups: BTreeMap<String, Vec<VmValue>> = BTreeMap::new();
        for item in items.iter() {
            let key = vm.call_callable_value(callable, &[item.clone()]).await?;
            groups.entry(key.display()).or_default().push(item.clone());
        }
        Ok(VmValue::Dict(Rc::new(
            groups
                .into_iter()
                .map(|(key, values)| (key, VmValue::List(Rc::new(values))))
                .collect(),
        )))
    });

    vm.register_async_builtin("partition", |args| async move {
        let items = list_arg(&args, "partition")?;
        let callable = args
            .get(1)
            .ok_or_else(|| VmError::Runtime("partition: callback is required".to_string()))?;
        if !Vm::is_callable_value(callable) {
            return Err(VmError::TypeError(format!(
                "partition: callback must be callable, got {}",
                callable.type_name()
            )));
        }
        let mut vm = current_async_vm("partition")?;
        let mut matched = Vec::new();
        let mut no_match = Vec::new();
        for item in items.iter() {
            let result = vm.call_callable_value(callable, &[item.clone()]).await?;
            if result.is_truthy() {
                matched.push(item.clone());
            } else {
                no_match.push(item.clone());
            }
        }
        Ok(VmValue::Dict(Rc::new(BTreeMap::from([
            ("match".to_string(), VmValue::List(Rc::new(matched))),
            ("no_match".to_string(), VmValue::List(Rc::new(no_match))),
        ]))))
    });

    vm.register_async_builtin("dedup_by", |args| async move {
        let items = list_arg(&args, "dedup_by")?;
        let callable = args
            .get(1)
            .ok_or_else(|| VmError::Runtime("dedup_by: callback is required".to_string()))?;
        if !Vm::is_callable_value(callable) {
            return Err(VmError::TypeError(format!(
                "dedup_by: callback must be callable, got {}",
                callable.type_name()
            )));
        }
        let mut vm = current_async_vm("dedup_by")?;
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for item in items.iter() {
            let key = vm.call_callable_value(callable, &[item.clone()]).await?;
            if seen.insert(value_structural_hash_key(&key)) {
                out.push(item.clone());
            }
        }
        Ok(VmValue::List(Rc::new(out)))
    });

    vm.register_async_builtin("flat_map", |args| async move {
        let items = list_arg(&args, "flat_map")?;
        let callable = args
            .get(1)
            .ok_or_else(|| VmError::Runtime("flat_map: callback is required".to_string()))?;
        if !Vm::is_callable_value(callable) {
            return Err(VmError::TypeError(format!(
                "flat_map: callback must be callable, got {}",
                callable.type_name()
            )));
        }
        let mut vm = current_async_vm("flat_map")?;
        let mut out = Vec::new();
        for item in items.iter() {
            match vm.call_callable_value(callable, &[item.clone()]).await? {
                VmValue::List(inner) => out.extend(inner.iter().cloned()),
                other => out.push(other),
            }
        }
        Ok(VmValue::List(Rc::new(out)))
    });

    register_dict_builder_builtins(vm);
}

/// Registers the native fast-paths for the `std/collections` and `std/json`
/// option-builder helpers. The `std/*.harn` modules used to express these
/// in pure Harn with `result + {[k]: v}` accumulators, paying a fresh
/// `BTreeMap` allocation per inserted entry. The native paths cut every
/// helper to one allocation and skip the per-entry callback dispatch
/// `filter_nil`'s `.filter(closure)` form previously paid.
fn register_dict_builder_builtins(vm: &mut Vm) {
    vm.register_builtin("__dict_filter_nil", |args, _out| {
        dict_filter_nil(args.first().unwrap_or(&VmValue::Nil))
    });
    vm.register_builtin("__dict_merge", |args, _out| {
        dict_merge(
            args.first().unwrap_or(&VmValue::Nil),
            args.get(1).unwrap_or(&VmValue::Nil),
        )
    });
    vm.register_builtin("__dict_pick", |args, _out| {
        dict_pick(
            args.first().unwrap_or(&VmValue::Nil),
            args.get(1).unwrap_or(&VmValue::Nil),
        )
    });
    vm.register_builtin("__dict_pick_keys", |args, _out| {
        dict_pick_keys(
            args.first().unwrap_or(&VmValue::Nil),
            args.get(1).unwrap_or(&VmValue::Nil),
            args.get(2).map(VmValue::is_truthy).unwrap_or(false),
        )
    });
    vm.register_builtin("__dict_omit", |args, _out| {
        dict_omit(
            args.first().unwrap_or(&VmValue::Nil),
            args.get(1).unwrap_or(&VmValue::Nil),
        )
    });
}

fn dict_filter_nil(value: &VmValue) -> Result<VmValue, VmError> {
    let dict = dict_arg(value, "filter_nil")?;
    if dict.is_empty() || dict.values().all(keep_filter_nil) {
        return Ok(VmValue::Dict(dict));
    }
    let mut out = Rc::try_unwrap(dict).unwrap_or_else(|d| (*d).clone());
    out.retain(|_, value| keep_filter_nil(value));
    Ok(VmValue::Dict(Rc::new(out)))
}

fn dict_merge(a: &VmValue, b: &VmValue) -> Result<VmValue, VmError> {
    let left = dict_arg(a, "merge")?;
    let right = dict_arg(b, "merge")?;
    if right.is_empty() {
        return Ok(VmValue::Dict(left));
    }
    if left.is_empty() {
        return Ok(VmValue::Dict(right));
    }
    let mut merged = Rc::try_unwrap(left).unwrap_or_else(|d| (*d).clone());
    match Rc::try_unwrap(right) {
        Ok(entries) => merged.extend(entries),
        Err(entries) => merged.extend(entries.iter().map(|(k, v)| (k.clone(), v.clone()))),
    }
    Ok(VmValue::Dict(Rc::new(merged)))
}

fn dict_pick(data: &VmValue, keys: &VmValue) -> Result<VmValue, VmError> {
    let dict = dict_arg(data, "pick")?;
    let keys = key_list_arg(keys, "pick")?;
    let mut out = BTreeMap::new();
    for key in keys {
        let key = key.display();
        if let Some(value) = dict.get(&key) {
            if !matches!(value, VmValue::Nil) {
                out.insert(key, value.clone());
            }
        }
    }
    Ok(VmValue::Dict(Rc::new(out)))
}

fn dict_pick_keys(data: &VmValue, keys: &VmValue, drop_nil: bool) -> Result<VmValue, VmError> {
    let dict = dict_arg(data, "pick_keys")?;
    let keys = key_list_arg(keys, "pick_keys")?;
    let mut out = BTreeMap::new();
    for key in keys {
        let key = key.display();
        if let Some(value) = dict.get(&key) {
            if drop_nil && matches!(value, VmValue::Nil) {
                continue;
            }
            out.insert(key, value.clone());
        }
    }
    Ok(VmValue::Dict(Rc::new(out)))
}

fn dict_omit(data: &VmValue, keys: &VmValue) -> Result<VmValue, VmError> {
    let dict = dict_arg(data, "omit")?;
    let exclude: BTreeSet<String> = key_list_arg(keys, "omit")?
        .iter()
        .map(VmValue::display)
        .collect();
    if exclude.is_empty() || dict.keys().all(|k| !exclude.contains(k)) {
        return Ok(VmValue::Dict(dict));
    }
    let mut out = Rc::try_unwrap(dict).unwrap_or_else(|d| (*d).clone());
    out.retain(|key, _| !exclude.contains(key));
    Ok(VmValue::Dict(Rc::new(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(entries: &[(&str, VmValue)]) -> VmValue {
        let mut map = BTreeMap::new();
        for (k, v) in entries {
            map.insert((*k).to_string(), v.clone());
        }
        VmValue::Dict(Rc::new(map))
    }

    fn keys(items: &[&str]) -> VmValue {
        VmValue::List(Rc::new(
            items
                .iter()
                .map(|k| VmValue::String(Rc::from(*k)))
                .collect(),
        ))
    }

    #[test]
    fn filter_nil_drops_nil_empty_and_null_strings() {
        let input = dict(&[
            ("keep", VmValue::Int(1)),
            ("nil_value", VmValue::Nil),
            ("empty", VmValue::String(Rc::from(""))),
            ("null_string", VmValue::String(Rc::from("null"))),
            ("kept_zero", VmValue::Int(0)),
        ]);
        let result = dict_filter_nil(&input).unwrap();
        let dict = result.as_dict().expect("dict result");
        assert_eq!(dict.len(), 2);
        assert!(dict.contains_key("keep"));
        assert!(dict.contains_key("kept_zero"));
    }

    #[test]
    fn dict_merge_overrides_left_with_right() {
        let a = dict(&[("a", VmValue::Int(1)), ("b", VmValue::Int(2))]);
        let b = dict(&[("b", VmValue::Int(3)), ("c", VmValue::Int(4))]);
        let result = dict_merge(&a, &b).unwrap();
        let merged = result.as_dict().expect("dict result");
        assert_eq!(merged.get("a").and_then(VmValue::as_int), Some(1));
        assert_eq!(merged.get("b").and_then(VmValue::as_int), Some(3));
        assert_eq!(merged.get("c").and_then(VmValue::as_int), Some(4));
    }

    #[test]
    fn dict_merge_treats_nil_argument_as_empty_dict() {
        let a = dict(&[("a", VmValue::Int(1))]);
        let result = dict_merge(&a, &VmValue::Nil).unwrap();
        let merged = result.as_dict().expect("dict result");
        assert_eq!(merged.len(), 1);
        assert_eq!(merged.get("a").and_then(VmValue::as_int), Some(1));
    }

    #[test]
    fn dict_pick_drops_missing_and_nil_values() {
        let data = dict(&[
            ("a", VmValue::Int(1)),
            ("b", VmValue::Nil),
            ("c", VmValue::Int(3)),
        ]);
        let result = dict_pick(&data, &keys(&["a", "b", "missing"])).unwrap();
        let picked = result.as_dict().expect("dict result");
        assert_eq!(picked.len(), 1);
        assert_eq!(picked.get("a").and_then(VmValue::as_int), Some(1));
    }

    #[test]
    fn dict_pick_keys_respects_drop_nil_flag() {
        let data = dict(&[
            ("a", VmValue::Int(1)),
            ("b", VmValue::Nil),
            ("c", VmValue::Int(3)),
        ]);
        let kept = dict_pick_keys(&data, &keys(&["a", "b"]), false).unwrap();
        assert_eq!(kept.as_dict().expect("dict").len(), 2);

        let dropped = dict_pick_keys(&data, &keys(&["a", "b"]), true).unwrap();
        let dropped = dropped.as_dict().expect("dict");
        assert_eq!(dropped.len(), 1);
        assert!(dropped.contains_key("a"));
    }

    #[test]
    fn dict_omit_excludes_listed_keys() {
        let data = dict(&[
            ("a", VmValue::Int(1)),
            ("b", VmValue::Int(2)),
            ("c", VmValue::Int(3)),
        ]);
        let result = dict_omit(&data, &keys(&["a", "c"])).unwrap();
        let kept = result.as_dict().expect("dict result");
        assert_eq!(kept.len(), 1);
        assert!(kept.contains_key("b"));
    }
}
