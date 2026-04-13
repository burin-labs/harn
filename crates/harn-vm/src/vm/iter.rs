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
    /// Terminal state: `next` always returns `None`.
    Exhausted,
}

impl VmIter {
    /// Produce the next value, or `None` when exhausted.
    ///
    /// Step (a) doesn't invoke closures, so the `_vm` / `_functions` arguments
    /// are unused. They're kept in the signature so combinator variants added
    /// in later steps can call back into the VM without a breaking API change.
    pub async fn next(
        &mut self,
        _vm: &mut crate::vm::Vm,
        _functions: &[CompiledFunction],
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
                    let mut map: BTreeMap<String, VmValue> = BTreeMap::new();
                    map.insert("key".to_string(), VmValue::String(Rc::from(k.as_str())));
                    map.insert("value".to_string(), v);
                    *idx += 1;
                    Ok(Some(VmValue::Dict(Rc::new(map))))
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
