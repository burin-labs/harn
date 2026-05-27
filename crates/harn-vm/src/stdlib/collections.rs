use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::rc::Rc;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
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

/// Coerce a discriminator value (returned by a `group_by` / `count_by`
/// callback) into the canonical dict-key string. Strict-string only:
/// dict keys are intrinsically `String`, and coercing other types via
/// `display()` silently merged collidable buckets (`Int(1)` and
/// `String("1")` both rendered `"1"`). Callers explicitly project to
/// string via `to_string(...)` so the discriminator is unambiguous at
/// the call site.
///
/// Exposed at crate scope so the method-call path in
/// `vm/methods/list.rs` enforces the same contract as the free
/// builtin path here.
pub(crate) fn string_discriminator(value: &VmValue, builtin: &str) -> Result<String, VmError> {
    match value {
        VmValue::String(s) => Ok((**s).to_string()),
        VmValue::Nil => Err(VmError::TypeError(format!(
            "{builtin}: callback returned nil; expected a string discriminator (wrap with to_string(...) if you intended a scalar)"
        ))),
        other => Err(VmError::TypeError(format!(
            "{builtin}: callback must return a string discriminator, got {}; wrap with to_string(...) so the bucket key is unambiguous",
            other.type_name()
        ))),
    }
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
            let key = vm.call_callable_one(callable, item).await?;
            let bucket = string_discriminator(&key, "group_by")?;
            groups.entry(bucket).or_default().push(item.clone());
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
            let result = vm.call_callable_one(callable, item).await?;
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
            let key = vm.call_callable_one(callable, item).await?;
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
            match vm.call_callable_one(callable, item).await? {
                VmValue::List(inner) => out.extend(inner.iter().cloned()),
                other => out.push(other),
            }
        }
        Ok(VmValue::List(Rc::new(out)))
    });

    vm.register_async_builtin("take_while", |args| async move {
        let items = list_arg(&args, "take_while")?;
        let callable = args
            .get(1)
            .ok_or_else(|| VmError::Runtime("take_while: callback is required".to_string()))?;
        if !Vm::is_callable_value(callable) {
            return Err(VmError::TypeError(format!(
                "take_while: callback must be callable, got {}",
                callable.type_name()
            )));
        }
        let mut vm = current_async_vm("take_while")?;
        let mut out = Vec::new();
        for item in items.iter() {
            let result = vm.call_callable_one(callable, item).await?;
            if !result.is_truthy() {
                break;
            }
            out.push(item.clone());
        }
        Ok(VmValue::List(Rc::new(out)))
    });

    vm.register_async_builtin("drop_while", |args| async move {
        let items = list_arg(&args, "drop_while")?;
        let callable = args
            .get(1)
            .ok_or_else(|| VmError::Runtime("drop_while: callback is required".to_string()))?;
        if !Vm::is_callable_value(callable) {
            return Err(VmError::TypeError(format!(
                "drop_while: callback must be callable, got {}",
                callable.type_name()
            )));
        }
        let mut vm = current_async_vm("drop_while")?;
        let mut out = Vec::new();
        let mut dropping = true;
        for item in items.iter() {
            if dropping {
                let result = vm.call_callable_one(callable, item).await?;
                if result.is_truthy() {
                    continue;
                }
                dropping = false;
            }
            out.push(item.clone());
        }
        Ok(VmValue::List(Rc::new(out)))
    });

    vm.register_async_builtin("count_by", |args| async move {
        let items = list_arg(&args, "count_by")?;
        let callable = args
            .get(1)
            .ok_or_else(|| VmError::Runtime("count_by: callback is required".to_string()))?;
        if !Vm::is_callable_value(callable) {
            return Err(VmError::TypeError(format!(
                "count_by: callback must be callable, got {}",
                callable.type_name()
            )));
        }
        let mut vm = current_async_vm("count_by")?;
        let mut counts: BTreeMap<String, i64> = BTreeMap::new();
        for item in items.iter() {
            let key = vm.call_callable_one(callable, item).await?;
            let bucket = string_discriminator(&key, "count_by")?;
            *counts.entry(bucket).or_insert(0) += 1;
        }
        Ok(VmValue::Dict(Rc::new(
            counts
                .into_iter()
                .map(|(key, count)| (key, VmValue::Int(count)))
                .collect(),
        )))
    });

    register_dict_builder_builtins(vm);
}

/// Registers the native fast-paths for the `std/collections` and `std/json`
/// option-builder helpers. The `std/*.harn` modules used to express these
/// in pure Harn with `result + {[k]: v}` accumulators, paying a fresh
/// `BTreeMap` allocation per inserted entry. The native paths cut every
/// helper to one allocation and skip the per-entry callback dispatch
/// `filter_nil`'s `.filter(closure)` form previously paid.
///
/// Implementations are individual `#[harn_builtin]`-annotated functions
/// below; `register_dict_builder_builtins` walks the collected
/// [`DICT_BUILDER_BUILTINS`] slice.
fn register_dict_builder_builtins(vm: &mut Vm) {
    for def in DICT_BUILDER_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(sig = "__dict_filter_nil(d: dict) -> dict", category = "collections")]
fn dict_filter_nil_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    dict_filter_nil(args.first().unwrap_or(&VmValue::Nil))
}

#[harn_builtin(
    sig = "__dict_merge(a: dict, b: dict) -> dict",
    category = "collections"
)]
fn dict_merge_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    dict_merge(
        args.first().unwrap_or(&VmValue::Nil),
        args.get(1).unwrap_or(&VmValue::Nil),
    )
}

#[harn_builtin(
    sig = "__dict_pick(d: dict, keys: list) -> dict",
    category = "collections"
)]
fn dict_pick_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    dict_pick(
        args.first().unwrap_or(&VmValue::Nil),
        args.get(1).unwrap_or(&VmValue::Nil),
    )
}

#[harn_builtin(
    sig = "__dict_pick_keys(d: dict, keys: list, drop_nil?: bool) -> dict",
    category = "collections"
)]
fn dict_pick_keys_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    dict_pick_keys(
        args.first().unwrap_or(&VmValue::Nil),
        args.get(1).unwrap_or(&VmValue::Nil),
        args.get(2).map(VmValue::is_truthy).unwrap_or(false),
    )
}

#[harn_builtin(
    sig = "__dict_omit(d: dict, keys: list) -> dict",
    category = "collections"
)]
fn dict_omit_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    dict_omit(
        args.first().unwrap_or(&VmValue::Nil),
        args.get(1).unwrap_or(&VmValue::Nil),
    )
}

#[harn_builtin(sig = "clone(value: any) -> any", category = "collections")]
fn clone_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(shallow_clone(args.first().unwrap_or(&VmValue::Nil)))
}

#[harn_builtin(sig = "deep_clone(value: any) -> any", category = "collections")]
fn deep_clone_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(deep_clone_value(args.first().unwrap_or(&VmValue::Nil)))
}

#[harn_builtin(
    sig = "deep_merge(a: dict, b: dict) -> dict",
    aliases = ["__deep_merge"],
    category = "collections"
)]
fn deep_merge_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    deep_merge_value(
        args.first().unwrap_or(&VmValue::Nil),
        args.get(1).unwrap_or(&VmValue::Nil),
    )
}

#[harn_builtin(
    sig = "unique(items: list) -> list",
    aliases = ["__list_unique"],
    category = "collections"
)]
fn unique_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    list_unique(args.first().unwrap_or(&VmValue::Nil))
}

#[harn_builtin(
    sig = "dict_from_pairs(pairs: list) -> dict",
    aliases = ["__dict_from_pairs"],
    category = "collections"
)]
fn dict_from_pairs_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    dict_from_pairs(args.first().unwrap_or(&VmValue::Nil))
}

const DICT_BUILDER_BUILTINS: &[&VmBuiltinDef] = &[
    &DICT_FILTER_NIL_IMPL_DEF,
    &DICT_MERGE_IMPL_DEF,
    &DICT_PICK_IMPL_DEF,
    &DICT_PICK_KEYS_IMPL_DEF,
    &DICT_OMIT_IMPL_DEF,
    &CLONE_IMPL_DEF,
    &DEEP_CLONE_IMPL_DEF,
    &DEEP_MERGE_IMPL_DEF,
    &UNIQUE_IMPL_DEF,
    &DICT_FROM_PAIRS_IMPL_DEF,
];

/// The macro-emitted builtins from this module that the top-level stdlib
/// aggregator pushes into `all_builtin_defs()`. Currently covers the
/// dict_builder family; the rest of `collections.rs` still uses inline
/// `vm.register_builtin` closures and will migrate in later passes.
pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = DICT_BUILDER_BUILTINS;

/// Returns a shallow copy of `value`. Dicts and lists become fresh
/// allocations independent of the source; primitives are returned by value.
/// Opaque handles (closures, channels, atomics, MCP clients, etc.) are
/// returned unchanged because copying them would either be a no-op
/// (Rc-cloned handle) or violate identity invariants.
fn shallow_clone(value: &VmValue) -> VmValue {
    match value {
        VmValue::Dict(d) => VmValue::Dict(Rc::new((**d).clone())),
        VmValue::List(items) => VmValue::List(Rc::new((**items).clone())),
        VmValue::Set(items) => VmValue::Set(Rc::new((**items).clone())),
        other => other.clone(),
    }
}

/// Returns a recursive deep copy of `value`. Dicts and lists are duplicated
/// and their entries are deep-cloned in turn. Handles and other opaque
/// runtime values are passed through unchanged (deep copying a channel or
/// MCP client is undefined and the runtime treats them as identities).
fn deep_clone_value(value: &VmValue) -> VmValue {
    match value {
        VmValue::Dict(d) => {
            let mut out = BTreeMap::new();
            for (key, val) in d.iter() {
                out.insert(key.clone(), deep_clone_value(val));
            }
            VmValue::Dict(Rc::new(out))
        }
        VmValue::List(items) => {
            VmValue::List(Rc::new(items.iter().map(deep_clone_value).collect()))
        }
        VmValue::Set(items) => VmValue::Set(Rc::new(items.iter().map(deep_clone_value).collect())),
        VmValue::Pair(p) => {
            VmValue::Pair(Rc::new((deep_clone_value(&p.0), deep_clone_value(&p.1))))
        }
        other => other.clone(),
    }
}

/// Recursively merges `b` into `a`, returning a fresh dict. When both
/// sides have a dict at the same key, the dicts are merged recursively.
/// Otherwise the right-hand value wins, matching the shallow `merge`
/// semantics for terminal nodes. Nil arguments are treated as empty
/// dicts so that variadic accumulators don't require a base case.
fn deep_merge_value(a: &VmValue, b: &VmValue) -> Result<VmValue, VmError> {
    let left = dict_arg(a, "deep_merge")?;
    let right = dict_arg(b, "deep_merge")?;
    if right.is_empty() {
        return Ok(VmValue::Dict(left));
    }
    if left.is_empty() {
        return Ok(VmValue::Dict(right));
    }
    let mut merged = Rc::try_unwrap(left).unwrap_or_else(|d| (*d).clone());
    let right_entries: Vec<(String, VmValue)> = match Rc::try_unwrap(right) {
        Ok(map) => map.into_iter().collect(),
        Err(rc) => rc.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
    };
    for (key, value) in right_entries {
        match merged.remove(&key) {
            Some(existing) => match (&existing, &value) {
                (VmValue::Dict(_), VmValue::Dict(_)) => {
                    let recursed = deep_merge_value(&existing, &value)?;
                    merged.insert(key, recursed);
                }
                _ => {
                    merged.insert(key, value);
                }
            },
            None => {
                merged.insert(key, value);
            }
        }
    }
    Ok(VmValue::Dict(Rc::new(merged)))
}

/// Returns a list with duplicate entries removed while preserving the
/// first-seen order. Structural equality is used (matching `==` in
/// scripts) via `value_structural_hash_key`, so two structurally equal
/// dicts collapse to a single entry.
fn list_unique(value: &VmValue) -> Result<VmValue, VmError> {
    let items = match value {
        VmValue::List(items) | VmValue::Set(items) => Rc::clone(items),
        VmValue::Nil => return Ok(VmValue::List(Rc::new(Vec::new()))),
        other => {
            return Err(VmError::TypeError(format!(
                "unique: expected a list, got {}",
                other.type_name()
            )));
        }
    };
    let mut seen: HashSet<String> = HashSet::with_capacity(items.len());
    let mut out: Vec<VmValue> = Vec::with_capacity(items.len());
    for item in items.iter() {
        let key = value_structural_hash_key(item);
        if seen.insert(key) {
            out.push(item.clone());
        }
    }
    Ok(VmValue::List(Rc::new(out)))
}

/// Converts a list of `[key, value]` pairs (or `pair(key, value)` values)
/// into a dict. The conversion is order-independent (BTreeMap), and
/// later pairs override earlier ones — matching the `__dict_merge`
/// right-wins convention.
fn dict_from_pairs(value: &VmValue) -> Result<VmValue, VmError> {
    let pairs = match value {
        VmValue::List(items) | VmValue::Set(items) => Rc::clone(items),
        VmValue::Nil => return Ok(VmValue::Dict(Rc::new(BTreeMap::new()))),
        other => {
            return Err(VmError::TypeError(format!(
                "dict_from_pairs: expected a list of [key, value] pairs, got {}",
                other.type_name()
            )));
        }
    };
    let mut out = BTreeMap::new();
    for (index, entry) in pairs.iter().enumerate() {
        let (key, val) = match entry {
            VmValue::Pair(p) => (p.0.clone(), p.1.clone()),
            VmValue::List(items) if items.len() == 2 => (items[0].clone(), items[1].clone()),
            other => {
                return Err(VmError::TypeError(format!(
                    "dict_from_pairs: entry {index} must be a [key, value] list or pair, got {}",
                    other.type_name()
                )));
            }
        };
        let key_string = match key {
            VmValue::String(s) => (*s).to_string(),
            VmValue::Int(n) => n.to_string(),
            VmValue::Bool(b) => b.to_string(),
            other => other.display(),
        };
        out.insert(key_string, val);
    }
    Ok(VmValue::Dict(Rc::new(out)))
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

    #[test]
    fn shallow_clone_decouples_dict_from_source() {
        let inner = VmValue::Dict(Rc::new(BTreeMap::new()));
        let source = dict(&[("inner", inner)]);
        let copy = shallow_clone(&source);
        match (&source, &copy) {
            (VmValue::Dict(a), VmValue::Dict(b)) => assert!(!Rc::ptr_eq(a, b)),
            _ => panic!("expected dicts"),
        }
        match (
            source.as_dict().unwrap().get("inner").unwrap(),
            copy.as_dict().unwrap().get("inner").unwrap(),
        ) {
            (VmValue::Dict(a), VmValue::Dict(b)) => {
                assert!(Rc::ptr_eq(a, b), "shallow clone shares inner Rc");
            }
            _ => panic!("expected nested dicts"),
        }
    }

    #[test]
    fn deep_clone_duplicates_nested_dicts_and_lists() {
        let inner_dict = dict(&[("k", VmValue::Int(1))]);
        let inner_list = VmValue::List(Rc::new(vec![VmValue::Int(1), VmValue::Int(2)]));
        let source = dict(&[("d", inner_dict), ("l", inner_list)]);
        let copy = deep_clone_value(&source);

        let copy_dict = copy.as_dict().unwrap();
        match copy_dict.get("d").unwrap() {
            VmValue::Dict(rc) => {
                let original_inner = source.as_dict().unwrap().get("d").unwrap();
                let VmValue::Dict(orig_rc) = original_inner else {
                    panic!()
                };
                assert!(!Rc::ptr_eq(rc, orig_rc));
            }
            _ => panic!("expected dict at d"),
        }
        match copy_dict.get("l").unwrap() {
            VmValue::List(items) => {
                let VmValue::List(orig_items) = source.as_dict().unwrap().get("l").unwrap() else {
                    panic!()
                };
                assert!(!Rc::ptr_eq(items, orig_items));
            }
            _ => panic!("expected list at l"),
        }
    }

    #[test]
    fn deep_merge_recurses_into_nested_dicts() {
        let a = dict(&[(
            "config",
            dict(&[("retries", VmValue::Int(3)), ("backoff", VmValue::Int(100))]),
        )]);
        let b = dict(&[(
            "config",
            dict(&[
                ("retries", VmValue::Int(5)),
                ("jitter", VmValue::Bool(true)),
            ]),
        )]);
        let merged = deep_merge_value(&a, &b).unwrap();
        let config = merged
            .as_dict()
            .and_then(|d| d.get("config"))
            .and_then(|v| v.as_dict())
            .expect("nested dict");
        assert_eq!(config.get("retries").and_then(VmValue::as_int), Some(5));
        assert_eq!(config.get("backoff").and_then(VmValue::as_int), Some(100));
        match config.get("jitter") {
            Some(VmValue::Bool(true)) => {}
            other => panic!("expected jitter=true, got {other:?}"),
        }
    }

    #[test]
    fn deep_merge_right_wins_when_types_differ() {
        let a = dict(&[("x", dict(&[("a", VmValue::Int(1))]))]);
        let b = dict(&[("x", VmValue::Int(99))]);
        let merged = deep_merge_value(&a, &b).unwrap();
        assert_eq!(
            merged
                .as_dict()
                .and_then(|d| d.get("x"))
                .and_then(VmValue::as_int),
            Some(99)
        );
    }

    #[test]
    fn list_unique_preserves_first_seen_order() {
        let input = VmValue::List(Rc::new(vec![
            VmValue::Int(1),
            VmValue::Int(2),
            VmValue::Int(1),
            VmValue::Int(3),
            VmValue::Int(2),
        ]));
        let result = list_unique(&input).unwrap();
        let items = match &result {
            VmValue::List(items) => items.clone(),
            _ => panic!("expected list"),
        };
        let ints: Vec<i64> = items.iter().filter_map(VmValue::as_int).collect();
        assert_eq!(ints, vec![1, 2, 3]);
    }

    #[test]
    fn dict_from_pairs_accepts_two_element_lists() {
        let pairs = VmValue::List(Rc::new(vec![
            VmValue::List(Rc::new(vec![
                VmValue::String(Rc::from("a")),
                VmValue::Int(1),
            ])),
            VmValue::List(Rc::new(vec![
                VmValue::String(Rc::from("b")),
                VmValue::Int(2),
            ])),
        ]));
        let result = dict_from_pairs(&pairs).unwrap();
        let d = result.as_dict().unwrap();
        assert_eq!(d.get("a").and_then(VmValue::as_int), Some(1));
        assert_eq!(d.get("b").and_then(VmValue::as_int), Some(2));
    }
}
