//! Iterator and stream builtins.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::stdlib::macros::{harn_builtin, BuiltinSignature, Param, VmBuiltinDef, TY_ANY, TY_LIST};
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
        VmValue::dict(
            std::iter::once((
                "_namespace".to_string(),
                VmValue::String(arcstr::ArcStr::from("stream")),
            ))
            .chain(names.into_iter().map(|name| {
                (
                    name.to_string(),
                    VmValue::BuiltinRef(arcstr::ArcStr::from(format!("stream.{name}"))),
                )
            }))
            .collect::<BTreeMap<_, _>>(),
        ),
    );
}

pub(crate) fn register_iter_builtins(vm: &mut Vm) {
    register_stream_namespace(vm);
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

// `iter` parser signature: union of list/dict/string/set/range/iter/Generator/stream/channel.
// We express that as a hand-built signature via `sig_expr` so the `iter`, `Generator`, etc.
// named types reach the parser as `Ty::Named("iter")`, matching the original entry.
#[harn_builtin(
    sig = "iter(value: list | dict | string | set | range | iter | Generator | stream | channel) -> iter",
    category = "iter"
)]
fn iter_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let v = args
        .first()
        .cloned()
        .ok_or_else(|| VmError::TypeError("iter: expected 1 argument".to_string()))?;
    iter_from_value(v)
}

#[harn_builtin(sig = "pair(...args: any) -> pair", category = "iter")]
fn pair_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() != 2 {
        return Err(VmError::TypeError(format!(
            "pair: expected 2 arguments, got {}",
            args.len()
        )));
    }
    Ok(VmValue::Pair(std::sync::Arc::new((
        args[0].clone(),
        args[1].clone(),
    ))))
}

// stream.* builtins use dotted names that the sig-string grammar can't tokenize,
// so we build their `BuiltinSignature` literals directly with `sig_expr`.

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("stream.map", &[Param::new("args", TY_ANY)], TY_ANY),
    category = "iter"
)]
fn stream_map_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let inner = iter_handle_from_value(require_arg(args, 0, "stream.map")?)?;
    let f = require_callable(args, 1, "stream.map")?;
    Ok(VmValue::Iter(Arc::new(Mutex::new(VmIter::Map {
        inner,
        f,
    }))))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("stream.filter", &[Param::new("args", TY_ANY)], TY_ANY),
    category = "iter"
)]
fn stream_filter_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let inner = iter_handle_from_value(require_arg(args, 0, "stream.filter")?)?;
    let p = require_callable(args, 1, "stream.filter")?;
    Ok(VmValue::Iter(Arc::new(Mutex::new(VmIter::Filter {
        inner,
        p,
    }))))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("stream.tap", &[Param::new("args", TY_ANY)], TY_ANY),
    category = "iter"
)]
fn stream_tap_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let inner = iter_handle_from_value(require_arg(args, 0, "stream.tap")?)?;
    let f = require_callable(args, 1, "stream.tap")?;
    Ok(VmValue::Iter(Arc::new(Mutex::new(VmIter::Tap {
        inner,
        f,
    }))))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("stream.scan", &[Param::new("args", TY_ANY)], TY_ANY),
    category = "iter"
)]
fn stream_scan_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let inner = iter_handle_from_value(require_arg(args, 0, "stream.scan")?)?;
    let acc = require_arg(args, 1, "stream.scan")?;
    let f = require_callable(args, 2, "stream.scan")?;
    Ok(VmValue::Iter(Arc::new(Mutex::new(VmIter::Scan {
        inner,
        acc,
        f,
    }))))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("stream.take", &[Param::new("args", TY_ANY)], TY_ANY),
    category = "iter"
)]
fn stream_take_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let inner = iter_handle_from_value(require_arg(args, 0, "stream.take")?)?;
    let remaining = require_non_negative_usize(args, 1, "stream.take")?;
    Ok(VmValue::Iter(Arc::new(Mutex::new(VmIter::Take {
        inner,
        remaining,
    }))))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("stream.take_until", &[Param::new("args", TY_ANY)], TY_ANY),
    category = "iter"
)]
fn stream_take_until_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let inner = iter_handle_from_value(require_arg(args, 0, "stream.take_until")?)?;
    let p = require_callable(args, 1, "stream.take_until")?;
    Ok(VmValue::Iter(Arc::new(Mutex::new(VmIter::TakeUntil {
        inner,
        p,
    }))))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("stream.merge", &[Param::new("args", TY_ANY)], TY_ANY),
    category = "iter"
)]
fn stream_merge_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
    Ok(VmValue::Iter(Arc::new(Mutex::new(VmIter::Merge {
        sources,
        cursor: 0,
    }))))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("stream.interleave", &[Param::new("args", TY_ANY)], TY_ANY),
    category = "iter"
)]
fn stream_interleave_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
    Ok(VmValue::Iter(Arc::new(Mutex::new(VmIter::Interleave {
        sources,
        cursor: 0,
    }))))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("stream.zip", &[Param::new("args", TY_ANY)], TY_ANY),
    category = "iter"
)]
fn stream_zip_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() != 2 {
        return Err(type_error(format!(
            "stream.zip: expected 2 streams, got {}",
            args.len()
        )));
    }
    let a = iter_handle_from_value(args[0].clone())?;
    let b = iter_handle_from_value(args[1].clone())?;
    Ok(VmValue::Iter(Arc::new(Mutex::new(VmIter::Zip { a, b }))))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("stream.broadcast", &[Param::new("args", TY_ANY)], TY_LIST),
    category = "iter"
)]
fn stream_broadcast_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let source = iter_handle_from_value(require_arg(args, 0, "stream.broadcast")?)?;
    let n = require_positive_usize(args, 1, "stream.broadcast")?;
    Ok(VmValue::List(std::sync::Arc::new(broadcast_branches(
        source, n,
    ))))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("stream.race", &[Param::new("args", TY_ANY)], TY_ANY),
    category = "iter"
)]
fn stream_race_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
    Ok(VmValue::Iter(Arc::new(Mutex::new(VmIter::Race {
        sources,
        winner: None,
    }))))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("stream.throttle", &[Param::new("args", TY_ANY)], TY_ANY),
    category = "iter"
)]
fn stream_throttle_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let inner = iter_handle_from_value(require_arg(args, 0, "stream.throttle")?)?;
    let per_sec = require_positive_f64(args, 1, "stream.throttle")?;
    let interval_ms = (1000.0 / per_sec).ceil().max(1.0) as u64;
    Ok(VmValue::Iter(Arc::new(Mutex::new(VmIter::Throttle {
        inner,
        interval_ms,
        next_ready: None,
    }))))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("stream.debounce", &[Param::new("args", TY_ANY)], TY_ANY),
    category = "iter"
)]
fn stream_debounce_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let inner = iter_handle_from_value(require_arg(args, 0, "stream.debounce")?)?;
    let window_ms = require_non_negative_usize(args, 1, "stream.debounce")? as u64;
    Ok(VmValue::Iter(Arc::new(Mutex::new(VmIter::Debounce {
        inner,
        window_ms,
    }))))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("stream.collect", &[Param::new("args", TY_ANY)], TY_LIST),
    kind = "async",
    category = "iter"
)]
async fn stream_collect_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let inner = iter_handle_from_value(require_arg(&args, 0, "stream.collect")?)?;
    let max = collect_max_arg(&args)?;
    let mut vm = ctx.child_vm();
    Ok(VmValue::List(std::sync::Arc::new(
        drain_capped(&inner, &mut vm, max).await?,
    )))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("stream.fold", &[Param::new("args", TY_ANY)], TY_ANY),
    kind = "async",
    category = "iter"
)]
async fn stream_fold_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let inner = iter_handle_from_value(require_arg(&args, 0, "stream.fold")?)?;
    let mut acc = require_arg(&args, 1, "stream.fold")?;
    let f = require_callable(&args, 2, "stream.fold")?;
    let mut vm = ctx.child_vm();
    loop {
        match next_handle(&inner, &mut vm).await? {
            Some(v) => acc = vm.call_callable_two(&f, &acc, &v).await?,
            None => return Ok(acc),
        }
    }
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("stream.first", &[Param::new("args", TY_ANY)], TY_ANY),
    kind = "async",
    category = "iter"
)]
async fn stream_first_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let inner = iter_handle_from_value(require_arg(&args, 0, "stream.first")?)?;
    let mut vm = ctx.child_vm();
    Ok(next_handle(&inner, &mut vm).await?.unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    sig = "parallel_race(...args: any) -> any",
    kind = "async",
    category = "iter"
)]
async fn parallel_race_impl_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
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
    let parent_vm = ctx.child_vm();
    parallel_race_impl(parent_vm, items.iter().cloned().collect(), callable, cap).await
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
        match vm.call_callable_one(&callable, &item).await {
            Ok(VmValue::EnumVariant(enum_variant)) if enum_variant.is_variant("Result", "Ok") => {
                Ok(enum_variant.fields.first().cloned().unwrap_or(VmValue::Nil))
            }
            Ok(VmValue::EnumVariant(enum_variant)) if enum_variant.is_variant("Result", "Err") => {
                let mut message = String::new();
                enum_variant
                    .fields
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

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &ITER_IMPL_DEF,
    &PAIR_IMPL_DEF,
    &STREAM_MAP_IMPL_DEF,
    &STREAM_FILTER_IMPL_DEF,
    &STREAM_TAP_IMPL_DEF,
    &STREAM_SCAN_IMPL_DEF,
    &STREAM_TAKE_IMPL_DEF,
    &STREAM_TAKE_UNTIL_IMPL_DEF,
    &STREAM_MERGE_IMPL_DEF,
    &STREAM_INTERLEAVE_IMPL_DEF,
    &STREAM_ZIP_IMPL_DEF,
    &STREAM_BROADCAST_IMPL_DEF,
    &STREAM_RACE_IMPL_DEF,
    &STREAM_THROTTLE_IMPL_DEF,
    &STREAM_DEBOUNCE_IMPL_DEF,
    &STREAM_COLLECT_IMPL_DEF,
    &STREAM_FOLD_IMPL_DEF,
    &STREAM_FIRST_IMPL_DEF,
    &PARALLEL_RACE_IMPL_BUILTIN_DEF,
];
