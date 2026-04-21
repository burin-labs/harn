use std::collections::BTreeMap;
use std::rc::Rc;

use crate::chunk::Op;
use crate::value::{VmError, VmValue};

impl super::super::Vm {
    pub(super) async fn try_execute_iter_op(&mut self, op: u8) -> Result<bool, VmError> {
        if op == Op::IterInit as u8 {
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
                        closed: ch.closed.clone(),
                    });
                }
                VmValue::Generator(gen) => {
                    self.iterators
                        .push(super::super::IterState::Generator { gen });
                }
                VmValue::Range(r) => {
                    let stop = if r.inclusive {
                        // Saturate to avoid i64 overflow on `i64::MAX to i64::MAX`.
                        r.end.saturating_add(1)
                    } else {
                        r.end
                    };
                    // `5 to 1` is simply empty — no reverse iteration.
                    let next = r.start;
                    self.iterators
                        .push(super::super::IterState::Range { next, stop });
                }
                VmValue::Iter(handle) => {
                    self.iterators
                        .push(super::super::IterState::VmIter { handle });
                }
                _ => {
                    self.iterators.push(super::super::IterState::Vec {
                        items: Rc::new(Vec::new()),
                        idx: 0,
                    });
                }
            }
        } else if op == Op::IterNext as u8 {
            let frame = self.frames.last_mut().unwrap();
            let target = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            // Clone the handle so we don't hold a borrow on self.iterators
            // across the async next() call.
            let vm_iter_handle = match self.iterators.last() {
                Some(super::super::IterState::VmIter { handle }) => Some(handle.clone()),
                _ => None,
            };
            if let Some(handle) = vm_iter_handle {
                // Safe for recursive VM reentry via closures as long as they
                // don't re-enter the same iter handle.
                let next_val = crate::vm::iter::next_handle(&handle, self).await?;
                match next_val {
                    Some(v) => self.stack.push(v),
                    None => {
                        self.iterators.pop();
                        let frame = self.frames.last_mut().unwrap();
                        frame.ip = target;
                    }
                }
            } else if let Some(state) = self.iterators.last_mut() {
                match state {
                    super::super::IterState::Vec { items, idx } => {
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
                    super::super::IterState::Dict { entries, keys, idx } => {
                        if *idx < keys.len() {
                            let key = &keys[*idx];
                            let value = entries.get(key).cloned().unwrap_or(VmValue::Nil);
                            *idx += 1;
                            self.stack.push(VmValue::Dict(Rc::new(BTreeMap::from([
                                ("key".to_string(), VmValue::String(Rc::from(key.as_str()))),
                                ("value".to_string(), value),
                            ]))));
                        } else {
                            self.iterators.pop();
                            let frame = self.frames.last_mut().unwrap();
                            frame.ip = target;
                        }
                    }
                    super::super::IterState::Channel { receiver, closed } => {
                        let rx = receiver.clone();
                        let is_closed = closed.load(std::sync::atomic::Ordering::Relaxed);
                        let mut guard = rx.lock().await;
                        // Closed sender: drain without blocking.
                        let item = if is_closed {
                            guard.try_recv().ok()
                        } else {
                            guard.recv().await
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
                    super::super::IterState::Range { next, stop } => {
                        if *next < *stop {
                            let v = *next;
                            *next += 1;
                            self.stack.push(VmValue::Int(v));
                        } else {
                            self.iterators.pop();
                            let frame = self.frames.last_mut().unwrap();
                            frame.ip = target;
                        }
                    }
                    super::super::IterState::Generator { gen } => {
                        if gen.done.get() {
                            self.iterators.pop();
                            let frame = self.frames.last_mut().unwrap();
                            frame.ip = target;
                        } else {
                            let rx = gen.receiver.clone();
                            let mut guard = rx.lock().await;
                            match guard.recv().await {
                                Some(val) => {
                                    self.stack.push(val);
                                }
                                None => {
                                    gen.done.set(true);
                                    drop(guard);
                                    self.iterators.pop();
                                    let frame = self.frames.last_mut().unwrap();
                                    frame.ip = target;
                                }
                            }
                        }
                    }
                    super::super::IterState::VmIter { .. } => {
                        unreachable!("VmIter state handled before this match");
                    }
                }
            } else {
                let frame = self.frames.last_mut().unwrap();
                frame.ip = target;
            }
        } else if op == Op::PopIterator as u8 {
            self.iterators.pop();
        } else {
            return Ok(false);
        }
        Ok(true)
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
            let items = Rc::new(vec![VmValue::Int(1), VmValue::Int(2)]);
            let mut vm = Vm::new();
            vm.stack.push(VmValue::List(items.clone()));

            vm.try_execute_iter_op(Op::IterInit as u8).await.unwrap();

            match vm.iterators.last().unwrap() {
                IterState::Vec {
                    items: iter_items,
                    idx,
                } => {
                    assert!(Rc::ptr_eq(&items, iter_items));
                    assert_eq!(*idx, 0);
                }
                _ => panic!("expected vec iterator state"),
            }
        });
    }

    #[test]
    fn iter_init_set_keeps_shared_backing_store() {
        run_iter_init_test(async {
            let items = Rc::new(vec![VmValue::Int(1), VmValue::Int(2)]);
            let mut vm = Vm::new();
            vm.stack.push(VmValue::Set(items.clone()));

            vm.try_execute_iter_op(Op::IterInit as u8).await.unwrap();

            match vm.iterators.last().unwrap() {
                IterState::Vec {
                    items: iter_items,
                    idx,
                } => {
                    assert!(Rc::ptr_eq(&items, iter_items));
                    assert_eq!(*idx, 0);
                }
                _ => panic!("expected vec iterator state"),
            }
        });
    }

    #[test]
    fn iter_init_dict_keeps_shared_entries_and_snapshots_keys() {
        run_iter_init_test(async {
            let entries = Rc::new(BTreeMap::from([
                ("a".to_string(), VmValue::Int(1)),
                ("b".to_string(), VmValue::Int(2)),
            ]));
            let mut vm = Vm::new();
            vm.stack.push(VmValue::Dict(entries.clone()));

            vm.try_execute_iter_op(Op::IterInit as u8).await.unwrap();

            match vm.iterators.last().unwrap() {
                IterState::Dict {
                    entries: iter_entries,
                    keys,
                    idx,
                } => {
                    assert!(Rc::ptr_eq(&entries, iter_entries));
                    assert_eq!(keys.as_slice(), ["a".to_string(), "b".to_string()]);
                    assert_eq!(*idx, 0);
                }
                _ => panic!("expected dict iterator state"),
            }
        });
    }
}
