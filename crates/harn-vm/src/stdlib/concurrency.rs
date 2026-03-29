use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

use crate::value::{VmAtomicHandle, VmChannelHandle, VmError, VmValue};
use crate::vm::Vm;

use std::collections::BTreeMap;

struct CircuitState {
    failures: usize,
    threshold: usize,
    reset_ms: u64,
    opened_at: Option<std::time::Instant>,
}

thread_local! {
    static CIRCUITS: RefCell<HashMap<String, CircuitState>> = RefCell::new(HashMap::new());
}

/// Build a select result dict with the given index, value, and channel name.
fn select_result(index: usize, value: VmValue, channel_name: &str) -> VmValue {
    let mut result = BTreeMap::new();
    result.insert("index".to_string(), VmValue::Int(index as i64));
    result.insert("value".to_string(), value);
    result.insert(
        "channel".to_string(),
        VmValue::String(Rc::from(channel_name)),
    );
    VmValue::Dict(Rc::new(result))
}

/// Build a select result dict indicating no channel was ready (index = -1).
fn select_none() -> VmValue {
    let mut result = BTreeMap::new();
    result.insert("index".to_string(), VmValue::Int(-1));
    result.insert("value".to_string(), VmValue::Nil);
    result.insert("channel".to_string(), VmValue::Nil);
    VmValue::Dict(Rc::new(result))
}

/// Try to receive from a list of channels (non-blocking).
fn try_poll_channels(channels: &[VmValue]) -> (Option<(usize, VmValue, String)>, bool) {
    let mut all_closed = true;
    for (i, ch_val) in channels.iter().enumerate() {
        if let VmValue::Channel(ch) = ch_val {
            if let Ok(mut rx) = ch.receiver.try_lock() {
                match rx.try_recv() {
                    Ok(val) => return (Some((i, val, ch.name.clone())), false),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                        all_closed = false;
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {}
                }
            } else {
                all_closed = false;
            }
        }
    }
    (None, all_closed)
}

pub(crate) fn register_concurrency_builtins(vm: &mut Vm) {
    // --- Channels ---

    vm.register_builtin("channel", |args, _out| {
        let name = args
            .first()
            .map(|a| a.display())
            .unwrap_or_else(|| "default".to_string());
        let capacity = args.get(1).and_then(|a| a.as_int()).unwrap_or(256) as usize;
        let capacity = capacity.max(1);
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        #[allow(clippy::arc_with_non_send_sync)]
        Ok(VmValue::Channel(VmChannelHandle {
            name,
            sender: Arc::new(tx),
            receiver: Arc::new(tokio::sync::Mutex::new(rx)),
            closed: Arc::new(AtomicBool::new(false)),
        }))
    });

    vm.register_builtin("close_channel", |args, _out| {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "close_channel: requires a channel",
            ))));
        }
        if let VmValue::Channel(ch) = &args[0] {
            ch.closed.store(true, Ordering::SeqCst);
            Ok(VmValue::Nil)
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                "close_channel: first argument must be a channel",
            ))))
        }
    });

    vm.register_builtin("try_receive", |args, _out| {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "try_receive: requires a channel",
            ))));
        }
        if let VmValue::Channel(ch) = &args[0] {
            match ch.receiver.try_lock() {
                Ok(mut rx) => match rx.try_recv() {
                    Ok(val) => Ok(val),
                    Err(_) => Ok(VmValue::Nil),
                },
                Err(_) => Ok(VmValue::Nil),
            }
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                "try_receive: first argument must be a channel",
            ))))
        }
    });

    // --- Atomics ---

    vm.register_builtin("atomic", |args, _out| {
        let initial = match args.first() {
            Some(VmValue::Int(n)) => *n,
            Some(VmValue::Float(f)) => *f as i64,
            Some(VmValue::Bool(b)) => {
                if *b {
                    1
                } else {
                    0
                }
            }
            _ => 0,
        };
        Ok(VmValue::Atomic(VmAtomicHandle {
            value: Arc::new(AtomicI64::new(initial)),
        }))
    });

    vm.register_builtin("atomic_get", |args, _out| {
        if let Some(VmValue::Atomic(a)) = args.first() {
            Ok(VmValue::Int(a.value.load(Ordering::SeqCst)))
        } else {
            Ok(VmValue::Nil)
        }
    });

    vm.register_builtin("atomic_set", |args, _out| {
        if args.len() >= 2 {
            if let (VmValue::Atomic(a), Some(val)) = (&args[0], args[1].as_int()) {
                let old = a.value.swap(val, Ordering::SeqCst);
                return Ok(VmValue::Int(old));
            }
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("atomic_add", |args, _out| {
        if args.len() >= 2 {
            if let (VmValue::Atomic(a), Some(delta)) = (&args[0], args[1].as_int()) {
                let prev = a.value.fetch_add(delta, Ordering::SeqCst);
                return Ok(VmValue::Int(prev));
            }
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("atomic_cas", |args, _out| {
        if args.len() >= 3 {
            if let (VmValue::Atomic(a), Some(expected), Some(new_val)) =
                (&args[0], args[1].as_int(), args[2].as_int())
            {
                let result =
                    a.value
                        .compare_exchange(expected, new_val, Ordering::SeqCst, Ordering::SeqCst);
                return Ok(VmValue::Bool(result.is_ok()));
            }
        }
        Ok(VmValue::Bool(false))
    });

    // --- Async builtins ---

    vm.register_async_builtin("sleep", |args| async move {
        let ms = match args.first() {
            Some(VmValue::Duration(ms)) => *ms,
            Some(VmValue::Int(n)) => *n as u64,
            _ => 0,
        };
        if ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(ms)).await;
        }
        Ok(VmValue::Nil)
    });

    vm.register_async_builtin("send", |args| async move {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "send: requires channel and value",
            ))));
        }
        if let VmValue::Channel(ch) = &args[0] {
            if ch.closed.load(Ordering::SeqCst) {
                return Ok(VmValue::Bool(false));
            }
            let val = args[1].clone();
            match ch.sender.send(val).await {
                Ok(()) => Ok(VmValue::Bool(true)),
                Err(_) => Ok(VmValue::Bool(false)),
            }
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                "send: first argument must be a channel",
            ))))
        }
    });

    vm.register_async_builtin("receive", |args| async move {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "receive: requires a channel",
            ))));
        }
        if let VmValue::Channel(ch) = &args[0] {
            if ch.closed.load(Ordering::SeqCst) {
                let mut rx = ch.receiver.lock().await;
                return match rx.try_recv() {
                    Ok(val) => Ok(val),
                    Err(_) => Ok(VmValue::Nil),
                };
            }
            let mut rx = ch.receiver.lock().await;
            match rx.recv().await {
                Some(val) => Ok(val),
                None => Ok(VmValue::Nil),
            }
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                "receive: first argument must be a channel",
            ))))
        }
    });

    vm.register_async_builtin("select", |args| async move {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "select: requires at least one channel",
            ))));
        }
        for arg in &args {
            if !matches!(arg, VmValue::Channel(_)) {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "select: all arguments must be channels",
                ))));
            }
        }
        loop {
            let (found, all_closed) = try_poll_channels(&args);
            if let Some((i, val, name)) = found {
                return Ok(select_result(i, val, &name));
            }
            if all_closed {
                return Ok(select_none());
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
        }
    });

    vm.register_async_builtin("__select_timeout", |args| async move {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "__select_timeout: requires channel list and timeout",
            ))));
        }
        let channels = match &args[0] {
            VmValue::List(items) => (**items).clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "__select_timeout: first argument must be a list of channels",
                ))));
            }
        };
        let timeout_ms = match &args[1] {
            VmValue::Int(n) => (*n).max(0) as u64,
            VmValue::Duration(ms) => *ms,
            _ => 5000,
        };
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);
        loop {
            let (found, all_closed) = try_poll_channels(&channels);
            if let Some((i, val, name)) = found {
                return Ok(select_result(i, val, &name));
            }
            if all_closed || tokio::time::Instant::now() >= deadline {
                return Ok(select_none());
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
        }
    });

    vm.register_async_builtin("__select_try", |args| async move {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "__select_try: requires channel list",
            ))));
        }
        let channels = match &args[0] {
            VmValue::List(items) => (**items).clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "__select_try: first argument must be a list of channels",
                ))));
            }
        };
        let (found, _) = try_poll_channels(&channels);
        if let Some((i, val, name)) = found {
            Ok(select_result(i, val, &name))
        } else {
            Ok(select_none())
        }
    });

    vm.register_async_builtin("__select_list", |args| async move {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "__select_list: requires channel list",
            ))));
        }
        let channels = match &args[0] {
            VmValue::List(items) => (**items).clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "__select_list: first argument must be a list of channels",
                ))));
            }
        };
        loop {
            let (found, all_closed) = try_poll_channels(&channels);
            if let Some((i, val, name)) = found {
                return Ok(select_result(i, val, &name));
            }
            if all_closed {
                return Ok(select_none());
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
        }
    });

    // --- Timer builtins ---

    vm.register_builtin("timer_start", |args, _out| {
        let name = args
            .first()
            .map(|a| a.display())
            .unwrap_or_else(|| "default".to_string());
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let mut timer = BTreeMap::new();
        timer.insert("name".to_string(), VmValue::String(Rc::from(name)));
        timer.insert("start_ms".to_string(), VmValue::Int(now_ms));
        Ok(VmValue::Dict(Rc::new(timer)))
    });

    // --- Circuit breaker builtins ---

    vm.register_builtin("circuit_breaker", |args, _out| {
        let name = args
            .first()
            .map(|a| a.display())
            .unwrap_or_else(|| "default".to_string());
        let threshold = args.get(1).and_then(|a| a.as_int()).unwrap_or(5) as usize;
        let reset_ms = args.get(2).and_then(|a| a.as_int()).unwrap_or(30000) as u64;
        CIRCUITS.with(|circuits| {
            circuits.borrow_mut().insert(
                name.clone(),
                CircuitState {
                    failures: 0,
                    threshold,
                    reset_ms,
                    opened_at: None,
                },
            );
        });
        Ok(VmValue::String(Rc::from(name)))
    });

    vm.register_builtin("circuit_check", |args, _out| {
        let name = args
            .first()
            .map(|a| a.display())
            .unwrap_or_else(|| "default".to_string());
        let state = CIRCUITS.with(|circuits| {
            let circuits = circuits.borrow();
            let Some(cs) = circuits.get(&name) else {
                return "closed".to_string();
            };
            match cs.opened_at {
                None => "closed".to_string(),
                Some(opened) => {
                    let elapsed = opened.elapsed().as_millis() as u64;
                    if elapsed >= cs.reset_ms {
                        "half_open".to_string()
                    } else {
                        "open".to_string()
                    }
                }
            }
        });
        Ok(VmValue::String(Rc::from(state)))
    });

    vm.register_builtin("circuit_record_success", |args, _out| {
        let name = args
            .first()
            .map(|a| a.display())
            .unwrap_or_else(|| "default".to_string());
        CIRCUITS.with(|circuits| {
            let mut circuits = circuits.borrow_mut();
            if let Some(cs) = circuits.get_mut(&name) {
                cs.failures = 0;
                cs.opened_at = None;
            }
        });
        Ok(VmValue::Nil)
    });

    vm.register_builtin("circuit_record_failure", |args, _out| {
        let name = args
            .first()
            .map(|a| a.display())
            .unwrap_or_else(|| "default".to_string());
        let is_open = CIRCUITS.with(|circuits| {
            let mut circuits = circuits.borrow_mut();
            if let Some(cs) = circuits.get_mut(&name) {
                cs.failures += 1;
                if cs.failures >= cs.threshold && cs.opened_at.is_none() {
                    cs.opened_at = Some(std::time::Instant::now());
                    return true;
                }
            }
            false
        });
        Ok(VmValue::Bool(is_open))
    });

    vm.register_builtin("circuit_reset", |args, _out| {
        let name = args
            .first()
            .map(|a| a.display())
            .unwrap_or_else(|| "default".to_string());
        CIRCUITS.with(|circuits| {
            let mut circuits = circuits.borrow_mut();
            if let Some(cs) = circuits.get_mut(&name) {
                cs.failures = 0;
                cs.opened_at = None;
            }
        });
        Ok(VmValue::Nil)
    });

    vm.register_builtin("timer_end", |args, out| {
        let timer = match args.first() {
            Some(VmValue::Dict(d)) => d,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "timer_end: argument must be a timer dict from timer_start",
                ))));
            }
        };
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let start_ms = timer
            .get("start_ms")
            .and_then(|v| v.as_int())
            .unwrap_or(now_ms);
        let elapsed = now_ms - start_ms;
        let name = timer.get("name").map(|v| v.display()).unwrap_or_default();
        out.push_str(&format!("[timer] {name}: {elapsed}ms\n"));
        Ok(VmValue::Int(elapsed))
    });
}
