//! Lazy iterator protocol for the Harn VM.
//!
//! `VmIter` is the backing enum for `VmValue::Iter`. It's a single-pass, fused
//! iterator; once `next` returns `None` the variant is replaced with
//! `Exhausted`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::value::{VmChannelHandle, VmError, VmGenerator, VmStream, VmValue};

pub type VmIterHandle = Arc<Mutex<VmIter>>;

fn range_initial_done(start: i64, end: i64, inclusive: bool) -> bool {
    if inclusive {
        start > end
    } else {
        start >= end
    }
}

fn range_next(next: &mut i64, end: i64, inclusive: bool, done: &mut bool) -> Option<i64> {
    if *done {
        return None;
    }
    let value = *next;
    let at_end = if inclusive {
        value >= end
    } else {
        value
            .checked_add(1)
            .is_none_or(|candidate| candidate >= end)
    };
    if at_end {
        *done = true;
    } else {
        *next += 1;
    }
    Some(value)
}

#[derive(Debug)]
pub struct VmBroadcastState {
    source: VmIterHandle,
    buffer: Vec<VmValue>,
    exhausted: bool,
}

/// Backing enum for `VmValue::Iter`. See module docs.
#[derive(Debug)]
pub enum VmIter {
    /// Step through a lazy integer range without materializing.
    Range {
        next: i64,
        end: i64,
        inclusive: bool,
        done: bool,
    },
    /// Snapshot over a shared list / set backing store.
    Vec {
        items: Arc<Vec<VmValue>>,
        idx: usize,
    },
    /// Snapshot over a dict; yields `Pair(key, value)` items.
    Dict {
        entries: Arc<BTreeMap<String, VmValue>>,
        keys: Vec<String>,
        idx: usize,
    },
    /// Unicode scalar iteration over a string.
    Chars { s: Arc<str>, byte_idx: usize },
    /// Drains a generator's yield channel.
    Gen { gen: Arc<VmGenerator> },
    /// Drains a stream's emit channel.
    Stream { stream: Arc<VmStream> },
    /// Reads from a channel handle.
    Chan { handle: Arc<VmChannelHandle> },
    /// Maps each item through a closure.
    Map { inner: VmIterHandle, f: VmValue },
    /// Runs a callback for side effects, then yields the original item.
    Tap { inner: VmIterHandle, f: VmValue },
    /// Keeps only items for which the predicate is truthy.
    Filter { inner: VmIterHandle, p: VmValue },
    /// Running fold that yields each accumulator.
    Scan {
        inner: VmIterHandle,
        acc: VmValue,
        f: VmValue,
    },
    /// Maps each item to an iterable and flattens one level.
    FlatMap {
        inner: VmIterHandle,
        f: VmValue,
        cur: Option<VmIterHandle>,
    },
    /// Yields up to `remaining` items from `inner`, then becomes Exhausted.
    Take {
        inner: VmIterHandle,
        remaining: usize,
    },
    /// Skips the first `remaining` items from `inner` on the first call, then
    /// forwards. `remaining == 0` is the sentinel for "already primed".
    Skip {
        inner: VmIterHandle,
        remaining: usize,
    },
    /// Yields items from `inner` while the predicate is truthy; after the
    /// first falsy predicate or inner exhaustion, becomes Exhausted.
    TakeWhile {
        inner: VmIterHandle,
        p: VmValue,
        done: bool,
    },
    /// Yields items until the predicate is truthy. The matching sentinel item
    /// is consumed but not yielded.
    TakeUntil { inner: VmIterHandle, p: VmValue },
    /// Discards items while the predicate is truthy; after the first falsy
    /// item, forwards that item and all subsequent items from `inner`.
    SkipWhile {
        inner: VmIterHandle,
        p: VmValue,
        primed: bool,
    },
    /// Advances two inner iters in lockstep; yields `Pair(a, b)` until either
    /// side is exhausted.
    Zip { a: VmIterHandle, b: VmIterHandle },
    /// Yields `Pair(i, item)` starting at `i = 0`.
    Enumerate { inner: VmIterHandle, i: i64 },
    /// Concatenates two iters: drains `a` first, then `b`.
    Chain {
        a: VmIterHandle,
        b: VmIterHandle,
        on_a: bool,
    },
    /// Drains any non-exhausted source in rotating order.
    Merge {
        sources: Vec<Option<VmIterHandle>>,
        cursor: usize,
    },
    /// Strict round-robin over non-exhausted sources.
    Interleave {
        sources: Vec<Option<VmIterHandle>>,
        cursor: usize,
    },
    /// First source to yield wins; subsequent pulls only read that source.
    Race {
        sources: Vec<Option<VmIterHandle>>,
        winner: Option<VmIterHandle>,
    },
    /// One source fanned out into several single-pass branches.
    Broadcast {
        shared: Arc<Mutex<VmBroadcastState>>,
        branch: usize,
        index: usize,
    },
    /// Sleeps between emissions after the first item.
    Throttle {
        inner: VmIterHandle,
        interval_ms: u64,
        next_ready: Option<tokio::time::Instant>,
    },
    /// Coalesces immediately available bursts and emits the last item seen
    /// after the quiet window.
    Debounce { inner: VmIterHandle, window_ms: u64 },
    /// Yields `VmValue::List` batches of up to `n` items from `inner`.
    /// The final batch may be shorter; empty input yields no batches.
    Chunks { inner: VmIterHandle, n: usize },
    /// Yields sliding windows of exactly `n` items from `inner` as `VmValue::List`.
    /// If the input has fewer than `n` items total, no windows are yielded.
    Windows {
        inner: VmIterHandle,
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
    /// closures through the `vm` parameter.
    pub fn next<'a>(
        &'a mut self,
        vm: &'a mut crate::vm::Vm,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<VmValue>, VmError>> + Send + 'a>,
    > {
        Box::pin(async move { self.next_impl(vm).await })
    }

    async fn next_impl(&mut self, vm: &mut crate::vm::Vm) -> Result<Option<VmValue>, VmError> {
        match self {
            VmIter::Exhausted => Ok(None),
            VmIter::Range {
                next,
                end,
                inclusive,
                done,
            } => {
                if let Some(v) = range_next(next, *end, *inclusive, done) {
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
                    Ok(Some(VmValue::Pair(std::sync::Arc::new((
                        VmValue::String(std::sync::Arc::from(k.as_str())),
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
                    let mut buf = [0u8; 4];
                    let encoded = c.encode_utf8(&mut buf);
                    Ok(Some(VmValue::String(std::sync::Arc::from(&*encoded))))
                } else {
                    *self = VmIter::Exhausted;
                    Ok(None)
                }
            }
            VmIter::Gen { gen } => {
                if gen.is_done() {
                    *self = VmIter::Exhausted;
                    return Ok(None);
                }
                let rx = gen.receiver.clone();
                let mut guard = rx.lock().await;
                match guard.recv().await {
                    Some(Ok(v)) => Ok(Some(v)),
                    Some(Err(error)) => {
                        gen.mark_done();
                        drop(guard);
                        *self = VmIter::Exhausted;
                        Err(error)
                    }
                    None => {
                        gen.mark_done();
                        drop(guard);
                        *self = VmIter::Exhausted;
                        Ok(None)
                    }
                }
            }
            VmIter::Stream { stream } => {
                if stream.is_done() {
                    *self = VmIter::Exhausted;
                    return Ok(None);
                }
                let rx = stream.receiver.clone();
                let mut guard = rx.lock().await;
                match guard.recv().await {
                    Some(Ok(v)) => Ok(Some(v)),
                    Some(Err(error)) => {
                        stream.mark_done();
                        drop(guard);
                        *self = VmIter::Exhausted;
                        Err(error)
                    }
                    None => {
                        stream.mark_done();
                        drop(guard);
                        *self = VmIter::Exhausted;
                        Ok(None)
                    }
                }
            }
            VmIter::Map { inner, f } => {
                let f = f.clone();
                let item = next_handle(inner, vm).await?;
                match item {
                    None => {
                        *self = VmIter::Exhausted;
                        Ok(None)
                    }
                    Some(v) => {
                        let out = vm.call_callable_one(&f, &v).await?;
                        Ok(Some(out))
                    }
                }
            }
            VmIter::Tap { inner, f } => {
                let f = f.clone();
                let item = next_handle(inner, vm).await?;
                match item {
                    None => {
                        *self = VmIter::Exhausted;
                        Ok(None)
                    }
                    Some(v) => {
                        vm.call_callable_one(&f, &v).await?;
                        Ok(Some(v))
                    }
                }
            }
            VmIter::Filter { inner, p } => {
                let p = p.clone();
                loop {
                    let item = next_handle(inner, vm).await?;
                    match item {
                        None => {
                            *self = VmIter::Exhausted;
                            return Ok(None);
                        }
                        Some(v) => {
                            let keep = vm.call_callable_one(&p, &v).await?;
                            if keep.is_truthy() {
                                return Ok(Some(v));
                            }
                        }
                    }
                }
            }
            VmIter::Scan { inner, acc, f } => {
                let f = f.clone();
                let item = next_handle(inner, vm).await?;
                match item {
                    None => {
                        *self = VmIter::Exhausted;
                        Ok(None)
                    }
                    Some(v) => {
                        let next_acc = vm.call_callable_two(&f, acc, &v).await?;
                        *acc = next_acc.clone();
                        Ok(Some(next_acc))
                    }
                }
            }
            VmIter::FlatMap { inner, f, cur } => {
                let f = f.clone();
                loop {
                    if let Some(cur_iter) = cur.clone() {
                        let item = next_handle(&cur_iter, vm).await?;
                        if let Some(v) = item {
                            return Ok(Some(v));
                        }
                        *cur = None;
                    }
                    let item = next_handle(inner, vm).await?;
                    match item {
                        None => {
                            *self = VmIter::Exhausted;
                            return Ok(None);
                        }
                        Some(v) => {
                            let result = vm.call_callable_one(&f, &v).await?;
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
                let item = next_handle(inner, vm).await?;
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
                    let item = next_handle(inner, vm).await?;
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
                let item = next_handle(inner, vm).await?;
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
                let item = next_handle(inner, vm).await?;
                match item {
                    None => {
                        *self = VmIter::Exhausted;
                        Ok(None)
                    }
                    Some(v) => {
                        let keep = vm.call_callable_one(&p, &v).await?;
                        if keep.is_truthy() {
                            Ok(Some(v))
                        } else {
                            *self = VmIter::Exhausted;
                            Ok(None)
                        }
                    }
                }
            }
            VmIter::TakeUntil { inner, p } => {
                let p = p.clone();
                let item = next_handle(inner, vm).await?;
                match item {
                    None => {
                        *self = VmIter::Exhausted;
                        Ok(None)
                    }
                    Some(v) => {
                        let stop = vm.call_callable_one(&p, &v).await?;
                        if stop.is_truthy() {
                            *self = VmIter::Exhausted;
                            Ok(None)
                        } else {
                            Ok(Some(v))
                        }
                    }
                }
            }
            VmIter::SkipWhile { inner, p, primed } => {
                if *primed {
                    let item = next_handle(inner, vm).await?;
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
                    let item = next_handle(inner, vm).await?;
                    match item {
                        None => {
                            *self = VmIter::Exhausted;
                            return Ok(None);
                        }
                        Some(v) => {
                            let drop_it = vm.call_callable_one(&p, &v).await?;
                            if !drop_it.is_truthy() {
                                *primed = true;
                                return Ok(Some(v));
                            }
                        }
                    }
                }
            }
            VmIter::Zip { a, b } => {
                let ia = next_handle(a, vm).await?;
                let x = match ia {
                    None => {
                        *self = VmIter::Exhausted;
                        return Ok(None);
                    }
                    Some(v) => v,
                };
                let ib = next_handle(b, vm).await?;
                let y = match ib {
                    None => {
                        *self = VmIter::Exhausted;
                        return Ok(None);
                    }
                    Some(v) => v,
                };
                Ok(Some(VmValue::Pair(std::sync::Arc::new((x, y)))))
            }
            VmIter::Enumerate { inner, i } => {
                let item = next_handle(inner, vm).await?;
                match item {
                    None => {
                        *self = VmIter::Exhausted;
                        Ok(None)
                    }
                    Some(v) => {
                        let idx = *i;
                        *i += 1;
                        Ok(Some(VmValue::Pair(std::sync::Arc::new((
                            VmValue::Int(idx),
                            v,
                        )))))
                    }
                }
            }
            VmIter::Chain { a, b, on_a } => {
                if *on_a {
                    let item = next_handle(a, vm).await?;
                    if let Some(v) = item {
                        return Ok(Some(v));
                    }
                    *on_a = false;
                }
                let item = next_handle(b, vm).await?;
                match item {
                    None => {
                        *self = VmIter::Exhausted;
                        Ok(None)
                    }
                    Some(v) => Ok(Some(v)),
                }
            }
            VmIter::Merge { sources, cursor } => loop {
                if sources.is_empty() || sources.iter().all(Option::is_none) {
                    *self = VmIter::Exhausted;
                    return Ok(None);
                }
                let len = sources.len();
                let mut live = 0usize;
                for offset in 0..len {
                    let idx = (*cursor + offset) % len;
                    let Some(handle) = sources[idx].clone() else {
                        continue;
                    };
                    match try_next_ready(&handle, vm).await? {
                        Some(v) => {
                            *cursor = (idx + 1) % len;
                            return Ok(Some(v));
                        }
                        None => {
                            if is_exhausted_handle(&handle) {
                                sources[idx] = None;
                            } else {
                                live += 1;
                            }
                        }
                    }
                }
                if live == 0 {
                    *self = VmIter::Exhausted;
                    return Ok(None);
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
            },
            VmIter::Interleave { sources, cursor } => {
                if sources.is_empty() || sources.iter().all(Option::is_none) {
                    *self = VmIter::Exhausted;
                    return Ok(None);
                }
                let len = sources.len();
                for offset in 0..len {
                    let idx = (*cursor + offset) % len;
                    let Some(handle) = sources[idx].clone() else {
                        continue;
                    };
                    match next_handle(&handle, vm).await? {
                        Some(v) => {
                            *cursor = (idx + 1) % len;
                            return Ok(Some(v));
                        }
                        None => {
                            sources[idx] = None;
                        }
                    }
                }
                *self = VmIter::Exhausted;
                Ok(None)
            }
            VmIter::Race { sources, winner } => {
                if let Some(handle) = winner.clone() {
                    let item = next_handle(&handle, vm).await?;
                    return match item {
                        Some(v) => Ok(Some(v)),
                        None => {
                            *self = VmIter::Exhausted;
                            Ok(None)
                        }
                    };
                }
                loop {
                    let mut live = 0usize;
                    for source in sources.iter_mut() {
                        let Some(handle) = source.clone() else {
                            continue;
                        };
                        match try_next_ready(&handle, vm).await? {
                            Some(v) => {
                                *winner = Some(handle);
                                sources.clear();
                                return Ok(Some(v));
                            }
                            None => {
                                if is_exhausted_handle(&handle) {
                                    *source = None;
                                } else {
                                    live += 1;
                                }
                            }
                        }
                    }
                    if live == 0 {
                        *self = VmIter::Exhausted;
                        return Ok(None);
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                }
            }
            VmIter::Broadcast {
                shared,
                branch,
                index,
            } => {
                let _ = branch;
                loop {
                    let mut state = std::mem::replace(&mut *shared.lock(), empty_broadcast_state());
                    if *index < state.buffer.len() {
                        let item = state.buffer[*index].clone();
                        *index += 1;
                        *shared.lock() = state;
                        return Ok(Some(item));
                    }
                    if state.exhausted {
                        *shared.lock() = state;
                        *self = VmIter::Exhausted;
                        return Ok(None);
                    }
                    let next = next_handle(&state.source, vm).await;
                    match next {
                        Err(err) => {
                            *shared.lock() = state;
                            return Err(err);
                        }
                        Ok(Some(v)) => {
                            state.buffer.push(v);
                            *shared.lock() = state;
                        }
                        Ok(None) => {
                            state.exhausted = true;
                            *shared.lock() = state;
                        }
                    }
                }
            }
            VmIter::Throttle {
                inner,
                interval_ms,
                next_ready,
            } => {
                if let Some(ready_at) = next_ready.take() {
                    let now = tokio::time::Instant::now();
                    if ready_at > now {
                        tokio::time::sleep_until(ready_at).await;
                    }
                }
                let item = next_handle(inner, vm).await?;
                match item {
                    None => {
                        *self = VmIter::Exhausted;
                        Ok(None)
                    }
                    Some(v) => {
                        *next_ready = Some(
                            tokio::time::Instant::now()
                                + tokio::time::Duration::from_millis(*interval_ms),
                        );
                        Ok(Some(v))
                    }
                }
            }
            VmIter::Debounce { inner, window_ms } => {
                let mut last = match next_handle(inner, vm).await? {
                    Some(v) => v,
                    None => {
                        *self = VmIter::Exhausted;
                        return Ok(None);
                    }
                };
                if *window_ms > 0 {
                    tokio::time::sleep(tokio::time::Duration::from_millis(*window_ms)).await;
                }
                while let Some(v) = try_next_ready(inner, vm).await? {
                    last = v;
                }
                Ok(Some(last))
            }
            VmIter::Chunks { inner, n } => {
                let n = *n;
                let mut batch: Vec<VmValue> = Vec::with_capacity(n);
                for _ in 0..n {
                    let item = next_handle(inner, vm).await?;
                    match item {
                        Some(v) => {
                            batch.push(v);
                        }
                        None => break,
                    }
                }
                if batch.is_empty() {
                    *self = VmIter::Exhausted;
                    Ok(None)
                } else {
                    Ok(Some(VmValue::List(std::sync::Arc::new(batch))))
                }
            }
            VmIter::Windows { inner, n, buf } => {
                let n = *n;
                if buf.is_empty() {
                    while buf.len() < n {
                        let item = next_handle(inner, vm).await?;
                        match item {
                            Some(v) => buf.push_back(v),
                            None => {
                                *self = VmIter::Exhausted;
                                return Ok(None);
                            }
                        }
                    }
                } else {
                    let item = next_handle(inner, vm).await?;
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
                Ok(Some(VmValue::List(std::sync::Arc::new(snapshot))))
            }
            VmIter::Chan { handle } => {
                let is_closed = handle.is_closed();
                let rx = handle.receiver.clone();
                let mut closed_rx = handle.subscribe_closed();
                let mut guard = rx.lock().await;
                let item = if is_closed {
                    guard.try_recv().ok()
                } else {
                    tokio::select! {
                        item = guard.recv() => item,
                        _ = closed_rx.changed() => guard.try_recv().ok(),
                    }
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

/// Advance a handle without holding the iterator-state lock across the await.
///
/// Swaps the iter state out into a local owned value (replacing it with
/// `Exhausted`), runs `next` on the owned state, then swaps it back. This
/// avoids blocking re-entrant iterator access while preserving single-pass
/// semantics: a nested `next` call on the same handle during the await would
/// see `Exhausted` (the iter protocol doesn't permit re-entrant stepping of
/// the same handle anyway).
pub async fn next_handle(
    handle: &VmIterHandle,
    vm: &mut crate::vm::Vm,
) -> Result<Option<VmValue>, VmError> {
    let mut state = std::mem::replace(&mut *handle.lock(), VmIter::Exhausted);
    let result = state.next(vm).await;
    // Restore state unless the inner call replaced it with Exhausted.
    *handle.lock() = state;
    result
}

/// Fully consume an iter handle into a Vec of values.
pub async fn drain(handle: &VmIterHandle, vm: &mut crate::vm::Vm) -> Result<Vec<VmValue>, VmError> {
    let mut out = Vec::new();
    loop {
        let v = next_handle(handle, vm).await?;
        match v {
            Some(v) => out.push(v),
            None => break,
        }
    }
    Ok(out)
}

/// Fully consume an iter handle into a Vec, failing before pushing item
/// `max + 1`.
pub async fn drain_capped(
    handle: &VmIterHandle,
    vm: &mut crate::vm::Vm,
    max: usize,
) -> Result<Vec<VmValue>, VmError> {
    let mut out = Vec::new();
    loop {
        let v = next_handle(handle, vm).await?;
        match v {
            Some(v) => {
                if out.len() >= max {
                    return Err(VmError::Runtime(format!(
                        "stream.collect: max cap {max} exceeded"
                    )));
                }
                out.push(v);
            }
            None => break,
        }
    }
    Ok(out)
}

pub fn iter_handle_from_value(v: VmValue) -> Result<VmIterHandle, VmError> {
    match iter_from_value(v)? {
        VmValue::Iter(handle) => Ok(handle),
        _ => unreachable!("iter_from_value returns Iter"),
    }
}

pub fn broadcast_branches(source: VmIterHandle, n: usize) -> Vec<VmValue> {
    let shared = Arc::new(Mutex::new(VmBroadcastState {
        source,
        buffer: Vec::new(),
        exhausted: false,
    }));
    (0..n)
        .map(|branch| {
            VmValue::Iter(Arc::new(Mutex::new(VmIter::Broadcast {
                shared: Arc::clone(&shared),
                branch,
                index: 0,
            })))
        })
        .collect()
}

fn empty_broadcast_state() -> VmBroadcastState {
    VmBroadcastState {
        source: Arc::new(Mutex::new(VmIter::Exhausted)),
        buffer: Vec::new(),
        exhausted: true,
    }
}

fn try_next_ready<'a>(
    handle: &'a VmIterHandle,
    vm: &'a mut crate::vm::Vm,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Option<VmValue>, VmError>> + Send + 'a>,
> {
    Box::pin(async move {
        let mut state = std::mem::replace(&mut *handle.lock(), VmIter::Exhausted);
        let result = state.try_next_ready_impl(vm).await;
        *handle.lock() = state;
        result
    })
}

fn is_exhausted_handle(handle: &VmIterHandle) -> bool {
    matches!(&*handle.lock(), VmIter::Exhausted)
}

impl VmIter {
    async fn try_next_ready_impl(
        &mut self,
        vm: &mut crate::vm::Vm,
    ) -> Result<Option<VmValue>, VmError> {
        match self {
            VmIter::Exhausted => Ok(None),
            VmIter::Map { inner, f } => {
                let f = f.clone();
                match try_next_ready(inner, vm).await? {
                    Some(v) => Ok(Some(vm.call_callable_one(&f, &v).await?)),
                    None => {
                        if is_exhausted_handle(inner) {
                            *self = VmIter::Exhausted;
                        }
                        Ok(None)
                    }
                }
            }
            VmIter::Tap { inner, f } => {
                let f = f.clone();
                match try_next_ready(inner, vm).await? {
                    Some(v) => {
                        vm.call_callable_one(&f, &v).await?;
                        Ok(Some(v))
                    }
                    None => {
                        if is_exhausted_handle(inner) {
                            *self = VmIter::Exhausted;
                        }
                        Ok(None)
                    }
                }
            }
            VmIter::Filter { inner, p } => {
                let p = p.clone();
                loop {
                    match try_next_ready(inner, vm).await? {
                        Some(v) => {
                            let keep = vm.call_callable_one(&p, &v).await?;
                            if keep.is_truthy() {
                                return Ok(Some(v));
                            }
                        }
                        None => {
                            if is_exhausted_handle(inner) {
                                *self = VmIter::Exhausted;
                            }
                            return Ok(None);
                        }
                    }
                }
            }
            VmIter::Scan { inner, acc, f } => {
                let f = f.clone();
                match try_next_ready(inner, vm).await? {
                    Some(v) => {
                        let next_acc = vm.call_callable_two(&f, acc, &v).await?;
                        *acc = next_acc.clone();
                        Ok(Some(next_acc))
                    }
                    None => {
                        if is_exhausted_handle(inner) {
                            *self = VmIter::Exhausted;
                        }
                        Ok(None)
                    }
                }
            }
            VmIter::FlatMap { inner, f, cur } => {
                let f = f.clone();
                loop {
                    if let Some(cur_iter) = cur.clone() {
                        match try_next_ready(&cur_iter, vm).await? {
                            Some(v) => return Ok(Some(v)),
                            None => {
                                if is_exhausted_handle(&cur_iter) {
                                    *cur = None;
                                }
                                return Ok(None);
                            }
                        }
                    }
                    match try_next_ready(inner, vm).await? {
                        Some(v) => {
                            let result = vm.call_callable_one(&f, &v).await?;
                            *cur = Some(iter_handle_from_value(result)?);
                        }
                        None => {
                            if is_exhausted_handle(inner) {
                                *self = VmIter::Exhausted;
                            }
                            return Ok(None);
                        }
                    }
                }
            }
            VmIter::Take { inner, remaining } => {
                if *remaining == 0 {
                    *self = VmIter::Exhausted;
                    return Ok(None);
                }
                match try_next_ready(inner, vm).await? {
                    Some(v) => {
                        *remaining -= 1;
                        if *remaining == 0 {
                            *self = VmIter::Exhausted;
                        }
                        Ok(Some(v))
                    }
                    None => {
                        if is_exhausted_handle(inner) {
                            *self = VmIter::Exhausted;
                        }
                        Ok(None)
                    }
                }
            }
            VmIter::TakeUntil { inner, p } => {
                let p = p.clone();
                match try_next_ready(inner, vm).await? {
                    Some(v) => {
                        let stop = vm.call_callable_one(&p, &v).await?;
                        if stop.is_truthy() {
                            *self = VmIter::Exhausted;
                            Ok(None)
                        } else {
                            Ok(Some(v))
                        }
                    }
                    None => {
                        if is_exhausted_handle(inner) {
                            *self = VmIter::Exhausted;
                        }
                        Ok(None)
                    }
                }
            }
            VmIter::Throttle {
                inner,
                interval_ms,
                next_ready,
            } => {
                if let Some(ready_at) = *next_ready {
                    if ready_at > tokio::time::Instant::now() {
                        return Ok(None);
                    }
                }
                match try_next_ready(inner, vm).await? {
                    Some(v) => {
                        *next_ready = Some(
                            tokio::time::Instant::now()
                                + tokio::time::Duration::from_millis(*interval_ms),
                        );
                        Ok(Some(v))
                    }
                    None => {
                        if is_exhausted_handle(inner) {
                            *self = VmIter::Exhausted;
                        }
                        Ok(None)
                    }
                }
            }
            VmIter::Range { .. }
            | VmIter::Vec { .. }
            | VmIter::Dict { .. }
            | VmIter::Chars { .. }
            | VmIter::Skip { .. }
            | VmIter::TakeWhile { .. }
            | VmIter::SkipWhile { .. }
            | VmIter::Zip { .. }
            | VmIter::Enumerate { .. }
            | VmIter::Chain { .. }
            | VmIter::Merge { .. }
            | VmIter::Interleave { .. }
            | VmIter::Race { .. }
            | VmIter::Broadcast { .. }
            | VmIter::Debounce { .. }
            | VmIter::Chunks { .. }
            | VmIter::Windows { .. } => self.next(vm).await,
            VmIter::Gen { gen } => {
                if gen.is_done() {
                    *self = VmIter::Exhausted;
                    return Ok(None);
                }
                let rx = gen.receiver.clone();
                let result = match rx.try_lock() {
                    Ok(mut guard) => match guard.try_recv() {
                        Ok(Ok(v)) => Ok(Some(v)),
                        Ok(Err(error)) => {
                            gen.mark_done();
                            *self = VmIter::Exhausted;
                            Err(error)
                        }
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => Ok(None),
                        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                            gen.mark_done();
                            *self = VmIter::Exhausted;
                            Ok(None)
                        }
                    },
                    Err(_) => Ok(None),
                };
                result
            }
            VmIter::Stream { stream } => {
                if stream.is_done() {
                    *self = VmIter::Exhausted;
                    return Ok(None);
                }
                let rx = stream.receiver.clone();
                let result = match rx.try_lock() {
                    Ok(mut guard) => match guard.try_recv() {
                        Ok(Ok(v)) => Ok(Some(v)),
                        Ok(Err(error)) => {
                            stream.mark_done();
                            *self = VmIter::Exhausted;
                            Err(error)
                        }
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => Ok(None),
                        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                            stream.mark_done();
                            *self = VmIter::Exhausted;
                            Ok(None)
                        }
                    },
                    Err(_) => Ok(None),
                };
                result
            }
            VmIter::Chan { handle } => {
                let rx = handle.receiver.clone();
                let result = match rx.try_lock() {
                    Ok(mut guard) => match guard.try_recv() {
                        Ok(v) => Ok(Some(v)),
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => Ok(None),
                        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                            *self = VmIter::Exhausted;
                            Ok(None)
                        }
                    },
                    Err(_) => Ok(None),
                };
                result
            }
        }
    }
}

/// Convenience: wrap a source value into a `VmValue::Iter`. Used by the
/// `iter()` builtin and by combinator/sink implementations in later steps.
pub fn iter_from_value(v: VmValue) -> Result<VmValue, VmError> {
    let inner = match v {
        VmValue::Iter(h) => return Ok(VmValue::Iter(h)),
        VmValue::Range(r) => VmIter::Range {
            next: r.start,
            end: r.end,
            inclusive: r.inclusive,
            done: range_initial_done(r.start, r.end, r.inclusive),
        },
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
        VmValue::Stream(stream) => VmIter::Stream { stream },
        VmValue::Channel(handle) => VmIter::Chan { handle },
        other => {
            return Err(VmError::TypeError(format!(
                "iter: value of type {} is not iterable",
                other.type_name()
            )))
        }
    };
    Ok(VmValue::Iter(Arc::new(Mutex::new(inner))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::VmRange;

    fn run_iter_test(test: impl std::future::Future<Output = ()>) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(test);
    }

    #[test]
    fn inclusive_range_at_i64_max_yields_the_endpoint() {
        run_iter_test(async {
            let mut vm = crate::vm::Vm::new();
            let VmValue::Iter(handle) = iter_from_value(VmValue::Range(VmRange {
                start: i64::MAX,
                end: i64::MAX,
                inclusive: true,
            }))
            .unwrap() else {
                panic!("expected iter");
            };

            let first = next_handle(&handle, &mut vm).await.unwrap();
            assert!(matches!(first, Some(VmValue::Int(value)) if value == i64::MAX));
            assert!(next_handle(&handle, &mut vm).await.unwrap().is_none());
        });
    }

    #[test]
    fn exclusive_range_at_i64_max_is_empty() {
        run_iter_test(async {
            let mut vm = crate::vm::Vm::new();
            let VmValue::Iter(handle) = iter_from_value(VmValue::Range(VmRange {
                start: i64::MAX,
                end: i64::MAX,
                inclusive: false,
            }))
            .unwrap() else {
                panic!("expected iter");
            };

            assert!(next_handle(&handle, &mut vm).await.unwrap().is_none());
        });
    }
}
