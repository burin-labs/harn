use std::collections::BTreeMap;
use std::sync::Arc;

use crate::value::{VmError, VmValue};

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

impl super::super::Vm {
    pub(super) fn execute_iter_init(&mut self) -> Result<(), VmError> {
        let iterable = self.pop()?;
        match iterable {
            VmValue::List(items) => {
                self.iterators
                    .push(super::super::IterState::Vec { items, idx: 0 });
            }
            VmValue::Dict(map) => {
                let keys = map.keys().cloned().collect();
                self.iterators.push(super::super::IterState::Dict {
                    entries: map,
                    keys,
                    idx: 0,
                });
            }
            VmValue::Set(items) => {
                self.iterators
                    .push(super::super::IterState::Vec { items, idx: 0 });
            }
            VmValue::Channel(ch) => {
                self.iterators.push(super::super::IterState::Channel {
                    receiver: ch.receiver.clone(),
                    close: ch.close.clone(),
                });
            }
            VmValue::Generator(gen) => {
                self.iterators
                    .push(super::super::IterState::Generator { gen });
            }
            VmValue::Stream(stream) => {
                self.iterators
                    .push(super::super::IterState::Stream { stream });
            }
            VmValue::Range(r) => {
                self.iterators.push(super::super::IterState::Range {
                    next: r.start,
                    end: r.end,
                    inclusive: r.inclusive,
                    done: range_initial_done(r.start, r.end, r.inclusive),
                });
            }
            VmValue::Iter(handle) => {
                self.iterators
                    .push(super::super::IterState::VmIter { handle });
            }
            _ => {
                self.iterators.push(super::super::IterState::Vec {
                    items: Arc::new(Vec::new()),
                    idx: 0,
                });
            }
        }
        Ok(())
    }

    /// Sync fast path for the `for-in` step opcode. Handles the Vec / Dict /
    /// Range / no-iterator-active arms inline (the >99% case in real Harn
    /// programs), returning `Some(Ok(()))` on a normal step or
    /// `Some(Err(_))` on a runtime error.
    ///
    /// Returns `None` without touching `ip` when the active iterator
    /// requires `.await` (Channel / Generator / Stream / VmIter); the
    /// caller must fall through to [`execute_iter_next_async`]. Keeping
    /// `ip` untouched on the async hand-off lets the async path read the
    /// same operand the sync path would have, with no rewinding gymnastics.
    pub(super) fn execute_iter_next_sync(&mut self) -> Option<Result<(), VmError>> {
        // Classify the iterator variant before reading the bytecode operand.
        // Channel/Generator/Stream/VmIter all suspend on a receiver lock or
        // a host-side iterator, so they belong on the async path. Leaving
        // `ip` untouched on hand-off means `execute_iter_next_async` reads
        // the operand exactly once.
        match self.iterators.last() {
            None
            | Some(super::super::IterState::Vec { .. })
            | Some(super::super::IterState::Dict { .. })
            | Some(super::super::IterState::Range { .. }) => {}
            Some(_) => return None,
        }

        let frame = self.frames.last_mut().unwrap();
        let target = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;

        match self.iterators.last_mut() {
            Some(super::super::IterState::Vec { items, idx }) => {
                if *idx < items.len() {
                    let item = items[*idx].clone();
                    *idx += 1;
                    self.stack.push(item);
                } else {
                    self.iterators.pop();
                    let frame = self.frames.last_mut().unwrap();
                    frame.ip = target;
                }
            }
            Some(super::super::IterState::Dict { entries, keys, idx }) => {
                if *idx < keys.len() {
                    let key = &keys[*idx];
                    let value = entries.get(key).cloned().unwrap_or(VmValue::Nil);
                    let entry_key = VmValue::String(std::sync::Arc::from(key.as_str()));
                    *idx += 1;
                    self.stack
                        .push(VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
                            ("key".to_string(), entry_key),
                            ("value".to_string(), value),
                        ]))));
                } else {
                    self.iterators.pop();
                    let frame = self.frames.last_mut().unwrap();
                    frame.ip = target;
                }
            }
            Some(super::super::IterState::Range {
                next,
                end,
                inclusive,
                done,
            }) => {
                if let Some(v) = range_next(next, *end, *inclusive, done) {
                    self.stack.push(VmValue::Int(v));
                } else {
                    self.iterators.pop();
                    let frame = self.frames.last_mut().unwrap();
                    frame.ip = target;
                }
            }
            None => {
                let frame = self.frames.last_mut().unwrap();
                frame.ip = target;
            }
            // The variant guard above already routed Channel/Generator/
            // Stream/VmIter to the async path before we touched `ip`.
            Some(_) => unreachable!("async iterator variant reached sync path"),
        }
        Some(Ok(()))
    }

    /// Async slow path for `for-in` step. Handles Channel / Generator /
    /// Stream / VmIter — the four iterator variants that actually suspend.
    /// Reading the jump-on-exhaustion `target` (and advancing `ip`) happens
    /// here so the sync fast path can return early without consuming
    /// bytecode when it sees an async-only iterator state.
    pub(super) async fn execute_iter_next_async(&mut self) -> Result<(), VmError> {
        let frame = self.frames.last_mut().unwrap();
        let target = frame.chunk.read_u16(frame.ip) as usize;
        frame.ip += 2;
        // Clone the handle so we don't hold a borrow on self.iterators across
        // the async next() call.
        let vm_iter_handle = match self.iterators.last() {
            Some(super::super::IterState::VmIter { handle }) => Some(handle.clone()),
            _ => None,
        };
        if let Some(handle) = vm_iter_handle {
            // Safe for recursive VM reentry via closures as long as they don't
            // re-enter the same iter handle.
            let next_val = crate::vm::iter::next_handle(&handle, self).await?;
            match next_val {
                Some(v) => self.stack.push(v),
                None => {
                    self.iterators.pop();
                    let frame = self.frames.last_mut().unwrap();
                    frame.ip = target;
                }
            }
            return Ok(());
        }

        match self.iterators.last_mut() {
            Some(super::super::IterState::Channel { receiver, close }) => {
                let rx = receiver.clone();
                let mut closed_rx = close.subscribe();
                let is_closed = close.is_closed() || *closed_rx.borrow();
                let mut guard = rx.lock().await;
                // Closed sender: drain without blocking.
                let item = if is_closed {
                    guard.try_recv().ok()
                } else {
                    tokio::select! {
                        item = guard.recv() => item,
                        _ = closed_rx.changed() => guard.try_recv().ok(),
                    }
                };
                match item {
                    Some(val) => {
                        self.stack.push(val);
                    }
                    None => {
                        drop(guard);
                        self.iterators.pop();
                        let frame = self.frames.last_mut().unwrap();
                        frame.ip = target;
                    }
                }
            }
            Some(super::super::IterState::Generator { gen }) => {
                if gen.is_done() {
                    self.iterators.pop();
                    let frame = self.frames.last_mut().unwrap();
                    frame.ip = target;
                } else {
                    let rx = gen.receiver.clone();
                    let mut guard = rx.lock().await;
                    match guard.recv().await {
                        Some(Ok(val)) => {
                            self.stack.push(val);
                        }
                        Some(Err(error)) => {
                            gen.mark_done();
                            drop(guard);
                            self.iterators.pop();
                            return Err(error);
                        }
                        None => {
                            gen.mark_done();
                            drop(guard);
                            self.iterators.pop();
                            let frame = self.frames.last_mut().unwrap();
                            frame.ip = target;
                        }
                    }
                }
            }
            Some(super::super::IterState::Stream { stream }) => {
                if stream.is_done() {
                    self.iterators.pop();
                    let frame = self.frames.last_mut().unwrap();
                    frame.ip = target;
                } else {
                    let rx = stream.receiver.clone();
                    let mut guard = rx.lock().await;
                    match guard.recv().await {
                        Some(Ok(val)) => {
                            self.stack.push(val);
                        }
                        Some(Err(error)) => {
                            stream.mark_done();
                            drop(guard);
                            self.iterators.pop();
                            return Err(error);
                        }
                        None => {
                            stream.mark_done();
                            drop(guard);
                            self.iterators.pop();
                            let frame = self.frames.last_mut().unwrap();
                            frame.ip = target;
                        }
                    }
                }
            }
            // VmIter was handled above; sync variants belong on the sync
            // path; an empty iterator stack would have been routed to the
            // sync path too. Reaching this branch means the sync/async
            // classification in `execute_iter_next_sync` drifted from the
            // arms above.
            _ => {
                debug_assert!(
                    false,
                    "execute_iter_next_async reached non-async iterator state — \
                     dispatch tables in execute_iter_next_sync / execute_iter_next_async \
                     are out of sync"
                );
                return Err(VmError::Runtime(
                    "internal VM dispatch error: iter_next async slow path \
                     reached a non-async iterator state"
                        .into(),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn execute_pop_iterator(&mut self) {
        if let Some(super::super::IterState::Stream { stream }) = self.iterators.pop() {
            stream.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::{IterState, Vm};

    fn run_iter_init_test(test: impl std::future::Future<Output = ()>) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(test);
    }

    #[test]
    fn iter_init_list_keeps_shared_backing_store() {
        run_iter_init_test(async {
            let items = Arc::new(vec![VmValue::Int(1), VmValue::Int(2)]);
            let mut vm = Vm::new();
            vm.stack.push(VmValue::List(items.clone()));

            vm.execute_iter_init().unwrap();

            match vm.iterators.last().unwrap() {
                IterState::Vec {
                    items: iter_items,
                    idx,
                } => {
                    assert!(Arc::ptr_eq(&items, iter_items));
                    assert_eq!(*idx, 0);
                }
                _ => panic!("expected vec iterator state"),
            }
        });
    }

    #[test]
    fn iter_init_set_keeps_shared_backing_store() {
        run_iter_init_test(async {
            let items = Arc::new(vec![VmValue::Int(1), VmValue::Int(2)]);
            let mut vm = Vm::new();
            vm.stack.push(VmValue::Set(items.clone()));

            vm.execute_iter_init().unwrap();

            match vm.iterators.last().unwrap() {
                IterState::Vec {
                    items: iter_items,
                    idx,
                } => {
                    assert!(Arc::ptr_eq(&items, iter_items));
                    assert_eq!(*idx, 0);
                }
                _ => panic!("expected vec iterator state"),
            }
        });
    }

    #[test]
    fn iter_init_dict_keeps_shared_entries_and_snapshots_keys() {
        run_iter_init_test(async {
            let entries = Arc::new(BTreeMap::from([
                ("a".to_string(), VmValue::Int(1)),
                ("b".to_string(), VmValue::Int(2)),
            ]));
            let mut vm = Vm::new();
            vm.stack.push(VmValue::Dict(entries.clone()));

            vm.execute_iter_init().unwrap();

            match vm.iterators.last().unwrap() {
                IterState::Dict {
                    entries: iter_entries,
                    keys,
                    idx,
                } => {
                    assert!(Arc::ptr_eq(&entries, iter_entries));
                    assert_eq!(keys.as_slice(), ["a".to_string(), "b".to_string()]);
                    assert_eq!(*idx, 0);
                }
                _ => panic!("expected dict iterator state"),
            }
        });
    }

    #[test]
    fn iter_init_inclusive_range_at_i64_max_is_not_empty() {
        run_iter_init_test(async {
            let mut vm = Vm::new();
            vm.stack.push(VmValue::Range(crate::value::VmRange {
                start: i64::MAX,
                end: i64::MAX,
                inclusive: true,
            }));

            vm.execute_iter_init().unwrap();

            match vm.iterators.last().unwrap() {
                IterState::Range {
                    next,
                    end,
                    inclusive,
                    done,
                } => {
                    assert_eq!(*next, i64::MAX);
                    assert_eq!(*end, i64::MAX);
                    assert!(*inclusive);
                    assert!(!*done);
                }
                _ => panic!("expected range iterator state"),
            }
        });
    }
}
