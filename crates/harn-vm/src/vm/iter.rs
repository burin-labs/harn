//! Lazy iterator protocol for the Harn VM.
//!
//! `VmIter` is the backing enum for `VmValue::Iter`. It's a single-pass, fused
//! iterator; once `next` returns `None` the variant is replaced with
//! `Exhausted`. Step (a) only introduces source variants (Vec, Dict, Chars,
//! Gen, Chan, Exhausted) and wires them into the for-loop driver. Combinator
//! variants (`Map`, `Filter`, `Take`, ...) and sink builtins land in later
//! steps per the plan.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::chunk::CompiledFunction;
use crate::value::{VmChannelHandle, VmError, VmGenerator, VmValue};

/// Backing enum for `VmValue::Iter`. See module docs.
#[derive(Debug)]
pub enum VmIter {
    /// Snapshot over a shared list / set backing store.
    Vec { items: Rc<Vec<VmValue>>, idx: usize },
    /// Snapshot over a dict; yields one-key `{key, value}` dicts for now.
    /// Step (b) swaps these for `VmValue::Pair` when the Pair variant lands.
    Dict {
        entries: Rc<BTreeMap<String, VmValue>>,
        keys: Vec<String>,
        idx: usize,
    },
    /// Unicode scalar iteration over a string.
    Chars { s: Rc<str>, byte_idx: usize },
    /// Drains a generator's yield channel.
    Gen { gen: VmGenerator },
    /// Reads from a channel handle.
    Chan { handle: VmChannelHandle },
    /// Maps each item through a closure.
    Map {
        inner: Rc<RefCell<VmIter>>,
        f: VmValue,
    },
    /// Keeps only items for which the predicate is truthy.
    Filter {
        inner: Rc<RefCell<VmIter>>,
        p: VmValue,
    },
    /// Maps each item to an iterable and flattens one level.
    FlatMap {
        inner: Rc<RefCell<VmIter>>,
        f: VmValue,
        cur: Option<Rc<RefCell<VmIter>>>,
    },
    /// Terminal state: `next` always returns `None`.
    Exhausted,
}

impl VmIter {
    /// Produce the next value, or `None` when exhausted.
    ///
    /// Combinator variants (`Map`, `Filter`, `FlatMap`) invoke user-provided
    /// closures through the `vm` / `functions` parameters.
    pub fn next<'a>(
        &'a mut self,
        vm: &'a mut crate::vm::Vm,
        functions: &'a [CompiledFunction],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<VmValue>, VmError>> + 'a>>
    {
        Box::pin(async move { self.next_impl(vm, functions).await })
    }

    async fn next_impl(
        &mut self,
        vm: &mut crate::vm::Vm,
        functions: &[CompiledFunction],
    ) -> Result<Option<VmValue>, VmError> {
        match self {
            VmIter::Exhausted => Ok(None),
            VmIter::Vec { items, idx } => {
                if *idx < items.len() {
                    let v = items[*idx].clone();
                    *idx += 1;
                    Ok(Some(v))
                } else {
                    *self = VmIter::Exhausted;
                    Ok(None)
                }
            }
            VmIter::Dict { entries, keys, idx } => {
                if *idx < keys.len() {
                    let k = &keys[*idx];
                    let v = entries.get(k).cloned().unwrap_or(VmValue::Nil);
                    *idx += 1;
                    Ok(Some(VmValue::Pair(Rc::new((
                        VmValue::String(Rc::from(k.as_str())),
                        v,
                    )))))
                } else {
                    *self = VmIter::Exhausted;
                    Ok(None)
                }
            }
            VmIter::Chars { s, byte_idx } => {
                if *byte_idx >= s.len() {
                    *self = VmIter::Exhausted;
                    return Ok(None);
                }
                let rest = &s[*byte_idx..];
                if let Some(c) = rest.chars().next() {
                    *byte_idx += c.len_utf8();
                    Ok(Some(VmValue::String(Rc::from(c.to_string().as_str()))))
                } else {
                    *self = VmIter::Exhausted;
                    Ok(None)
                }
            }
            VmIter::Gen { gen } => {
                if gen.done.get() {
                    *self = VmIter::Exhausted;
                    return Ok(None);
                }
                let rx = gen.receiver.clone();
                let mut guard = rx.lock().await;
                match guard.recv().await {
                    Some(v) => Ok(Some(v)),
                    None => {
                        gen.done.set(true);
                        drop(guard);
                        *self = VmIter::Exhausted;
                        Ok(None)
                    }
                }
            }
            VmIter::Map { inner, f } => {
                let f = f.clone();
                let item = inner.borrow_mut().next(vm, functions).await?;
                match item {
                    None => {
                        *self = VmIter::Exhausted;
                        Ok(None)
                    }
                    Some(v) => {
                        let out = vm.call_callable_value(&f, &[v], functions).await?;
                        Ok(Some(out))
                    }
                }
            }
            VmIter::Filter { inner, p } => {
                let p = p.clone();
                loop {
                    let item = inner.borrow_mut().next(vm, functions).await?;
                    match item {
                        None => {
                            *self = VmIter::Exhausted;
                            return Ok(None);
                        }
                        Some(v) => {
                            let keep = vm.call_callable_value(&p, &[v.clone()], functions).await?;
                            if keep.is_truthy() {
                                return Ok(Some(v));
                            }
                        }
                    }
                }
            }
            VmIter::FlatMap { inner, f, cur } => {
                let f = f.clone();
                loop {
                    if let Some(cur_iter) = cur.clone() {
                        let item = cur_iter.borrow_mut().next(vm, functions).await?;
                        if let Some(v) = item {
                            return Ok(Some(v));
                        }
                        *cur = None;
                    }
                    let item = inner.borrow_mut().next(vm, functions).await?;
                    match item {
                        None => {
                            *self = VmIter::Exhausted;
                            return Ok(None);
                        }
                        Some(v) => {
                            let result = vm.call_callable_value(&f, &[v], functions).await?;
                            let lifted = iter_from_value(result)?;
                            if let VmValue::Iter(h) = lifted {
                                *cur = Some(h);
                            } else {
                                return Err(VmError::TypeError(
                                    "flat_map: expected iterable result".to_string(),
                                ));
                            }
                        }
                    }
                }
            }
            VmIter::Chan { handle } => {
                let is_closed = handle.closed.load(std::sync::atomic::Ordering::Relaxed);
                let rx = handle.receiver.clone();
                let mut guard = rx.lock().await;
                let item = if is_closed {
                    guard.try_recv().ok()
                } else {
                    guard.recv().await
                };
                match item {
                    Some(v) => Ok(Some(v)),
                    None => {
                        drop(guard);
                        *self = VmIter::Exhausted;
                        Ok(None)
                    }
                }
            }
        }
    }
}

/// Fully consume an iter handle into a Vec of values.
pub async fn drain(
    handle: &Rc<RefCell<VmIter>>,
    vm: &mut crate::vm::Vm,
    functions: &[CompiledFunction],
) -> Result<Vec<VmValue>, VmError> {
    let mut out = Vec::new();
    loop {
        let v = handle.borrow_mut().next(vm, functions).await?;
        match v {
            Some(v) => out.push(v),
            None => break,
        }
    }
    Ok(out)
}

/// Convenience: wrap a source value into a `VmValue::Iter`. Used by the
/// `iter()` builtin and by combinator/sink implementations in later steps.
pub fn iter_from_value(v: VmValue) -> Result<VmValue, VmError> {
    let inner = match v {
        VmValue::Iter(h) => return Ok(VmValue::Iter(h)),
        VmValue::List(items) => VmIter::Vec { items, idx: 0 },
        VmValue::Set(items) => VmIter::Vec { items, idx: 0 },
        VmValue::Dict(entries) => {
            let keys: Vec<String> = entries.keys().cloned().collect();
            VmIter::Dict {
                entries,
                keys,
                idx: 0,
            }
        }
        VmValue::String(s) => VmIter::Chars { s, byte_idx: 0 },
        VmValue::Generator(gen) => VmIter::Gen { gen },
        VmValue::Channel(handle) => VmIter::Chan { handle },
        other => {
            return Err(VmError::TypeError(format!(
                "iter: value of type {} is not iterable",
                other.type_name()
            )))
        }
    };
    Ok(VmValue::Iter(Rc::new(RefCell::new(inner))))
}
