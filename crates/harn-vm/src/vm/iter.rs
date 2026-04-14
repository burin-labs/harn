//! Lazy iterator protocol for the Harn VM.
//!
//! `VmIter` is the backing enum for `VmValue::Iter`. It's a single-pass, fused
//! iterator; once `next` returns `None` the variant is replaced with
//! `Exhausted`. Step (a) only introduces source variants (Vec, Dict, Chars,
//! Gen, Chan, Exhausted) and wires them into the for-loop driver. Combinator
//! variants (`Map`, `Filter`, `Take`, ...) and sink builtins land in later
//! steps per the plan.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use crate::chunk::CompiledFunction;
use crate::value::{VmChannelHandle, VmError, VmGenerator, VmValue};

/// Backing enum for `VmValue::Iter`. See module docs.
#[derive(Debug)]
pub enum VmIter {
    /// Step through a lazy integer range without materializing.
    /// `next` is the value to emit on the next call; `stop` is the
    /// first value that terminates the iteration (one past the end).
    Range { next: i64, stop: i64 },
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
    /// Yields up to `remaining` items from `inner`, then becomes Exhausted.
    Take {
        inner: Rc<RefCell<VmIter>>,
        remaining: usize,
    },
    /// Skips the first `remaining` items from `inner` on the first call, then
    /// forwards. `remaining == 0` is the sentinel for "already primed".
    Skip {
        inner: Rc<RefCell<VmIter>>,
        remaining: usize,
    },
    /// Yields items from `inner` while the predicate is truthy; after the
    /// first falsy predicate or inner exhaustion, becomes Exhausted.
    TakeWhile {
        inner: Rc<RefCell<VmIter>>,
        p: VmValue,
        done: bool,
    },
    /// Discards items while the predicate is truthy; after the first falsy
    /// item, forwards that item and all subsequent items from `inner`.
    SkipWhile {
        inner: Rc<RefCell<VmIter>>,
        p: VmValue,
        primed: bool,
    },
    /// Advances two inner iters in lockstep; yields `Pair(a, b)` until either
    /// side is exhausted.
    Zip {
        a: Rc<RefCell<VmIter>>,
        b: Rc<RefCell<VmIter>>,
    },
    /// Yields `Pair(i, item)` starting at `i = 0`.
    Enumerate { inner: Rc<RefCell<VmIter>>, i: i64 },
    /// Concatenates two iters: drains `a` first, then `b`.
    Chain {
        a: Rc<RefCell<VmIter>>,
        b: Rc<RefCell<VmIter>>,
        on_a: bool,
    },
    /// Yields `VmValue::List` batches of up to `n` items from `inner`.
    /// The final batch may be shorter; empty input yields no batches.
    Chunks {
        inner: Rc<RefCell<VmIter>>,
        n: usize,
    },
    /// Yields sliding windows of exactly `n` items from `inner` as `VmValue::List`.
    /// If the input has fewer than `n` items total, no windows are yielded.
    Windows {
        inner: Rc<RefCell<VmIter>>,
        n: usize,
        buf: VecDeque<VmValue>,
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
            VmIter::Range { next, stop } => {
                if *next < *stop {
                    let v = *next;
                    *next += 1;
                    Ok(Some(VmValue::Int(v)))
                } else {
                    *self = VmIter::Exhausted;
                    Ok(None)
                }
            }
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
                let item = next_handle(inner, vm, functions).await?;
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
                    let item = next_handle(inner, vm, functions).await?;
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
                        let item = next_handle(&cur_iter, vm, functions).await?;
                        if let Some(v) = item {
                            return Ok(Some(v));
                        }
                        *cur = None;
                    }
                    let item = next_handle(inner, vm, functions).await?;
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
            VmIter::Take { inner, remaining } => {
                if *remaining == 0 {
                    *self = VmIter::Exhausted;
                    return Ok(None);
                }
                let item = next_handle(inner, vm, functions).await?;
                match item {
                    None => {
                        *self = VmIter::Exhausted;
                        Ok(None)
                    }
                    Some(v) => {
                        *remaining -= 1;
                        if *remaining == 0 {
                            *self = VmIter::Exhausted;
                        }
                        Ok(Some(v))
                    }
                }
            }
            VmIter::Skip { inner, remaining } => {
                while *remaining > 0 {
                    let item = next_handle(inner, vm, functions).await?;
                    match item {
                        None => {
                            *self = VmIter::Exhausted;
                            return Ok(None);
                        }
                        Some(_) => {
                            *remaining -= 1;
                        }
                    }
                }
                let item = next_handle(inner, vm, functions).await?;
                match item {
                    None => {
                        *self = VmIter::Exhausted;
                        Ok(None)
                    }
                    Some(v) => Ok(Some(v)),
                }
            }
            VmIter::TakeWhile { inner, p, done } => {
                if *done {
                    return Ok(None);
                }
                let p = p.clone();
                let item = next_handle(inner, vm, functions).await?;
                match item {
                    None => {
                        *self = VmIter::Exhausted;
                        Ok(None)
                    }
                    Some(v) => {
                        let keep = vm.call_callable_value(&p, &[v.clone()], functions).await?;
                        if keep.is_truthy() {
                            Ok(Some(v))
                        } else {
                            *self = VmIter::Exhausted;
                            Ok(None)
                        }
                    }
                }
            }
            VmIter::SkipWhile { inner, p, primed } => {
                if *primed {
                    let item = next_handle(inner, vm, functions).await?;
                    return match item {
                        None => {
                            *self = VmIter::Exhausted;
                            Ok(None)
                        }
                        Some(v) => Ok(Some(v)),
                    };
                }
                let p = p.clone();
                loop {
                    let item = next_handle(inner, vm, functions).await?;
                    match item {
                        None => {
                            *self = VmIter::Exhausted;
                            return Ok(None);
                        }
                        Some(v) => {
                            let drop_it =
                                vm.call_callable_value(&p, &[v.clone()], functions).await?;
                            if !drop_it.is_truthy() {
                                *primed = true;
                                return Ok(Some(v));
                            }
                        }
                    }
                }
            }
            VmIter::Zip { a, b } => {
                let ia = next_handle(a, vm, functions).await?;
                let x = match ia {
                    None => {
                        *self = VmIter::Exhausted;
                        return Ok(None);
                    }
                    Some(v) => v,
                };
                let ib = next_handle(b, vm, functions).await?;
                let y = match ib {
                    None => {
                        *self = VmIter::Exhausted;
                        return Ok(None);
                    }
                    Some(v) => v,
                };
                Ok(Some(VmValue::Pair(Rc::new((x, y)))))
            }
            VmIter::Enumerate { inner, i } => {
                let item = next_handle(inner, vm, functions).await?;
                match item {
                    None => {
                        *self = VmIter::Exhausted;
                        Ok(None)
                    }
                    Some(v) => {
                        let idx = *i;
                        *i += 1;
                        Ok(Some(VmValue::Pair(Rc::new((VmValue::Int(idx), v)))))
                    }
                }
            }
            VmIter::Chain { a, b, on_a } => {
                if *on_a {
                    let item = next_handle(a, vm, functions).await?;
                    if let Some(v) = item {
                        return Ok(Some(v));
                    }
                    *on_a = false;
                }
                let item = next_handle(b, vm, functions).await?;
                match item {
                    None => {
                        *self = VmIter::Exhausted;
                        Ok(None)
                    }
                    Some(v) => Ok(Some(v)),
                }
            }
            VmIter::Chunks { inner, n } => {
                let n = *n;
                let mut batch: Vec<VmValue> = Vec::with_capacity(n);
                for _ in 0..n {
                    let item = next_handle(inner, vm, functions).await?;
                    match item {
                        Some(v) => batch.push(v),
                        None => break,
                    }
                }
                if batch.is_empty() {
                    *self = VmIter::Exhausted;
                    Ok(None)
                } else {
                    Ok(Some(VmValue::List(Rc::new(batch))))
                }
            }
            VmIter::Windows { inner, n, buf } => {
                let n = *n;
                if buf.is_empty() {
                    // First call: fill buf to exactly n.
                    while buf.len() < n {
                        let item = next_handle(inner, vm, functions).await?;
                        match item {
                            Some(v) => buf.push_back(v),
                            None => {
                                *self = VmIter::Exhausted;
                                return Ok(None);
                            }
                        }
                    }
                } else {
                    // Subsequent calls: slide by one.
                    let item = next_handle(inner, vm, functions).await?;
                    match item {
                        Some(v) => {
                            buf.pop_front();
                            buf.push_back(v);
                        }
                        None => {
                            *self = VmIter::Exhausted;
                            return Ok(None);
                        }
                    }
                }
                let snapshot: Vec<VmValue> = buf.iter().cloned().collect();
                Ok(Some(VmValue::List(Rc::new(snapshot))))
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

/// Advance a handle without holding a `RefCell` borrow across the await.
///
/// Swaps the iter state out into a local owned value (replacing it with
/// `Exhausted`), runs `next` on the owned state, then swaps it back. This
/// avoids `clippy::await_holding_refcell_ref` while preserving single-pass
/// semantics: a nested `next` call on the same handle during the await would
/// see `Exhausted` (the iter protocol doesn't permit re-entrant stepping of
/// the same handle anyway).
pub async fn next_handle(
    handle: &Rc<RefCell<VmIter>>,
    vm: &mut crate::vm::Vm,
    functions: &[CompiledFunction],
) -> Result<Option<VmValue>, VmError> {
    let mut state = std::mem::replace(&mut *handle.borrow_mut(), VmIter::Exhausted);
    let result = state.next(vm, functions).await;
    // Restore the (possibly-mutated) state unless the inner call itself
    // replaced the state with Exhausted via `*self = ...`.
    *handle.borrow_mut() = state;
    result
}

/// Fully consume an iter handle into a Vec of values.
pub async fn drain(
    handle: &Rc<RefCell<VmIter>>,
    vm: &mut crate::vm::Vm,
    functions: &[CompiledFunction],
) -> Result<Vec<VmValue>, VmError> {
    let mut out = Vec::new();
    loop {
        let v = next_handle(handle, vm, functions).await?;
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
        VmValue::Range(r) => {
            let stop = if r.inclusive {
                r.end.saturating_add(1)
            } else {
                r.end
            };
            VmIter::Range {
                next: r.start,
                stop,
            }
        }
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
