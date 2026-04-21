use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use crate::value::{VmError, VmTaskHandle, VmValue};

/// Decode the `cap_val` stack operand pushed by `parallel ... with
/// { max_concurrent: N }`. A value of `0` (emitted when no option was
/// given) and any negative integer both mean "unlimited"; returning
/// `None` tells callers to run all tasks without a slot limit. Any
/// non-integer is rejected as a type error — the parser should have
/// already caught this, so hitting it implies a VM/compiler drift.
fn parallel_cap_from_value(cap_val: &VmValue, task_count: usize) -> Result<Option<usize>, VmError> {
    match cap_val {
        VmValue::Int(n) => {
            if *n <= 0 {
                Ok(None)
            } else {
                Ok(Some((*n as usize).min(task_count.max(1))))
            }
        }
        VmValue::Nil => Ok(None),
        other => Err(VmError::TypeError(format!(
            "parallel max_concurrent must be an int; got {}",
            other.type_name()
        ))),
    }
}

/// Run `futures` concurrently, capped to at most `cap` in-flight tasks
/// at any moment (or unlimited when `cap` is `None`). Results come back
/// in source order so callers can index by original position. A single
/// join error fails the whole batch, mirroring the pre-cap behavior of
/// the `Parallel*` opcodes.
async fn run_capped_ordered<F, T>(
    futures: Vec<F>,
    cap: Option<usize>,
    error_label: &'static str,
) -> Result<Vec<T>, VmError>
where
    F: std::future::Future<Output = T> + 'static,
    T: 'static,
{
    let total = futures.len();
    if total == 0 {
        return Ok(Vec::new());
    }
    let mut results: Vec<Option<T>> = (0..total).map(|_| None).collect();
    let slot = cap.unwrap_or(total).max(1).min(total);
    let mut pending: VecDeque<(usize, F)> = futures.into_iter().enumerate().collect();
    let mut join_set: tokio::task::JoinSet<(usize, T)> = tokio::task::JoinSet::new();

    while join_set.len() < slot {
        let Some((i, fut)) = pending.pop_front() else {
            break;
        };
        join_set.spawn_local(async move { (i, fut.await) });
    }

    while let Some(joined) = join_set.join_next().await {
        let (index, value) = joined.map_err(|e| VmError::Runtime(format!("{error_label}: {e}")))?;
        results[index] = Some(value);
        if let Some((i, fut)) = pending.pop_front() {
            join_set.spawn_local(async move { (i, fut.await) });
        }
    }

    Ok(results
        .into_iter()
        .map(|slot| slot.expect("run_capped_ordered: missing result slot"))
        .collect())
}

impl super::super::Vm {
    pub(super) async fn execute_parallel(&mut self) -> Result<(), VmError> {
        let _par_span =
            super::super::ScopeSpan::new(crate::tracing::SpanKind::Parallel, "parallel".into());
        let closure = self.pop()?;
        let count_val = self.pop()?;
        let cap_val = self.pop()?;
        let count = match &count_val {
            VmValue::Int(n) => (*n).max(0) as usize,
            _ => 0,
        };
        let cap = parallel_cap_from_value(&cap_val, count)?;
        if let VmValue::Closure(closure) = closure {
            let mut futures: Vec<_> = Vec::with_capacity(count);
            for i in 0..count {
                let mut child = self.child_vm();
                let closure = closure.clone();
                futures.push(async move {
                    let result = child
                        .call_closure(&closure, &[VmValue::Int(i as i64)])
                        .await?;
                    Ok::<(VmValue, String), VmError>((result, std::mem::take(&mut child.output)))
                });
            }
            let joined = run_capped_ordered(futures, cap, "Parallel task error").await?;
            let mut results = Vec::with_capacity(count);
            for entry in joined {
                let (val, task_output) = entry?;
                self.output.push_str(&task_output);
                results.push(val);
            }
            self.stack.push(VmValue::List(Rc::new(results)));
        } else {
            self.stack.push(VmValue::Nil);
        }
        Ok(())
    }

    pub(super) async fn execute_parallel_map(&mut self) -> Result<(), VmError> {
        let closure = self.pop()?;
        let list_val = self.pop()?;
        let cap_val = self.pop()?;
        match (&list_val, &closure) {
            (VmValue::List(items), VmValue::Closure(closure)) => {
                let len = items.len();
                let cap = parallel_cap_from_value(&cap_val, len)?;
                let mut futures = Vec::with_capacity(len);
                for item in items.iter() {
                    let mut child = self.child_vm();
                    let closure = closure.clone();
                    let item = item.clone();
                    futures.push(async move {
                        let result = child.call_closure(&closure, &[item]).await?;
                        Ok::<(VmValue, String), VmError>((
                            result,
                            std::mem::take(&mut child.output),
                        ))
                    });
                }
                let joined = run_capped_ordered(futures, cap, "Parallel map error").await?;
                let mut results = Vec::with_capacity(len);
                for entry in joined {
                    let (val, task_output) = entry?;
                    self.output.push_str(&task_output);
                    results.push(val);
                }
                self.stack.push(VmValue::List(Rc::new(results)));
            }
            _ => self.stack.push(VmValue::Nil),
        }
        Ok(())
    }

    pub(super) async fn execute_parallel_settle(&mut self) -> Result<(), VmError> {
        let closure = self.pop()?;
        let list_val = self.pop()?;
        let cap_val = self.pop()?;
        match (&list_val, &closure) {
            (VmValue::List(items), VmValue::Closure(closure)) => {
                let len = items.len();
                let cap = parallel_cap_from_value(&cap_val, len)?;
                let mut futures = Vec::with_capacity(len);
                for item in items.iter() {
                    let mut child = self.child_vm();
                    let closure = closure.clone();
                    let item = item.clone();
                    futures.push(async move {
                        let result = child.call_closure(&closure, &[item]).await;
                        let output = std::mem::take(&mut child.output);
                        (result, output)
                    });
                }
                let joined = run_capped_ordered(futures, cap, "Parallel settle error").await?;
                let mut results = Vec::with_capacity(len);
                let mut succeeded = 0i64;
                let mut failed = 0i64;
                for (result, task_output) in joined {
                    self.output.push_str(&task_output);
                    match result {
                        Ok(val) => {
                            succeeded += 1;
                            results.push(VmValue::enum_variant("Result", "Ok", vec![val]));
                        }
                        Err(e) => {
                            failed += 1;
                            results.push(VmValue::enum_variant(
                                "Result",
                                "Err",
                                vec![VmValue::String(Rc::from(e.to_string()))],
                            ));
                        }
                    }
                }
                let mut dict = BTreeMap::new();
                dict.insert("results".to_string(), VmValue::List(Rc::new(results)));
                dict.insert("succeeded".to_string(), VmValue::Int(succeeded));
                dict.insert("failed".to_string(), VmValue::Int(failed));
                self.stack.push(VmValue::Dict(Rc::new(dict)));
            }
            _ => self.stack.push(VmValue::Nil),
        }
        Ok(())
    }

    pub(super) fn execute_spawn(&mut self) -> Result<(), VmError> {
        let _spawn_span =
            super::super::ScopeSpan::new(crate::tracing::SpanKind::Spawn, "spawn".into());
        let closure = self.pop()?;
        if let VmValue::Closure(closure) = closure {
            self.task_counter += 1;
            let task_id = format!("vm_task_{}", self.task_counter);
            let mut child = self.child_vm();
            let cancel_token = Arc::new(std::sync::atomic::AtomicBool::new(false));
            child.cancel_token = Some(cancel_token.clone());
            let handle = tokio::task::spawn_local(async move {
                let result = child.call_closure(&closure, &[]).await?;
                Ok((result, std::mem::take(&mut child.output)))
            });
            self.spawned_tasks.insert(
                task_id.clone(),
                VmTaskHandle {
                    handle,
                    cancel_token,
                },
            );
            self.stack.push(VmValue::TaskHandle(task_id));
        } else {
            self.stack.push(VmValue::Nil);
        }
        Ok(())
    }

    pub(super) fn execute_deadline_setup(&mut self) -> Result<(), VmError> {
        let dur_val = self.pop()?;
        let ms = match &dur_val {
            VmValue::Duration(ms) => *ms,
            VmValue::Int(n) => (*n).max(0) as u64,
            _ => 30_000,
        };
        let deadline = Instant::now() + std::time::Duration::from_millis(ms);
        self.deadlines.push((deadline, self.frames.len()));
        Ok(())
    }

    pub(super) fn execute_deadline_end(&mut self) {
        self.deadlines.pop();
    }
}
