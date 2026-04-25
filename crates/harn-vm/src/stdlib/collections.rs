use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use crate::value::{value_structural_hash_key, VmError, VmValue};
use crate::vm::Vm;

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
}
