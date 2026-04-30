//! Iterator and stream builtins.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::iter::{
    broadcast_branches, drain_capped, iter_from_value, iter_handle_from_value, next_handle, VmIter,
};
use crate::vm::Vm;

fn type_error(message: impl Into<String>) -> VmError {
    VmError::TypeError(message.into())
}

fn require_arg(args: &[VmValue], index: usize, builtin: &str) -> Result<VmValue, VmError> {
    args.get(index)
        .cloned()
        .ok_or_else(|| type_error(format!("{builtin}: missing argument {}", index + 1)))
}

fn require_callable(args: &[VmValue], index: usize, builtin: &str) -> Result<VmValue, VmError> {
    let callable = require_arg(args, index, builtin)?;
    if !Vm::is_callable_value(&callable) {
        return Err(type_error(format!(
            "{builtin}: argument {} must be callable, got {}",
            index + 1,
            callable.type_name()
        )));
    }
    Ok(callable)
}

fn require_non_negative_usize(
    args: &[VmValue],
    index: usize,
    builtin: &str,
) -> Result<usize, VmError> {
    match args.get(index) {
        Some(VmValue::Int(n)) if *n >= 0 => Ok(*n as usize),
        Some(other) => Err(type_error(format!(
            "{builtin}: argument {} must be a non-negative int, got {}",
            index + 1,
            other.type_name()
        ))),
        None => Err(type_error(format!(
            "{builtin}: missing argument {}",
            index + 1
        ))),
    }
}

fn require_positive_usize(args: &[VmValue], index: usize, builtin: &str) -> Result<usize, VmError> {
    match args.get(index) {
        Some(VmValue::Int(n)) if *n > 0 => Ok(*n as usize),
        Some(other) => Err(type_error(format!(
            "{builtin}: argument {} must be a positive int, got {}",
            index + 1,
            other.type_name()
        ))),
        None => Err(type_error(format!(
            "{builtin}: missing argument {}",
            index + 1
        ))),
    }
}

fn require_positive_f64(args: &[VmValue], index: usize, builtin: &str) -> Result<f64, VmError> {
    let value = match args.get(index) {
        Some(VmValue::Int(n)) => *n as f64,
        Some(VmValue::Float(n)) => *n,
        Some(other) => {
            return Err(type_error(format!(
                "{builtin}: argument {} must be a positive number, got {}",
                index + 1,
                other.type_name()
            )))
        }
        None => {
            return Err(type_error(format!(
                "{builtin}: missing argument {}",
                index + 1
            )))
        }
    };
    if value <= 0.0 || !value.is_finite() {
        return Err(type_error(format!(
            "{builtin}: argument {} must be a positive finite number",
            index + 1
        )));
    }
    Ok(value)
}

fn collect_max_arg(args: &[VmValue]) -> Result<usize, VmError> {
    const DEFAULT_MAX: usize = 10_000;
    match args.get(1) {
        None | Some(VmValue::Nil) => Ok(DEFAULT_MAX),
        Some(VmValue::Int(n)) if *n >= 0 => Ok(*n as usize),
        Some(VmValue::Dict(options)) => match options.get("max") {
            Some(VmValue::Int(n)) if *n >= 0 => Ok(*n as usize),
            Some(other) => Err(type_error(format!(
                "stream.collect: max must be a non-negative int, got {}",
                other.type_name()
            ))),
            None => Ok(DEFAULT_MAX),
        },
        Some(other) => Err(type_error(format!(
            "stream.collect: second argument must be max int or options dict, got {}",
            other.type_name()
        ))),
    }
}

fn register_stream_namespace(vm: &mut Vm) {
    let names = [
        "map",
        "filter",
        "tap",
        "scan",
        "fold",
        "collect",
        "take",
        "take_until",
        "first",
        "merge",
        "interleave",
        "zip",
        "broadcast",
        "race",
        "throttle",
        "debounce",
    ];
    vm.set_global(
        "stream",
        VmValue::Dict(Rc::new(
            std::iter::once((
                "_namespace".to_string(),
                VmValue::String(Rc::from("stream")),
            ))
            .chain(names.into_iter().map(|name| {
                (
                    name.to_string(),
                    VmValue::BuiltinRef(Rc::from(format!("stream.{name}"))),
                )
            }))
            .collect::<BTreeMap<_, _>>(),
        )),
    );
}

pub(crate) fn register_iter_builtins(vm: &mut Vm) {
    register_stream_namespace(vm);

    vm.register_builtin("iter", |args, _out| {
        let v = args
            .first()
            .cloned()
            .ok_or_else(|| VmError::TypeError("iter: expected 1 argument".to_string()))?;
        iter_from_value(v)
    });
    vm.register_builtin("pair", |args, _out| {
        if args.len() != 2 {
            return Err(VmError::TypeError(format!(
                "pair: expected 2 arguments, got {}",
                args.len()
            )));
        }
        Ok(VmValue::Pair(Rc::new((args[0].clone(), args[1].clone()))))
    });

    vm.register_builtin("stream.map", |args, _out| {
        let inner = iter_handle_from_value(require_arg(args, 0, "stream.map")?)?;
        let f = require_callable(args, 1, "stream.map")?;
        Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Map {
            inner,
            f,
        }))))
    });
    vm.register_builtin("stream.filter", |args, _out| {
        let inner = iter_handle_from_value(require_arg(args, 0, "stream.filter")?)?;
        let p = require_callable(args, 1, "stream.filter")?;
        Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Filter {
            inner,
            p,
        }))))
    });
    vm.register_builtin("stream.tap", |args, _out| {
        let inner = iter_handle_from_value(require_arg(args, 0, "stream.tap")?)?;
        let f = require_callable(args, 1, "stream.tap")?;
        Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Tap {
            inner,
            f,
        }))))
    });
    vm.register_builtin("stream.scan", |args, _out| {
        let inner = iter_handle_from_value(require_arg(args, 0, "stream.scan")?)?;
        let acc = require_arg(args, 1, "stream.scan")?;
        let f = require_callable(args, 2, "stream.scan")?;
        Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Scan {
            inner,
            acc,
            f,
        }))))
    });
    vm.register_builtin("stream.take", |args, _out| {
        let inner = iter_handle_from_value(require_arg(args, 0, "stream.take")?)?;
        let remaining = require_non_negative_usize(args, 1, "stream.take")?;
        Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Take {
            inner,
            remaining,
        }))))
    });
    vm.register_builtin("stream.take_until", |args, _out| {
        let inner = iter_handle_from_value(require_arg(args, 0, "stream.take_until")?)?;
        let p = require_callable(args, 1, "stream.take_until")?;
        Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::TakeUntil {
            inner,
            p,
        }))))
    });
    vm.register_builtin("stream.merge", |args, _out| {
        if args.is_empty() {
            return Err(type_error("stream.merge: expected at least one stream"));
        }
        let sources = args
            .iter()
            .cloned()
            .map(iter_handle_from_value)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(Some)
            .collect();
        Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Merge {
            sources,
            cursor: 0,
        }))))
    });
    vm.register_builtin("stream.interleave", |args, _out| {
        if args.len() < 2 {
            return Err(type_error(
                "stream.interleave: expected at least two streams",
            ));
        }
        let sources = args
            .iter()
            .cloned()
            .map(iter_handle_from_value)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(Some)
            .collect();
        Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Interleave {
            sources,
            cursor: 0,
        }))))
    });
    vm.register_builtin("stream.zip", |args, _out| {
        if args.len() != 2 {
            return Err(type_error(format!(
                "stream.zip: expected 2 streams, got {}",
                args.len()
            )));
        }
        let a = iter_handle_from_value(args[0].clone())?;
        let b = iter_handle_from_value(args[1].clone())?;
        Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Zip { a, b }))))
    });
    vm.register_builtin("stream.broadcast", |args, _out| {
        let source = iter_handle_from_value(require_arg(args, 0, "stream.broadcast")?)?;
        let n = require_positive_usize(args, 1, "stream.broadcast")?;
        Ok(VmValue::List(Rc::new(broadcast_branches(source, n))))
    });
    vm.register_builtin("stream.race", |args, _out| {
        if args.is_empty() {
            return Err(type_error("stream.race: expected at least one stream"));
        }
        let sources = args
            .iter()
            .cloned()
            .map(iter_handle_from_value)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(Some)
            .collect();
        Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Race {
            sources,
            winner: None,
        }))))
    });
    vm.register_builtin("stream.throttle", |args, _out| {
        let inner = iter_handle_from_value(require_arg(args, 0, "stream.throttle")?)?;
        let per_sec = require_positive_f64(args, 1, "stream.throttle")?;
        let interval_ms = (1000.0 / per_sec).ceil().max(1.0) as u64;
        Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Throttle {
            inner,
            interval_ms,
            next_ready: None,
        }))))
    });
    vm.register_builtin("stream.debounce", |args, _out| {
        let inner = iter_handle_from_value(require_arg(args, 0, "stream.debounce")?)?;
        let window_ms = require_non_negative_usize(args, 1, "stream.debounce")? as u64;
        Ok(VmValue::Iter(Rc::new(RefCell::new(VmIter::Debounce {
            inner,
            window_ms,
        }))))
    });

    vm.register_async_builtin("stream.collect", |args| async move {
        let inner = iter_handle_from_value(require_arg(&args, 0, "stream.collect")?)?;
        let max = collect_max_arg(&args)?;
        let mut vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
            VmError::Runtime("stream.collect: builtin requires VM execution context".to_string())
        })?;
        Ok(VmValue::List(Rc::new(
            drain_capped(&inner, &mut vm, max).await?,
        )))
    });
    vm.register_async_builtin("stream.fold", |args| async move {
        let inner = iter_handle_from_value(require_arg(&args, 0, "stream.fold")?)?;
        let mut acc = require_arg(&args, 1, "stream.fold")?;
        let f = require_callable(&args, 2, "stream.fold")?;
        let mut vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
            VmError::Runtime("stream.fold: builtin requires VM execution context".to_string())
        })?;
        loop {
            match next_handle(&inner, &mut vm).await? {
                Some(v) => acc = vm.call_callable_value(&f, &[acc, v]).await?,
                None => return Ok(acc),
            }
        }
    });
    vm.register_async_builtin("stream.first", |args| async move {
        let inner = iter_handle_from_value(require_arg(&args, 0, "stream.first")?)?;
        let mut vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
            VmError::Runtime("stream.first: builtin requires VM execution context".to_string())
        })?;
        Ok(next_handle(&inner, &mut vm).await?.unwrap_or(VmValue::Nil))
    });

    vm.register_async_builtin("parallel_race", |args| async move {
        let items = match require_arg(&args, 0, "parallel_race")? {
            VmValue::List(items) => items,
            other => {
                return Err(type_error(format!(
                    "parallel_race: first argument must be a list, got {}",
                    other.type_name()
                )))
            }
        };
        let callable = require_callable(&args, 1, "parallel_race")?;
        let cap = parallel_race_cap(args.get(2), items.len())?;
        let parent_vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
            VmError::Runtime("parallel_race: builtin requires VM execution context".to_string())
        })?;
        parallel_race_impl(parent_vm, items.iter().cloned().collect(), callable, cap).await
    });
}

fn parallel_race_cap(value: Option<&VmValue>, total: usize) -> Result<Option<usize>, VmError> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        VmValue::Nil => Ok(None),
        VmValue::Int(n) if *n <= 0 => Ok(None),
        VmValue::Int(n) => Ok(Some((*n as usize).min(total.max(1)))),
        VmValue::Dict(options) => match options.get("max_concurrent") {
            None | Some(VmValue::Nil) => Ok(None),
            Some(VmValue::Int(n)) if *n <= 0 => Ok(None),
            Some(VmValue::Int(n)) => Ok(Some((*n as usize).min(total.max(1)))),
            Some(other) => Err(type_error(format!(
                "parallel_race: max_concurrent must be an int, got {}",
                other.type_name()
            ))),
        },
        other => Err(type_error(format!(
            "parallel_race: third argument must be max_concurrent int or options dict, got {}",
            other.type_name()
        ))),
    }
}

async fn parallel_race_impl(
    parent_vm: Vm,
    items: Vec<VmValue>,
    callable: VmValue,
    cap: Option<usize>,
) -> Result<VmValue, VmError> {
    let total = items.len();
    if total == 0 {
        return Err(VmError::Runtime(
            "parallel_race: expected at least one item".to_string(),
        ));
    }

    let slot = cap.unwrap_or(total).max(1).min(total);
    let mut pending: VecDeque<VmValue> = items.into_iter().collect();
    let mut join_set: tokio::task::JoinSet<Result<VmValue, String>> = tokio::task::JoinSet::new();
    let mut failures = Vec::new();

    while join_set.len() < slot {
        let Some(item) = pending.pop_front() else {
            break;
        };
        spawn_parallel_race_task(&mut join_set, parent_vm.child_vm(), callable.clone(), item);
    }

    while let Some(joined) = join_set.join_next().await {
        match joined {
            Ok(Ok(value)) => {
                join_set.abort_all();
                return Ok(value);
            }
            Ok(Err(error)) => failures.push(error),
            Err(error) => failures.push(format!("task join failed: {error}")),
        }
        if let Some(item) = pending.pop_front() {
            spawn_parallel_race_task(&mut join_set, parent_vm.child_vm(), callable.clone(), item);
        }
    }

    Err(VmError::Runtime(format!(
        "parallel_race: all {} task(s) failed: {}",
        failures.len(),
        failures.join("; ")
    )))
}

fn spawn_parallel_race_task(
    join_set: &mut tokio::task::JoinSet<Result<VmValue, String>>,
    mut vm: Vm,
    callable: VmValue,
    item: VmValue,
) {
    join_set.spawn_local(async move {
        match vm.call_callable_value(&callable, &[item]).await {
            Ok(VmValue::EnumVariant {
                enum_name,
                variant,
                fields,
            }) if enum_name.as_ref() == "Result" && variant.as_ref() == "Ok" => {
                Ok(fields.first().cloned().unwrap_or(VmValue::Nil))
            }
            Ok(VmValue::EnumVariant {
                enum_name,
                variant,
                fields,
            }) if enum_name.as_ref() == "Result" && variant.as_ref() == "Err" => {
                let mut message = String::new();
                fields
                    .first()
                    .cloned()
                    .unwrap_or(VmValue::Nil)
                    .write_display(&mut message);
                Err(message)
            }
            Ok(value) => Ok(value),
            Err(error) => Err(error.to_string()),
        }
    });
}
