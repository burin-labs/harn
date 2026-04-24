use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;

use crate::event_log::{active_event_log, EventLog, LogEvent, Topic};
use crate::value::{VmClosure, VmError, VmValue};
use crate::vm::Vm;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RestartMode {
    Never,
    OnFailure,
    Always,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Strategy {
    OneForOne,
    OneForAll,
    RestForOne,
    EscalateToParent,
}

#[derive(Clone, Debug)]
struct RestartPolicy {
    mode: RestartMode,
    max_restarts: Option<usize>,
    window_ms: u64,
    backoff_ms: u64,
    max_backoff_ms: u64,
    factor: f64,
    jitter_ms: u64,
    circuit_open_ms: Option<u64>,
}

#[derive(Clone)]
struct ChildSpec {
    name: String,
    kind: String,
    task: Rc<VmClosure>,
    restart: RestartPolicy,
    active_lease: Option<String>,
}

struct ChildSlot {
    spec: ChildSpec,
    status: String,
    restart_count: usize,
    generation: u64,
    failures: VecDeque<SystemTime>,
    last_error: Option<String>,
    current_wait_reason: Option<String>,
    next_restart_time_ms: Option<i64>,
    cancel_token: Option<Arc<AtomicBool>>,
    join: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Clone, Debug)]
struct SupervisorEvent {
    seq: i64,
    at_ms: i64,
    supervisor_id: String,
    child: Option<String>,
    kind: String,
    message: Option<String>,
}

struct SupervisorState {
    id: String,
    name: String,
    strategy: Strategy,
    shutdown_ms: u64,
    status: String,
    stop_requested: bool,
    children: Vec<ChildSlot>,
    events: Vec<SupervisorEvent>,
    next_seq: i64,
    started_count: i64,
    stopped_count: i64,
    failed_count: i64,
    restarted_count: i64,
    suppressed_count: i64,
    escalated_count: i64,
    supervisor_join: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Clone)]
struct SupervisorHandle {
    state: Rc<RefCell<SupervisorState>>,
    _tx: mpsc::UnboundedSender<SupervisorMsg>,
}

enum SupervisorMsg {
    ChildExited {
        index: usize,
        generation: u64,
        result: Result<VmValue, String>,
        output: String,
    },
    RestartDue {
        index: usize,
        generation: u64,
    },
}

thread_local! {
    static SUPERVISORS: RefCell<BTreeMap<String, SupervisorHandle>> = const { RefCell::new(BTreeMap::new()) };
    static SUPERVISOR_COUNTER: RefCell<u64> = const { RefCell::new(0) };
}

pub(crate) fn register_supervisor_builtins(vm: &mut Vm) {
    vm.register_async_builtin("supervisor_start", |args| async move {
        let base_vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
            VmError::Runtime("supervisor_start: requires VM execution context".to_string())
        })?;
        let spec = args
            .first()
            .ok_or_else(|| VmError::Runtime("supervisor_start: spec is required".to_string()))?;
        let handle = start_supervisor(base_vm, spec)?;
        let value = supervisor_handle_value(&handle.state.borrow());
        Ok(value)
    });

    vm.register_builtin("supervisor_state", |args, _out| {
        let state = supervisor_from_args(args, "supervisor_state")?;
        let value = supervisor_state_value(&state.borrow());
        Ok(value)
    });

    vm.register_builtin("supervisor_events", |args, _out| {
        let state = supervisor_from_args(args, "supervisor_events")?;
        let value = events_value(&state.borrow());
        Ok(value)
    });

    vm.register_builtin("supervisor_metrics", |args, _out| {
        let state = supervisor_from_args(args, "supervisor_metrics")?;
        let value = metrics_value(&state.borrow());
        Ok(value)
    });

    vm.register_async_builtin("supervisor_stop", |args| async move {
        let id = supervisor_id_from_args(&args, "supervisor_stop")?;
        let timeout_ms = args.get(1).and_then(duration_ms).unwrap_or_else(|| {
            supervisor_lookup(&id).map_or(5000, |h| h.state.borrow().shutdown_ms)
        });
        let handle = supervisor_lookup(&id).ok_or_else(|| {
            VmError::Runtime(format!("supervisor_stop: unknown supervisor '{id}'"))
        })?;

        {
            let mut state = handle.state.borrow_mut();
            if state.status != "stopped" {
                state.status = "draining".to_string();
                state.stop_requested = true;
                push_event(&mut state, None, "supervisor_stopping", None);
                for child in &mut state.children {
                    if child.status == "running" {
                        child.status = "draining".to_string();
                        child.current_wait_reason = Some("shutdown".to_string());
                    }
                    if let Some(token) = &child.cancel_token {
                        token.store(true, Ordering::SeqCst);
                    }
                }
            }
        }

        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            if handle
                .state
                .borrow()
                .children
                .iter()
                .all(|child| !matches!(child.status.as_str(), "running" | "draining"))
            {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        {
            let mut state = handle.state.borrow_mut();
            let mut forced = false;
            for idx in 0..state.children.len() {
                let child = &mut state.children[idx];
                if matches!(
                    child.status.as_str(),
                    "running" | "draining" | "waiting" | "circuit_open"
                ) {
                    child.generation = child.generation.saturating_add(1);
                    if let Some(join) = child.join.take() {
                        join.abort();
                    }
                    child.cancel_token = None;
                    child.status = "stopped".to_string();
                    child.current_wait_reason = None;
                    child.next_restart_time_ms = None;
                    forced = true;
                    let name = child.spec.name.clone();
                    push_event(
                        &mut state,
                        Some(name),
                        "child_stopped",
                        Some("supervisor stop".to_string()),
                    );
                }
            }
            state.status = "stopped".to_string();
            push_event(
                &mut state,
                None,
                "supervisor_stopped",
                Some(if forced { "forced" } else { "drained" }.to_string()),
            );
        }

        if let Some(join) = handle.state.borrow_mut().supervisor_join.take() {
            join.abort();
        }

        let value = supervisor_state_value(&handle.state.borrow());
        Ok(value)
    });
}

fn start_supervisor(base_vm: Vm, spec_value: &VmValue) -> Result<SupervisorHandle, VmError> {
    let spec = require_dict(spec_value, "supervisor_start")?;
    let id = spec
        .get("id")
        .map(VmValue::display)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(next_supervisor_id);
    let name = spec
        .get("name")
        .map(VmValue::display)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| id.clone());
    let strategy = parse_strategy(spec.get("strategy"))?;
    let shutdown_ms = spec
        .get("shutdown_ms")
        .or_else(|| spec.get("drain_ms"))
        .and_then(duration_ms)
        .unwrap_or(5000);
    let children = parse_children(spec.get("children"), spec.get("restart"))?;
    if children.is_empty() {
        return Err(VmError::Runtime(
            "supervisor_start: children must include at least one child".to_string(),
        ));
    }

    if supervisor_lookup(&id).is_some() {
        return Err(VmError::Runtime(format!(
            "supervisor_start: supervisor '{id}' already exists"
        )));
    }

    let slots = children
        .into_iter()
        .map(|spec| ChildSlot {
            spec,
            status: "pending".to_string(),
            restart_count: 0,
            generation: 0,
            failures: VecDeque::new(),
            last_error: None,
            current_wait_reason: None,
            next_restart_time_ms: None,
            cancel_token: None,
            join: None,
        })
        .collect();

    let state = Rc::new(RefCell::new(SupervisorState {
        id,
        name,
        strategy,
        shutdown_ms,
        status: "running".to_string(),
        stop_requested: false,
        children: slots,
        events: Vec::new(),
        next_seq: 0,
        started_count: 0,
        stopped_count: 0,
        failed_count: 0,
        restarted_count: 0,
        suppressed_count: 0,
        escalated_count: 0,
        supervisor_join: None,
    }));

    let (tx, rx) = mpsc::unbounded_channel();
    let handle = SupervisorHandle {
        state: state.clone(),
        _tx: tx.clone(),
    };
    SUPERVISORS.with(|registry| {
        registry
            .borrow_mut()
            .insert(state.borrow().id.clone(), handle.clone());
    });

    let supervisor_join =
        tokio::task::spawn_local(supervisor_loop(state.clone(), tx, rx, Rc::new(base_vm)));
    state.borrow_mut().supervisor_join = Some(supervisor_join);
    Ok(handle)
}

async fn supervisor_loop(
    state: Rc<RefCell<SupervisorState>>,
    tx: mpsc::UnboundedSender<SupervisorMsg>,
    mut rx: mpsc::UnboundedReceiver<SupervisorMsg>,
    base_vm: Rc<Vm>,
) {
    {
        let mut state_ref = state.borrow_mut();
        push_event(&mut state_ref, None, "supervisor_started", None);
    }
    let len = state.borrow().children.len();
    for index in 0..len {
        spawn_child(state.clone(), tx.clone(), base_vm.clone(), index, false);
    }

    while let Some(message) = rx.recv().await {
        match message {
            SupervisorMsg::ChildExited {
                index,
                generation,
                result,
                output,
            } => {
                if !output.is_empty() {
                    let mut state_ref = state.borrow_mut();
                    let name = state_ref
                        .children
                        .get(index)
                        .map(|child| child.spec.name.clone());
                    push_event(&mut state_ref, name, "child_output", Some(output));
                }
                handle_child_exit(
                    state.clone(),
                    tx.clone(),
                    base_vm.clone(),
                    index,
                    generation,
                    result,
                );
            }
            SupervisorMsg::RestartDue { index, generation } => {
                let should_spawn = {
                    let state_ref = state.borrow();
                    state_ref.children.get(index).is_some_and(|child| {
                        child.generation == generation && !state_ref.stop_requested
                    })
                };
                if should_spawn {
                    spawn_child(state.clone(), tx.clone(), base_vm.clone(), index, true);
                }
            }
        }

        if supervisor_terminal(&state.borrow()) {
            let mut state_ref = state.borrow_mut();
            if state_ref.status == "running" {
                state_ref.status = "completed".to_string();
                push_event(&mut state_ref, None, "supervisor_completed", None);
            }
            break;
        }
    }
}

fn handle_child_exit(
    state: Rc<RefCell<SupervisorState>>,
    tx: mpsc::UnboundedSender<SupervisorMsg>,
    base_vm: Rc<Vm>,
    index: usize,
    generation: u64,
    result: Result<VmValue, String>,
) {
    let failed = result.is_err();
    let mut restart_plan = Vec::new();

    {
        let mut state_ref = state.borrow_mut();
        if index >= state_ref.children.len() || state_ref.children[index].generation != generation {
            return;
        }

        let child_name = state_ref.children[index].spec.name.clone();
        let stop_requested = state_ref.stop_requested;
        {
            let child = &mut state_ref.children[index];
            child.join = None;
            child.cancel_token = None;
            child.next_restart_time_ms = None;
            child.current_wait_reason = None;
            match result {
                Ok(_) => {
                    child.status = "stopped".to_string();
                    child.last_error = None;
                }
                Err(error) => {
                    child.status = "failed".to_string();
                    child.last_error = Some(error.clone());
                    child.failures.push_back(SystemTime::now());
                    state_ref.failed_count += 1;
                    push_event(
                        &mut state_ref,
                        Some(child_name.clone()),
                        "child_failed",
                        Some(error),
                    );
                }
            }
        }

        if !failed {
            state_ref.stopped_count += 1;
            push_event(
                &mut state_ref,
                Some(child_name.clone()),
                "child_stopped",
                None,
            );
        }

        if stop_requested {
            if state_ref
                .children
                .iter()
                .all(|child| !matches!(child.status.as_str(), "running" | "draining"))
            {
                state_ref.status = "stopped".to_string();
                push_event(
                    &mut state_ref,
                    None,
                    "supervisor_stopped",
                    Some("drained".to_string()),
                );
            }
            return;
        }

        if failed && state_ref.strategy == Strategy::EscalateToParent {
            state_ref.status = "escalated".to_string();
            state_ref.stop_requested = true;
            state_ref.escalated_count += 1;
            push_event(
                &mut state_ref,
                Some(child_name),
                "child_escalated",
                Some("failure escalated to parent supervisor".to_string()),
            );
            for child in &mut state_ref.children {
                if let Some(token) = &child.cancel_token {
                    token.store(true, Ordering::SeqCst);
                }
                if let Some(join) = child.join.take() {
                    join.abort();
                }
                child.status = "stopped".to_string();
            }
            return;
        }

        if failed
            && matches!(
                state_ref.strategy,
                Strategy::OneForAll | Strategy::RestForOne
            )
        {
            let start = if state_ref.strategy == Strategy::OneForAll {
                0
            } else {
                index
            };
            for sibling_index in start..state_ref.children.len() {
                if sibling_index != index {
                    cancel_child_for_restart(&mut state_ref, sibling_index);
                }
            }
            for affected in start..state_ref.children.len() {
                if let Some(plan) = restart_decision(&mut state_ref, affected, affected == index) {
                    restart_plan.push(plan);
                }
            }
        } else if let Some(plan) = restart_decision(&mut state_ref, index, failed) {
            restart_plan.push(plan);
        }
    }

    for (idx, delay_ms, gen) in restart_plan {
        if delay_ms == 0 {
            spawn_child(state.clone(), tx.clone(), base_vm.clone(), idx, true);
        } else {
            schedule_restart(tx.clone(), idx, gen, delay_ms);
        }
    }
}

fn cancel_child_for_restart(state: &mut SupervisorState, index: usize) {
    let Some(child) = state.children.get_mut(index) else {
        return;
    };
    child.generation = child.generation.saturating_add(1);
    if let Some(token) = &child.cancel_token {
        token.store(true, Ordering::SeqCst);
    }
    if let Some(join) = child.join.take() {
        join.abort();
    }
    child.cancel_token = None;
    child.status = "waiting".to_string();
    child.current_wait_reason = Some("propagation_restart".to_string());
    let name = child.spec.name.clone();
    push_event(
        state,
        Some(name),
        "child_stopped",
        Some("propagation restart".to_string()),
    );
}

fn restart_decision(
    state: &mut SupervisorState,
    index: usize,
    failed: bool,
) -> Option<(usize, u64, u64)> {
    let should_restart = match state.children.get(index)?.spec.restart.mode {
        RestartMode::Never => false,
        RestartMode::OnFailure => failed,
        RestartMode::Always => true,
    };
    if !should_restart {
        return None;
    }

    let now = SystemTime::now();
    if state.children[index].spec.restart.window_ms > 0 {
        let window = Duration::from_millis(state.children[index].spec.restart.window_ms);
        let child = &mut state.children[index];
        while child
            .failures
            .front()
            .and_then(|at| now.duration_since(*at).ok())
            .is_some_and(|age| age > window)
        {
            child.failures.pop_front();
        }
    }

    if let Some(max) = state.children[index].spec.restart.max_restarts {
        let cap_exceeded = if failed && state.children[index].spec.restart.window_ms > 0 {
            // The current failure is already recorded, so max_restarts=1 allows one restart
            // within the sliding window and suppresses the second failure in that window.
            state.children[index].failures.len() > max
        } else {
            state.children[index].restart_count >= max
        };
        if cap_exceeded {
            state.suppressed_count += 1;
            let name = state.children[index].spec.name.clone();
            if let Some(open_ms) = state.children[index].spec.restart.circuit_open_ms {
                let child = &mut state.children[index];
                child.status = "circuit_open".to_string();
                child.current_wait_reason = Some("circuit_open".to_string());
                child.next_restart_time_ms = Some(now_ms() + open_ms as i64);
                child.restart_count = 0;
                child.failures.clear();
                let generation = child.generation;
                push_event(
                    state,
                    Some(name),
                    "child_suppressed",
                    Some("restart cap reached; circuit opened".to_string()),
                );
                return Some((index, open_ms, generation));
            }
            {
                let child = &mut state.children[index];
                child.status = "suppressed".to_string();
                child.current_wait_reason = Some("restart_cap_exceeded".to_string());
            }
            push_event(
                state,
                Some(name),
                "child_suppressed",
                Some("restart cap reached".to_string()),
            );
            return None;
        }
    }

    let child = &mut state.children[index];
    let delay_ms = restart_delay_ms(&child.spec.restart, child.restart_count, index);
    child.restart_count += 1;
    child.generation = child.generation.saturating_add(1);
    child.status = "waiting".to_string();
    child.current_wait_reason = Some("restart_backoff".to_string());
    child.next_restart_time_ms = Some(now_ms() + delay_ms as i64);
    let generation = child.generation;
    let name = child.spec.name.clone();
    state.restarted_count += 1;
    push_event(
        state,
        Some(name),
        "child_restarted",
        Some(format!("scheduled after {delay_ms}ms")),
    );
    Some((index, delay_ms, generation))
}

fn spawn_child(
    state: Rc<RefCell<SupervisorState>>,
    tx: mpsc::UnboundedSender<SupervisorMsg>,
    base_vm: Rc<Vm>,
    index: usize,
    is_restart: bool,
) {
    let (spec, supervisor_id, generation, cancel_token) = {
        let mut state_ref = state.borrow_mut();
        if state_ref.stop_requested || index >= state_ref.children.len() {
            return;
        }
        let supervisor_id = state_ref.id.clone();
        let name = state_ref.children[index].spec.name.clone();
        let token = Arc::new(AtomicBool::new(false));
        let generation = {
            let child = &mut state_ref.children[index];
            child.status = "running".to_string();
            child.current_wait_reason = None;
            child.next_restart_time_ms = None;
            child.cancel_token = Some(token.clone());
            child.generation
        };
        state_ref.started_count += 1;
        push_event(&mut state_ref, Some(name), "child_started", None);
        if is_restart {
            let name = state_ref.children[index].spec.name.clone();
            push_event(&mut state_ref, Some(name), "child_restart_started", None);
        }
        (
            state_ref.children[index].spec.clone(),
            supervisor_id,
            generation,
            token,
        )
    };

    let task = spec.task.clone();
    let mut child_vm = base_vm.child_vm_for_host();
    child_vm.runtime_context = base_vm.runtime_context.child_task(
        format!(
            "{}:supervisor:{}:{}",
            base_vm.runtime_context.task_id, supervisor_id, spec.name
        ),
        format!("supervisor {}", spec.kind),
        Some(supervisor_id.clone()),
    );
    child_vm.cancel_token = Some(cancel_token);
    let child_ctx = child_context_value(&supervisor_id, &spec, generation);
    let join = tokio::task::spawn_local(async move {
        let result = child_vm
            .call_closure_pub(&task, &[child_ctx])
            .await
            .map_err(|error| error.to_string());
        let output = std::mem::take(&mut child_vm.output);
        let _ = tx.send(SupervisorMsg::ChildExited {
            index,
            generation,
            result,
            output,
        });
    });

    if let Some(child) = state.borrow_mut().children.get_mut(index) {
        child.join = Some(join);
    }
}

fn schedule_restart(
    tx: mpsc::UnboundedSender<SupervisorMsg>,
    index: usize,
    generation: u64,
    delay_ms: u64,
) {
    tokio::task::spawn_local(async move {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        let _ = tx.send(SupervisorMsg::RestartDue { index, generation });
    });
}

fn supervisor_terminal(state: &SupervisorState) -> bool {
    if matches!(state.status.as_str(), "stopped" | "escalated") {
        return true;
    }
    state.children.iter().all(|child| {
        (matches!(child.status.as_str(), "stopped" | "failed" | "suppressed")
            || (child.status == "circuit_open" && child.next_restart_time_ms.is_none()))
            && child.join.is_none()
    }) && state.children.iter().all(|child| child.status != "waiting")
}

fn restart_delay_ms(policy: &RestartPolicy, restart_count: usize, child_index: usize) -> u64 {
    let base = if policy.backoff_ms == 0 {
        0
    } else {
        ((policy.backoff_ms as f64) * policy.factor.powi(restart_count as i32)).round() as u64
    };
    let capped = base.min(policy.max_backoff_ms.max(policy.backoff_ms));
    if policy.jitter_ms == 0 {
        capped
    } else {
        let deterministic = ((child_index as u64 + 1) * 1_103 + restart_count as u64 * 9_176)
            % (policy.jitter_ms + 1);
        capped.saturating_add(deterministic)
    }
}

fn parse_children(
    value: Option<&VmValue>,
    default_restart: Option<&VmValue>,
) -> Result<Vec<ChildSpec>, VmError> {
    let Some(VmValue::List(children)) = value else {
        return Err(VmError::Runtime(
            "supervisor_start: children must be a list".to_string(),
        ));
    };
    let mut out = Vec::with_capacity(children.len());
    for (idx, child_value) in children.iter().enumerate() {
        let dict = require_dict(child_value, "supervisor_start child")?;
        let name = dict
            .get("name")
            .map(VmValue::display)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("child_{idx}"));
        let kind = dict
            .get("kind")
            .map(VmValue::display)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "task".to_string());
        let task = match dict.get("task").or_else(|| dict.get("handler")) {
            Some(VmValue::Closure(closure)) => closure.clone(),
            _ => {
                return Err(VmError::Runtime(format!(
                    "supervisor_start: child '{name}' requires a task closure"
                )));
            }
        };
        let restart = parse_restart_policy(dict.get("restart").or(default_restart))?;
        let active_lease = dict
            .get("active_lease")
            .or_else(|| dict.get("lease"))
            .map(VmValue::display)
            .filter(|value| !value.is_empty());
        out.push(ChildSpec {
            name,
            kind,
            task,
            restart,
            active_lease,
        });
    }
    Ok(out)
}

fn parse_restart_policy(value: Option<&VmValue>) -> Result<RestartPolicy, VmError> {
    let mut policy = RestartPolicy {
        mode: RestartMode::OnFailure,
        max_restarts: None,
        window_ms: 60_000,
        backoff_ms: 0,
        max_backoff_ms: 30_000,
        factor: 2.0,
        jitter_ms: 0,
        circuit_open_ms: None,
    };

    match value {
        None | Some(VmValue::Nil) => {}
        Some(VmValue::String(mode)) => policy.mode = parse_restart_mode(mode)?,
        Some(VmValue::Dict(dict)) => {
            if let Some(mode) = dict.get("mode").or_else(|| dict.get("policy")) {
                policy.mode = parse_restart_mode(&mode.display())?;
            }
            policy.max_restarts = dict
                .get("max_restarts")
                .or_else(|| dict.get("max"))
                .and_then(VmValue::as_int)
                .map(|value| value.max(0) as usize);
            policy.window_ms = dict
                .get("window_ms")
                .or_else(|| dict.get("window"))
                .and_then(duration_ms)
                .unwrap_or(policy.window_ms);
            policy.backoff_ms = dict
                .get("backoff_ms")
                .or_else(|| dict.get("backoff"))
                .or_else(|| dict.get("initial_backoff_ms"))
                .and_then(duration_ms)
                .unwrap_or(policy.backoff_ms);
            policy.max_backoff_ms = dict
                .get("max_backoff_ms")
                .or_else(|| dict.get("max_backoff"))
                .and_then(duration_ms)
                .unwrap_or(policy.max_backoff_ms);
            policy.factor = dict
                .get("factor")
                .or_else(|| dict.get("backoff_factor"))
                .and_then(|value| match value {
                    VmValue::Float(value) => Some(*value),
                    VmValue::Int(value) => Some(*value as f64),
                    _ => None,
                })
                .unwrap_or(policy.factor)
                .max(1.0);
            policy.jitter_ms = dict
                .get("jitter_ms")
                .or_else(|| dict.get("jitter"))
                .and_then(duration_ms)
                .unwrap_or(policy.jitter_ms);
            policy.circuit_open_ms = dict
                .get("circuit_open_ms")
                .or_else(|| dict.get("circuit_open"))
                .and_then(duration_ms);
        }
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "supervisor_start: restart policy must be a string or dict, got {}",
                other.type_name()
            )));
        }
    }
    Ok(policy)
}

fn parse_restart_mode(value: &str) -> Result<RestartMode, VmError> {
    match normalize(value).as_str() {
        "never" => Ok(RestartMode::Never),
        "on_failure" | "onfailure" | "failure" => Ok(RestartMode::OnFailure),
        "always" => Ok(RestartMode::Always),
        other => Err(VmError::Runtime(format!(
            "supervisor_start: unknown restart mode '{other}'"
        ))),
    }
}

fn parse_strategy(value: Option<&VmValue>) -> Result<Strategy, VmError> {
    match value
        .map(VmValue::display)
        .as_deref()
        .map(normalize)
        .as_deref()
    {
        None | Some("one_for_one") | Some("oneforone") => Ok(Strategy::OneForOne),
        Some("one_for_all") | Some("oneforall") => Ok(Strategy::OneForAll),
        Some("rest_for_one") | Some("restforone") => Ok(Strategy::RestForOne),
        Some("escalate_to_parent") | Some("escalate") => Ok(Strategy::EscalateToParent),
        Some(other) => Err(VmError::Runtime(format!(
            "supervisor_start: unknown strategy '{other}'"
        ))),
    }
}

fn require_dict<'a>(
    value: &'a VmValue,
    builtin: &str,
) -> Result<&'a BTreeMap<String, VmValue>, VmError> {
    value
        .as_dict()
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: expected a dict")))
}

fn supervisor_from_args(
    args: &[VmValue],
    builtin: &str,
) -> Result<Rc<RefCell<SupervisorState>>, VmError> {
    let id = supervisor_id_from_args(args, builtin)?;
    supervisor_lookup(&id)
        .map(|handle| handle.state)
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: unknown supervisor '{id}'")))
}

fn supervisor_lookup(id: &str) -> Option<SupervisorHandle> {
    SUPERVISORS.with(|registry| registry.borrow().get(id).cloned())
}

fn supervisor_id_from_args(args: &[VmValue], builtin: &str) -> Result<String, VmError> {
    let value = args.first().ok_or_else(|| {
        VmError::Runtime(format!("{builtin}: supervisor handle or id is required"))
    })?;
    if let Some(dict) = value.as_dict() {
        if dict.get("_type").map(VmValue::display).as_deref() == Some("supervisor") {
            return dict
                .get("id")
                .map(VmValue::display)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    VmError::Runtime(format!("{builtin}: supervisor handle missing id"))
                });
        }
    }
    let id = value.display();
    if id.is_empty() {
        Err(VmError::Runtime(format!(
            "{builtin}: supervisor id must not be empty"
        )))
    } else {
        Ok(id)
    }
}

fn next_supervisor_id() -> String {
    SUPERVISOR_COUNTER.with(|counter| {
        let mut counter = counter.borrow_mut();
        *counter += 1;
        format!("supervisor_{}", *counter)
    })
}

fn supervisor_handle_value(state: &SupervisorState) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("_type".to_string(), string_value("supervisor"));
    dict.insert("id".to_string(), string_value(&state.id));
    dict.insert("name".to_string(), string_value(&state.name));
    VmValue::Dict(Rc::new(dict))
}

fn supervisor_state_value(state: &SupervisorState) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("_type".to_string(), string_value("supervisor_state"));
    dict.insert("id".to_string(), string_value(&state.id));
    dict.insert("name".to_string(), string_value(&state.name));
    dict.insert("status".to_string(), string_value(&state.status));
    dict.insert(
        "strategy".to_string(),
        string_value(strategy_name(state.strategy)),
    );
    dict.insert("metrics".to_string(), metrics_value(state));
    dict.insert(
        "children".to_string(),
        VmValue::List(Rc::new(
            state.children.iter().map(child_state_value).collect(),
        )),
    );
    VmValue::Dict(Rc::new(dict))
}

fn child_state_value(child: &ChildSlot) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("name".to_string(), string_value(&child.spec.name));
    dict.insert("kind".to_string(), string_value(&child.spec.kind));
    dict.insert("status".to_string(), string_value(&child.status));
    dict.insert(
        "restart_count".to_string(),
        VmValue::Int(child.restart_count as i64),
    );
    dict.insert(
        "last_error".to_string(),
        child
            .last_error
            .as_deref()
            .map(string_value)
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "current_wait_reason".to_string(),
        child
            .current_wait_reason
            .as_deref()
            .map(string_value)
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "active_lease".to_string(),
        child
            .spec
            .active_lease
            .as_deref()
            .map(string_value)
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "next_restart_time".to_string(),
        child
            .next_restart_time_ms
            .map(VmValue::Int)
            .unwrap_or(VmValue::Nil),
    );
    VmValue::Dict(Rc::new(dict))
}

fn metrics_value(state: &SupervisorState) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("started".to_string(), VmValue::Int(state.started_count));
    dict.insert("stopped".to_string(), VmValue::Int(state.stopped_count));
    dict.insert("failed".to_string(), VmValue::Int(state.failed_count));
    dict.insert("restarted".to_string(), VmValue::Int(state.restarted_count));
    dict.insert(
        "suppressed".to_string(),
        VmValue::Int(state.suppressed_count),
    );
    dict.insert("escalated".to_string(), VmValue::Int(state.escalated_count));
    VmValue::Dict(Rc::new(dict))
}

fn events_value(state: &SupervisorState) -> VmValue {
    VmValue::List(Rc::new(state.events.iter().map(event_value).collect()))
}

pub(crate) fn supervisor_debug_values() -> VmValue {
    SUPERVISORS.with(|registry| {
        VmValue::List(Rc::new(
            registry
                .borrow()
                .values()
                .map(|handle| supervisor_state_value(&handle.state.borrow()))
                .collect(),
        ))
    })
}

pub(crate) fn reset_supervisor_state() {
    SUPERVISORS.with(|registry| {
        let mut registry = registry.borrow_mut();
        for handle in registry.values_mut() {
            let mut state = handle.state.borrow_mut();
            for child in &mut state.children {
                if let Some(token) = &child.cancel_token {
                    token.store(true, Ordering::SeqCst);
                }
                if let Some(join) = child.join.take() {
                    join.abort();
                }
            }
            if let Some(join) = state.supervisor_join.take() {
                join.abort();
            }
        }
        registry.clear();
    });
    SUPERVISOR_COUNTER.with(|counter| {
        *counter.borrow_mut() = 0;
    });
}

fn event_value(event: &SupervisorEvent) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("seq".to_string(), VmValue::Int(event.seq));
    dict.insert("at_ms".to_string(), VmValue::Int(event.at_ms));
    dict.insert(
        "supervisor_id".to_string(),
        string_value(&event.supervisor_id),
    );
    dict.insert(
        "child".to_string(),
        event
            .child
            .as_deref()
            .map(string_value)
            .unwrap_or(VmValue::Nil),
    );
    dict.insert("kind".to_string(), string_value(&event.kind));
    dict.insert(
        "message".to_string(),
        event
            .message
            .as_deref()
            .map(string_value)
            .unwrap_or(VmValue::Nil),
    );
    VmValue::Dict(Rc::new(dict))
}

fn child_context_value(supervisor_id: &str, spec: &ChildSpec, generation: u64) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("supervisor_id".to_string(), string_value(supervisor_id));
    dict.insert("child_name".to_string(), string_value(&spec.name));
    dict.insert("child_kind".to_string(), string_value(&spec.kind));
    dict.insert("attempt".to_string(), VmValue::Int(generation as i64 + 1));
    dict.insert("restart_count".to_string(), VmValue::Int(generation as i64));
    VmValue::Dict(Rc::new(dict))
}

fn push_event(
    state: &mut SupervisorState,
    child: Option<String>,
    kind: &str,
    message: Option<String>,
) {
    let event = SupervisorEvent {
        seq: state.next_seq,
        at_ms: now_ms(),
        supervisor_id: state.id.clone(),
        child,
        kind: kind.to_string(),
        message,
    };
    state.next_seq += 1;
    emit_event_log(&event);
    state.events.push(event);
}

fn emit_event_log(event: &SupervisorEvent) {
    let Some(log) = active_event_log() else {
        return;
    };
    let Ok(topic) = Topic::new("supervisor.lifecycle") else {
        return;
    };
    let payload = serde_json::json!({
        "seq": event.seq,
        "supervisor_id": event.supervisor_id,
        "child": event.child,
        "message": event.message,
    });
    let log_event = LogEvent::new(event.kind.clone(), payload);
    tokio::task::spawn_local(async move {
        let _ = log.append(&topic, log_event).await;
    });
}

fn string_value(value: &str) -> VmValue {
    VmValue::String(Rc::from(value))
}

fn duration_ms(value: &VmValue) -> Option<u64> {
    match value {
        VmValue::Duration(ms) => Some(*ms),
        VmValue::Int(ms) => Some((*ms).max(0) as u64),
        VmValue::Float(ms) => Some(ms.max(0.0) as u64),
        VmValue::String(text) => parse_duration_string(text),
        _ => None,
    }
}

fn parse_duration_string(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let split_at = trimmed
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (number, unit) = trimmed.split_at(split_at);
    let amount = number.parse::<u64>().ok()?;
    match unit.trim() {
        "" | "ms" => Some(amount),
        "s" => Some(amount.saturating_mul(1000)),
        "m" => Some(amount.saturating_mul(60_000)),
        "h" => Some(amount.saturating_mul(3_600_000)),
        _ => None,
    }
}

fn normalize(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace(['-', ' '], "_")
}

fn strategy_name(strategy: Strategy) -> &'static str {
    match strategy {
        Strategy::OneForOne => "one_for_one",
        Strategy::OneForAll => "one_for_all",
        Strategy::RestForOne => "rest_for_one",
        Strategy::EscalateToParent => "escalate_to_parent",
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_delay_is_exponential_capped_and_jittered_deterministically() {
        let policy = RestartPolicy {
            mode: RestartMode::OnFailure,
            max_restarts: Some(5),
            window_ms: 1000,
            backoff_ms: 10,
            max_backoff_ms: 40,
            factor: 2.0,
            jitter_ms: 3,
            circuit_open_ms: None,
        };
        assert_eq!(restart_delay_ms(&policy, 0, 0), 13);
        assert_eq!(restart_delay_ms(&policy, 1, 0), 23);
        assert_eq!(restart_delay_ms(&policy, 2, 0), 43);
        assert_eq!(restart_delay_ms(&policy, 3, 0), 43);
    }

    #[test]
    fn parses_restart_policy_aliases() {
        let policy = parse_restart_policy(Some(&VmValue::Dict(Rc::new(BTreeMap::from([
            ("mode".to_string(), string_value("on-failure")),
            ("max".to_string(), VmValue::Int(2)),
            ("window".to_string(), VmValue::Duration(500)),
            ("backoff".to_string(), string_value("10ms")),
            ("factor".to_string(), VmValue::Float(3.0)),
            ("jitter".to_string(), VmValue::Int(4)),
            ("circuit_open".to_string(), string_value("1s")),
        ])))))
        .unwrap();

        assert_eq!(policy.mode, RestartMode::OnFailure);
        assert_eq!(policy.max_restarts, Some(2));
        assert_eq!(policy.window_ms, 500);
        assert_eq!(policy.backoff_ms, 10);
        assert_eq!(policy.factor, 3.0);
        assert_eq!(policy.jitter_ms, 4);
        assert_eq!(policy.circuit_open_ms, Some(1000));
    }
}
