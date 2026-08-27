use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::value::{DeadlockError, VmError, VmStream, VmStreamCancel, VmTaskHandle, VmValue};
use crate::wait_for_graph::VmWaitForGraph;

use super::super::CallArgs;
use crate::vm::subtask;

/// Human-readable rendering of a `mutex(resource)` key for diagnostics
/// (e.g. the HARN-ORC-011 deadlock message). Scalars render as themselves;
/// anything structural falls back to the stable structural key.
fn mutex_resource_display(v: &VmValue) -> String {
    match v {
        VmValue::String(s) => s.to_string(),
        VmValue::Int(n) => n.to_string(),
        VmValue::Bool(b) => b.to_string(),
        _ => crate::value::value_structural_hash_key(v),
    }
}

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

/// Cancels every inline child when the fan-out future is dropped (host
/// cancellation) and when fail-fast observes a terminal branch error. The
/// process wait loop and other blocking builtins observe this token from their
/// own worker threads, so dropping a LocalSet future cannot strand a child.
struct ParallelCancelGuard(Arc<std::sync::atomic::AtomicBool>);

impl Drop for ParallelCancelGuard {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

fn retain_lower_pre_cleanup_error(selected: &mut (usize, VmError), observed: (usize, VmError)) {
    if observed.0 < selected.0 {
        *selected = observed;
    }
}

/// Run `futures` concurrently, capped to at most `cap` in-flight tasks
/// at any moment (or unlimited when `cap` is `None`). Results come back
/// in source order so callers can index by original position. A single
/// join error fails the whole batch, mirroring the pre-cap behavior of
/// the `Parallel*` opcodes.
///
/// This is the DRAINING executor: every branch runs to completion even
/// when a sibling has already failed. It backs `parallel settle`, the
/// run-everything form. `parallel` / `parallel each` use the fail-fast
/// [`run_capped_ordered_fail_fast`] instead.
async fn run_capped_ordered<F, T>(
    futures: Vec<subtask::PreparedSubtask<F>>,
    cap: Option<usize>,
    wait_for_graph: Arc<VmWaitForGraph>,
    task_ids: Vec<String>,
    cancel_token: Arc<std::sync::atomic::AtomicBool>,
    error_label: &'static str,
) -> Result<Vec<T>, VmError>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let _cancel_guard = ParallelCancelGuard(cancel_token);
    let total = futures.len();
    if total == 0 {
        return Ok(Vec::new());
    }
    let mut results: Vec<Option<T>> = (0..total).map(|_| None).collect();
    let slot = cap.unwrap_or(total).max(1).min(total);
    assert_eq!(total, task_ids.len(), "parallel task metadata drift");
    let mut pending: VecDeque<(usize, subtask::PreparedSubtask<F>, String)> = futures
        .into_iter()
        .zip(task_ids)
        .enumerate()
        .map(|(index, (future, task_id))| (index, future, task_id))
        .collect();
    let mut join_set = tokio::task::JoinSet::new();

    let mut admitted = Vec::with_capacity(slot);
    while admitted.len() < slot {
        let Some((index, future, task_id)) = pending.pop_front() else {
            break;
        };
        admitted.push((index, future, wait_for_graph.register_task(task_id)));
    }
    for (index, future, activity) in admitted {
        subtask::spawn_into(
            &mut join_set,
            future.map_output(move |value| (index, value, activity)),
        );
    }

    while let Some(joined) = join_set.join_next().await {
        let (index, value, completed_activity) =
            joined.map_err(|e| VmError::Runtime(format!("{error_label}: {e}")))?;
        results[index] = Some(value);
        if let Some((next_index, future, task_id)) = pending.pop_front() {
            let next_activity = wait_for_graph.register_task(task_id);
            subtask::spawn_into(
                &mut join_set,
                future.map_output(move |value| (next_index, value, next_activity)),
            );
        }
        drop(completed_activity);
    }

    Ok(results
        .into_iter()
        .map(|slot| slot.expect("run_capped_ordered: missing result slot"))
        .collect())
}

/// Fail-fast variant of [`run_capped_ordered`] backing `parallel` and
/// `parallel each`: each future resolves to `Result<T, VmError>`, and the
/// first branch failure cancels the whole fan-out. In-flight siblings are
/// aborted (their futures are dropped at the next await point, which kills
/// in-flight LLM/host calls and releases RAII sync permits), and queued
/// branches are never started.
///
/// Structured concurrency: after the abort, the join set is still drained
/// to completion, so no branch task outlives this call.
///
/// Error identity: aborting on the first completion-order failure would
/// report a nondeterministic branch when several siblings fail near-
/// simultaneously. Errors already waiting in the join set are snapshotted
/// before cleanup, and the LOWEST SOURCE INDEX among them is reported — the
/// same first-by-source-order convention `scope { }` exit uses. Results that
/// arrive after cleanup begins are ignored because cooperative cancellation
/// can turn an otherwise-runnable sibling into a synthetic host-cancel error.
///
/// On failure, buffered print output from sibling branches is discarded
/// (deterministic: the construct contributes only its error), matching how
/// `scope { }` drops output from cancelled siblings.
async fn run_capped_ordered_fail_fast<F, T>(
    futures: Vec<subtask::PreparedSubtask<F>>,
    cap: Option<usize>,
    wait_for_graph: Arc<VmWaitForGraph>,
    task_ids: Vec<String>,
    cancel_token: Arc<std::sync::atomic::AtomicBool>,
    error_label: &'static str,
) -> Result<Vec<T>, VmError>
where
    F: std::future::Future<Output = Result<T, VmError>> + Send + 'static,
    T: Send + 'static,
{
    let _cancel_guard = ParallelCancelGuard(Arc::clone(&cancel_token));
    let total = futures.len();
    if total == 0 {
        return Ok(Vec::new());
    }
    let mut results: Vec<Option<T>> = (0..total).map(|_| None).collect();
    let slot = cap.unwrap_or(total).max(1).min(total);
    assert_eq!(total, task_ids.len(), "parallel task metadata drift");
    let mut pending: VecDeque<(usize, subtask::PreparedSubtask<F>, String)> = futures
        .into_iter()
        .zip(task_ids)
        .enumerate()
        .map(|(index, (future, task_id))| (index, future, task_id))
        .collect();
    let mut join_set = tokio::task::JoinSet::new();

    let mut admitted = Vec::with_capacity(slot);
    while admitted.len() < slot {
        let Some((index, future, task_id)) = pending.pop_front() else {
            break;
        };
        admitted.push((index, future, wait_for_graph.register_task(task_id)));
    }
    for (index, future, activity) in admitted {
        subtask::spawn_into(
            &mut join_set,
            future.map_output(move |value| (index, value, activity)),
        );
    }

    while let Some(joined) = join_set.join_next().await {
        let observed = match joined {
            Ok((index, Ok(value), completed_activity)) => {
                results[index] = Some(value);
                if let Some((next_index, future, task_id)) = pending.pop_front() {
                    let next_activity = wait_for_graph.register_task(task_id);
                    subtask::spawn_into(
                        &mut join_set,
                        future.map_output(move |value| (next_index, value, next_activity)),
                    );
                }
                drop(completed_activity);
                continue;
            }
            Ok((index, Err(error), _activity)) => (index, error),
            Err(join_error) => {
                if join_error.is_cancelled() {
                    // A sibling this loop aborted below; already accounted for.
                    continue;
                }
                // Panic in a branch task: the index is lost with the payload.
                // Rank it after every indexed error so a real branch error
                // wins the deterministic pick.
                (
                    usize::MAX,
                    VmError::Runtime(format!("{error_label}: {join_error}")),
                )
            }
        };
        let mut first_error = observed;

        // Snapshot only errors that reached the join set before cleanup began.
        // Once cancellation is requested, sibling failures can be artifacts of
        // that cleanup and must not replace the initiating semantic error.
        while let Some(joined) = join_set.try_join_next() {
            let candidate = match joined {
                Ok((index, Err(error), _activity)) => Some((index, error)),
                Ok((_index, Ok(_value), _activity)) => None,
                Err(join_error) if join_error.is_cancelled() => None,
                Err(join_error) => Some((
                    usize::MAX,
                    VmError::Runtime(format!("{error_label}: {join_error}")),
                )),
            };
            if let Some(candidate) = candidate {
                retain_lower_pre_cleanup_error(&mut first_error, candidate);
            }
        }
        cancel_token.store(true, std::sync::atomic::Ordering::SeqCst);
        // Give cooperatively-cancelled branches one scheduler turn to unwind
        // before hard-aborting anything that remains. Their cleanup results
        // are drained below but cannot replace the initiating error.
        tokio::task::yield_now().await;
        join_set.abort_all();
        pending.clear();
        while join_set.join_next().await.is_some() {}
        return Err(first_error.1);
    }

    Ok(results
        .into_iter()
        .map(|slot| slot.expect("run_capped_ordered_fail_fast: missing result slot"))
        .collect())
}

async fn stream_capped_unordered<F, T>(
    futures: Vec<subtask::PreparedSubtask<F>>,
    cap: Option<usize>,
    sender: tokio::sync::mpsc::Sender<Result<T, VmError>>,
    mut cancel_rx: tokio::sync::watch::Receiver<bool>,
    cancel_token: Arc<std::sync::atomic::AtomicBool>,
    error_label: &'static str,
) where
    F: std::future::Future<Output = Result<T, VmError>> + Send + 'static,
    T: Send + 'static,
{
    let _cancel_guard = ParallelCancelGuard(Arc::clone(&cancel_token));
    let total = futures.len();
    if total == 0 {
        return;
    }
    let slot = cap.unwrap_or(total).max(1).min(total);
    let mut pending: VecDeque<subtask::PreparedSubtask<F>> = futures.into_iter().collect();
    let mut join_set: tokio::task::JoinSet<Result<T, VmError>> = tokio::task::JoinSet::new();

    while join_set.len() < slot {
        let Some(fut) = pending.pop_front() else {
            break;
        };
        subtask::spawn_into(&mut join_set, fut);
    }

    loop {
        if *cancel_rx.borrow() {
            cancel_token.store(true, std::sync::atomic::Ordering::SeqCst);
            join_set.abort_all();
            return;
        }
        if join_set.is_empty() {
            return;
        }
        let joined = tokio::select! {
            _ = cancel_rx.changed() => {
                cancel_token.store(true, std::sync::atomic::Ordering::SeqCst);
                join_set.abort_all();
                return;
            }
            joined = join_set.join_next() => joined,
        };
        let Some(joined) = joined else {
            return;
        };
        let value = match joined {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(error),
            Err(error) => Err(VmError::Runtime(format!("{error_label}: {error}"))),
        };
        let should_stop = value.is_err();
        let send_result = tokio::select! {
            _ = cancel_rx.changed() => {
                cancel_token.store(true, std::sync::atomic::Ordering::SeqCst);
                join_set.abort_all();
                return;
            }
            result = sender.send(value) => result,
        };
        if send_result.is_err() || should_stop {
            cancel_token.store(true, std::sync::atomic::Ordering::SeqCst);
            join_set.abort_all();
            return;
        }
        if let Some(fut) = pending.pop_front() {
            subtask::spawn_into(&mut join_set, fut);
        }
    }
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
            self.runtime_context_counter += 1;
            let task_group_id = format!(
                "{}:parallel:{}",
                self.runtime_context.task_id, self.runtime_context_counter
            );
            let mut futures: Vec<_> = Vec::with_capacity(count);
            let mut task_ids = Vec::with_capacity(count);
            let cancel_token = Arc::new(std::sync::atomic::AtomicBool::new(false));
            for i in 0..count {
                let task_id = format!("{task_group_id}:{i}");
                let mut child = self.child_vm();
                child.cancel_token = Some(Arc::clone(&cancel_token));
                child.runtime_context = self.runtime_context.child_task(
                    task_id.clone(),
                    "parallel",
                    Some(task_group_id.clone()),
                );
                task_ids.push(task_id);
                let registry = child.pool_registry.clone();
                let closure = closure.clone();
                futures.push(subtask::prepare(registry, async move {
                    let arg = VmValue::Int(i as i64);
                    let result = child
                        .call_closure_args(&closure, CallArgs::One(&arg))
                        .await?;
                    Ok::<(VmValue, String), VmError>((result, std::mem::take(&mut child.output)))
                }));
            }
            let _wait = self
                .wait_for_graph
                .wait_for_tasks(&self.runtime_context.task_id, task_ids.clone())?;
            // Fail fast: the first branch error aborts in-flight siblings and
            // skips queued branches; only the settle form drains everything.
            let joined = run_capped_ordered_fail_fast(
                futures,
                cap,
                Arc::clone(&self.wait_for_graph),
                task_ids,
                cancel_token,
                "Parallel task error",
            )
            .await?;
            let mut results = Vec::with_capacity(count);
            for (val, task_output) in joined {
                self.output.push_str(&task_output);
                results.push(val);
            }
            self.stack.push(VmValue::List(std::sync::Arc::new(results)));
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
                let cancel_token = Arc::new(std::sync::atomic::AtomicBool::new(false));
                self.runtime_context_counter += 1;
                let task_group_id = format!(
                    "{}:parallel_each:{}",
                    self.runtime_context.task_id, self.runtime_context_counter
                );
                let mut futures = Vec::with_capacity(len);
                let mut task_ids = Vec::with_capacity(len);
                for (i, item) in items.iter().enumerate() {
                    let task_id = format!("{task_group_id}:{i}");
                    let mut child = self.child_vm();
                    child.cancel_token = Some(Arc::clone(&cancel_token));
                    child.runtime_context = self.runtime_context.child_task(
                        task_id.clone(),
                        "parallel each",
                        Some(task_group_id.clone()),
                    );
                    task_ids.push(task_id);
                    let registry = child.pool_registry.clone();
                    let closure = closure.clone();
                    let item = item.clone();
                    futures.push(subtask::prepare(registry, async move {
                        let result = child
                            .call_closure_args(&closure, CallArgs::One(&item))
                            .await?;
                        Ok::<(VmValue, String), VmError>((
                            result,
                            std::mem::take(&mut child.output),
                        ))
                    }));
                }
                let _wait = self
                    .wait_for_graph
                    .wait_for_tasks(&self.runtime_context.task_id, task_ids.clone())?;
                // Fail fast, matching `parallel`: use `parallel settle` when
                // every branch must run regardless of failures.
                let joined = run_capped_ordered_fail_fast(
                    futures,
                    cap,
                    Arc::clone(&self.wait_for_graph),
                    task_ids,
                    cancel_token,
                    "Parallel map error",
                )
                .await?;
                let mut results = Vec::with_capacity(len);
                for (val, task_output) in joined {
                    self.output.push_str(&task_output);
                    results.push(val);
                }
                self.stack.push(VmValue::List(std::sync::Arc::new(results)));
            }
            _ => self.stack.push(VmValue::Nil),
        }
        Ok(())
    }

    pub(super) async fn execute_parallel_map_stream(&mut self) -> Result<(), VmError> {
        let closure = self.pop()?;
        let list_val = self.pop()?;
        let cap_val = self.pop()?;
        match (&list_val, &closure) {
            (VmValue::List(items), VmValue::Closure(closure)) => {
                let len = items.len();
                let cap = parallel_cap_from_value(&cap_val, len)?;
                let cancel_token = Arc::new(std::sync::atomic::AtomicBool::new(false));
                self.runtime_context_counter += 1;
                let task_group_id = format!(
                    "{}:parallel_each_stream:{}",
                    self.runtime_context.task_id, self.runtime_context_counter
                );
                let mut futures = Vec::with_capacity(len);
                for (i, item) in items.iter().enumerate() {
                    let mut child = self.child_vm();
                    child.cancel_token = Some(Arc::clone(&cancel_token));
                    child.runtime_context = self.runtime_context.child_task(
                        format!("{task_group_id}:{i}"),
                        "parallel each as stream",
                        Some(task_group_id.clone()),
                    );
                    let registry = child.pool_registry.clone();
                    let closure = closure.clone();
                    let item = item.clone();
                    futures.push(subtask::prepare(registry, async move {
                        child
                            .call_closure_args(&closure, CallArgs::One(&item))
                            .await
                    }));
                }

                let (tx, rx) = tokio::sync::mpsc::channel::<Result<VmValue, VmError>>(1);
                let cancel = VmStreamCancel::new();
                let registry = self.pool_registry.clone();
                subtask::spawn(subtask::prepare(
                    registry,
                    stream_capped_unordered(
                        futures,
                        cap,
                        tx,
                        cancel.subscribe(),
                        cancel_token,
                        "Parallel map stream error",
                    ),
                ));
                self.stack.push(VmValue::stream(VmStream {
                    done: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    receiver: Arc::new(tokio::sync::Mutex::new(rx)),
                    cancel: Some(cancel),
                }));
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
                let cancel_token = Arc::new(std::sync::atomic::AtomicBool::new(false));
                self.runtime_context_counter += 1;
                let task_group_id = format!(
                    "{}:parallel_settle:{}",
                    self.runtime_context.task_id, self.runtime_context_counter
                );
                let mut futures = Vec::with_capacity(len);
                let mut task_ids = Vec::with_capacity(len);
                for (i, item) in items.iter().enumerate() {
                    let task_id = format!("{task_group_id}:{i}");
                    let mut child = self.child_vm();
                    child.cancel_token = Some(Arc::clone(&cancel_token));
                    child.runtime_context = self.runtime_context.child_task(
                        task_id.clone(),
                        "parallel settle",
                        Some(task_group_id.clone()),
                    );
                    task_ids.push(task_id);
                    let registry = child.pool_registry.clone();
                    let closure = closure.clone();
                    let item = item.clone();
                    futures.push(subtask::prepare(registry, async move {
                        let result = child
                            .call_closure_args(&closure, CallArgs::One(&item))
                            .await;
                        let output = std::mem::take(&mut child.output);
                        (result, output)
                    }));
                }
                let _wait = self
                    .wait_for_graph
                    .wait_for_tasks(&self.runtime_context.task_id, task_ids.clone())?;
                let joined = run_capped_ordered(
                    futures,
                    cap,
                    Arc::clone(&self.wait_for_graph),
                    task_ids,
                    cancel_token,
                    "Parallel settle error",
                )
                .await?;
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
                            // `parallel settle` normally makes branch errors
                            // available as Result.Err values. VM-wide control
                            // flow is different: lowering it would let a Harn
                            // program catch and ignore an explicit exit.
                            if e.is_uncatchable_control_flow() {
                                return Err(e);
                            }
                            failed += 1;
                            // Preserve the structured error value (matching
                            // `try`/`catch` via `handle_error`) so a categorized
                            // error thrown in a settle branch keeps its category
                            // instead of collapsing to a `to_string()` blob.
                            results.push(VmValue::enum_variant(
                                "Result",
                                "Err",
                                vec![e.thrown_value()],
                            ));
                        }
                    }
                }
                let mut dict = BTreeMap::new();
                dict.insert(
                    "results".to_string(),
                    VmValue::List(std::sync::Arc::new(results)),
                );
                dict.insert("succeeded".to_string(), VmValue::Int(succeeded));
                dict.insert("failed".to_string(), VmValue::Int(failed));
                self.stack.push(VmValue::dict(dict));
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
            let runtime_task_id = format!(
                "{}:spawn:{}",
                self.runtime_context.task_id, self.task_counter
            );
            let mut child = self.child_vm();
            child.runtime_context =
                self.runtime_context
                    .child_task(runtime_task_id.clone(), "spawn", None);
            let cancel_token = Arc::new(std::sync::atomic::AtomicBool::new(false));
            child.cancel_token = Some(cancel_token.clone());
            let registry = child.pool_registry.clone();
            let scheduled_activity = self.wait_for_graph.register_task(runtime_task_id.clone());
            let handle = subtask::spawn_child(registry, async move {
                let _scheduled_activity = scheduled_activity;
                let result = child.call_closure_args(&closure, CallArgs::Empty).await?;
                Ok((result, std::mem::take(&mut child.output)))
            });
            self.spawned_tasks.insert(
                task_id.clone(),
                VmTaskHandle {
                    handle,
                    cancel_token,
                    wait_task_id: runtime_task_id,
                },
            );
            // Structured concurrency: bind this task to the innermost open
            // `scope { }` so it is joined (and its error propagated) at scope
            // exit. A bare `spawn` with no enclosing scope stays detached
            // (backward-compatible) and is cancelled at VM drop.
            if let Some(scope) = self.task_scopes.last_mut() {
                scope.task_ids.push(task_id.clone());
            }
            self.stack.push(VmValue::task_handle(task_id));
        } else {
            self.stack.push(VmValue::Nil);
        }
        Ok(())
    }

    /// `TaskScopeEnter`: open a structured-concurrency nursery.
    pub(super) fn execute_task_scope_enter(&mut self) {
        self.task_scopes.push(super::super::TaskScope {
            task_ids: Vec::new(),
            frame_depth: self.frames.len(),
            env_scope_depth: self.env.scope_depth(),
        });
    }

    /// `TaskScopeExit`: close the innermost nursery — join every task still
    /// bound to it. The first task error cancels the remaining siblings and
    /// propagates out of the `scope { }` block.
    pub(super) async fn execute_task_scope_exit(&mut self) -> Result<(), VmError> {
        let Some(scope) = self.task_scopes.pop() else {
            return Ok(());
        };
        let mut first_error: Option<VmError> = None;
        for id in &scope.task_ids {
            let Some(task) = self.spawned_tasks.remove(id) else {
                continue; // already awaited / cancelled
            };
            if first_error.is_some() {
                // A sibling already failed: cancel the rest without awaiting.
                task.cancel_token
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                task.handle.abort();
                continue;
            }
            match task.handle.await {
                Ok(Ok((_result, output))) => {
                    self.output.push_str(&output);
                }
                Ok(Err(e)) => first_error = Some(e),
                Err(join_err) => {
                    first_error = Some(VmError::Runtime(format!("Task join error: {join_err}")));
                }
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Bare `mutex { }`: acquire the lock keyed on this block's *lexical
    /// call-site*. The `(chunk identity, instruction pointer)` pair is stable
    /// across every execution of this site — including concurrent tasks that
    /// share the cloned chunk `Arc` — so re-entries of the same block still
    /// serialize, while two distinct `mutex {}` blocks no longer contend on one
    /// process-wide lock.
    pub(super) async fn execute_sync_mutex_enter(&mut self) -> Result<(), VmError> {
        let frame = self.frames.last().unwrap();
        let key = format!("@{:x}:{}", Arc::as_ptr(&frame.chunk) as usize, frame.ip);
        self.sync_mutex_acquire_lexical(key, "<anonymous mutex block>".to_string())
            .await
    }

    /// `mutex(resource) { }`: acquire the lock keyed on the resource's
    /// structural value, so every block naming the same resource mutually
    /// excludes regardless of where it appears in the source.
    pub(super) async fn execute_sync_mutex_enter_keyed(&mut self) -> Result<(), VmError> {
        let resource = self.pop()?;
        let key = format!("v:{}", crate::value::value_structural_hash_key(&resource));
        let display = mutex_resource_display(&resource);
        self.sync_mutex_acquire_lexical(key, display).await
    }

    // Shared acquire path for both lexical `mutex` forms.
    //
    // Runtime self-deadlock guard: a lexical `mutex` block acquires a
    // capacity-1 semaphore with no timeout. If this VM already holds a permit
    // for the same `kind:key`, the acquire can never be granted (the sole
    // permit holder IS the requester, and with no timeout it blocks forever)
    // — a provably-unresolvable self-deadlock with zero false positives.
    async fn sync_mutex_acquire_lexical(
        &mut self,
        key: String,
        display: String,
    ) -> Result<(), VmError> {
        if self.held_permits_for("mutex", &key) >= 1 {
            return Err(VmError::Deadlock(Box::new(DeadlockError::self_deadlock(
                "mutex",
                display,
                "re-entrant acquire of a non-reentrant mutex already held by this task",
            ))));
        }
        let permit = self
            .sync_runtime
            .acquire("mutex", &key, 1, 1, None, self.cancel_token.clone())
            .await?
            .ok_or_else(|| VmError::Runtime(format!("mutex '{display}' timed out")))?;
        self.held_sync_guards
            .push(crate::synchronization::VmSyncHeldGuard {
                _permit: permit,
                frame_depth: self.frames.len(),
                env_scope_depth: self.env.scope_depth(),
            });
        Ok(())
    }

    pub(super) fn execute_deadline_setup(&mut self) -> Result<(), VmError> {
        let dur_val = self.pop()?;
        let ms = match &dur_val {
            VmValue::Duration(ms) => (*ms).max(0) as u64,
            VmValue::Int(n) => (*n).max(0) as u64,
            _ => 30_000,
        };
        self.push_deadline_after(Duration::from_millis(ms));
        Ok(())
    }

    pub(crate) fn push_deadline_after(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        self.deadlines.push((deadline, self.frames.len()));
    }

    pub(super) fn execute_deadline_end(&mut self) {
        self.deadlines.pop();
    }
}

#[cfg(test)]
mod scheduler_tests {
    use std::future::Future;
    use std::pin::Pin;

    use super::*;
    use crate::value::{VmChannelCloseState, VmChannelHandle};
    use crate::wait_for_graph::{channel_target, VmWaitForGraph};

    type Branch = Pin<Box<dyn Future<Output = Result<(), VmError>> + Send>>;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn admitted_lower_index_remains_visible_when_higher_index_starts_first() {
        let graph = Arc::new(VmWaitForGraph::new());
        let _root_activity = graph.register_task("root");
        let _root_wait = graph
            .wait_for_tasks("root", ["child:0".to_string(), "child:1".to_string()])
            .expect("scheduled children can still make progress");
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let channel = VmChannelHandle {
            name: Arc::from("empty"),
            sender: Arc::new(sender),
            receiver: Arc::new(tokio::sync::Mutex::new(receiver)),
            close: Arc::new(VmChannelCloseState::open()),
        };
        let target = channel_target(&channel);
        let (release_first, first_released) = tokio::sync::oneshot::channel();

        let first: Branch = Box::pin(async move {
            first_released
                .await
                .map_err(|error| VmError::Runtime(error.to_string()))?;
            Ok(())
        });
        let second_graph = Arc::clone(&graph);
        let second: Branch = Box::pin(async move {
            let _started_activity = second_graph.register_task("child:1");
            let wait = second_graph.wait_for_channel_receive("child:1", vec![target]);
            let _ = release_first.send(());
            let _wait = wait?;
            Ok(())
        });
        let registry = crate::stdlib::pool::new_pool_registry();
        let futures = vec![
            subtask::prepare(Arc::clone(&registry), first),
            subtask::prepare(registry, second),
        ];

        run_capped_ordered_fail_fast(
            futures,
            None,
            Arc::clone(&graph),
            vec!["child:0".to_string(), "child:1".to_string()],
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            "test parallel error",
        )
        .await
        .expect("the admitted lower-index branch is runnable even before its body starts");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fail_fast_keeps_initiating_deadlock_over_cleanup_cancellation() {
        let cancel_token = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let lower_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let lower_token = Arc::clone(&cancel_token);
        let lower_signal = Arc::clone(&lower_started);
        let lower: Branch = Box::pin(async move {
            lower_signal.store(true, std::sync::atomic::Ordering::SeqCst);
            while !lower_token.load(std::sync::atomic::Ordering::SeqCst) {
                std::hint::spin_loop();
            }
            Err(crate::vm::Vm::cancelled_error())
        });
        let higher_signal = Arc::clone(&lower_started);
        let higher: Branch = Box::pin(async move {
            while !higher_signal.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            Err(VmError::Deadlock(Box::new(DeadlockError::wait_for_graph(
                "channel",
                "empty",
                "initiating semantic error",
            ))))
        });
        let registry = crate::stdlib::pool::new_pool_registry();
        let graph = Arc::new(VmWaitForGraph::new());
        let error = run_capped_ordered_fail_fast(
            vec![
                subtask::prepare(Arc::clone(&registry), lower),
                subtask::prepare(registry, higher),
            ],
            None,
            graph,
            vec!["child:0".to_string(), "child:1".to_string()],
            cancel_token,
            "test parallel error",
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("HARN-ORC-012"));
    }
}
