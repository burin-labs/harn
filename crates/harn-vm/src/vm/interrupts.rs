use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::value::{VmError, VmValue};

use super::Vm;

const CANCEL_GRACE_INSTRUCTIONS: usize = 1024;

impl Vm {
    pub(crate) fn register_interrupt_handler(
        &mut self,
        handler: VmValue,
        opts: Option<&VmValue>,
    ) -> Result<VmValue, VmError> {
        if !Self::is_callable_value(&handler) {
            return Err(VmError::TypeError(format!(
                "on_interrupt: handler must be callable, got {}",
                handler.type_name()
            )));
        }

        let signals = parse_signal_list(opts)?;
        let once = parse_bool_option(opts, "once")?.unwrap_or(true);
        let graceful_timeout_ms =
            parse_non_negative_int_option(opts, "graceful_timeout_ms")?.map(|ms| ms as u64);

        let handle = self.next_interrupt_handle;
        self.next_interrupt_handle += 1;
        self.interrupt_handlers.push(super::InterruptHandler {
            handle,
            signals: signals.clone(),
            once,
            graceful_timeout_ms,
            handler,
        });

        Ok(VmValue::Dict(Rc::new(BTreeMap::from([
            ("handle".to_string(), VmValue::Int(handle)),
            (
                "signals".to_string(),
                VmValue::List(Rc::new(
                    signals
                        .into_iter()
                        .map(|signal| VmValue::String(Rc::from(signal)))
                        .collect(),
                )),
            ),
            ("once".to_string(), VmValue::Bool(once)),
        ]))))
    }

    pub(crate) fn unregister_interrupt_handler(&mut self, handle: &VmValue) -> Result<(), VmError> {
        let handle = parse_handle(handle)?;
        self.interrupt_handlers
            .retain(|entry| entry.handle != handle);
        Ok(())
    }

    pub(crate) fn interrupted(&self) -> bool {
        self.interrupted
            || self.pending_interrupt_signal.is_some()
            || self
                .interrupt_signal_token
                .as_ref()
                .is_some_and(|token| token.lock().ok().is_some_and(|guard| guard.is_some()))
            || self.is_cancel_requested()
    }

    pub(crate) fn signal_interrupt(&mut self, signal: &str) -> Result<(), VmError> {
        let signal = normalize_signal(signal)?;
        self.pending_interrupt_signal = Some(signal);
        Ok(())
    }

    pub(crate) fn has_interrupt_handler_for(&self, signal: &str) -> bool {
        self.interrupt_handlers
            .iter()
            .any(|entry| entry.signals.iter().any(|candidate| candidate == signal))
    }

    /// Box-pin'd to break the static recursion cycle between
    /// `drive_until_frame_depth` (the hot dispatch loop), `call_callable_value`
    /// (the hot per-callback path), and the slow interrupt-handler path: a
    /// handler runs another Harn closure, which re-enters the dispatch loop,
    /// which may again raise an interrupt. Indirecting at this slow-path
    /// boundary keeps the recursion satisfied while the hot path stays free of
    /// per-invocation heap allocation.
    pub(crate) fn dispatch_interrupt_handlers<'a>(
        &'a mut self,
        signal: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, VmError>> + 'a>> {
        Box::pin(async move {
            let signal = normalize_signal(signal)?;
            self.interrupted = true;

            if self.dispatching_interrupt {
                return Err(Self::cancelled_error());
            }

            let matching: Vec<(i64, bool, Option<u64>, VmValue)> = self
                .interrupt_handlers
                .iter()
                .rev()
                .filter(|entry| entry.signals.iter().any(|candidate| candidate == &signal))
                .map(|entry| {
                    (
                        entry.handle,
                        entry.once,
                        entry.graceful_timeout_ms,
                        entry.handler.clone(),
                    )
                })
                .collect();
            if matching.is_empty() {
                return Ok(false);
            }

            self.clear_cancel_request();
            self.dispatching_interrupt = true;
            let mut once_handles = Vec::new();
            let mut result = Ok(());
            for (handle, once, graceful_timeout_ms, handler) in matching {
                if once {
                    once_handles.push(handle);
                }
                let saved_interrupt_deadline = self.interrupt_handler_deadline;
                self.interrupt_handler_deadline = graceful_timeout_ms
                    .and_then(|ms| Instant::now().checked_add(Duration::from_millis(ms)));
                let handler_result = self.call_callable_zero(&handler).await;
                self.interrupt_handler_deadline = saved_interrupt_deadline;
                if let Err(error) = handler_result {
                    result = Err(error);
                    break;
                }
            }
            self.dispatching_interrupt = false;

            if !once_handles.is_empty() {
                self.interrupt_handlers
                    .retain(|entry| !once_handles.contains(&entry.handle));
            }

            result.map(|()| true)
        })
    }

    pub(crate) async fn pending_scope_interrupt(&mut self) -> Option<VmError> {
        let mut pending_from_host = false;
        if self.pending_interrupt_signal.is_none() {
            self.pending_interrupt_signal = self.take_host_interrupt_signal();
            pending_from_host = self.pending_interrupt_signal.is_some();
        }
        if let Some(signal) = self.pending_interrupt_signal.take() {
            match self.dispatch_interrupt_handlers(&signal).await {
                Ok(true) => return None,
                Ok(false) if !pending_from_host => return Some(Self::interrupted_error(&signal)),
                Ok(false) => {}
                Err(error) => return Some(error),
            }
        }

        if self
            .interrupt_handler_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Some(Self::interrupt_handler_timeout_error());
        }

        if self.is_cancel_requested() {
            let signal = self
                .take_host_interrupt_signal()
                .unwrap_or_else(|| "SIGINT".to_string());
            if self.has_interrupt_handler_for(&signal) {
                match self.dispatch_interrupt_handlers(&signal).await {
                    Ok(true) => return None,
                    Ok(false) => {}
                    Err(error) => return Some(error),
                }
            }

            match self.cancel_grace_instructions_remaining.as_mut() {
                Some(0) => {
                    self.cancel_spawned_tasks();
                    return Some(Self::cancelled_error());
                }
                Some(remaining) => *remaining -= 1,
                None => self.cancel_grace_instructions_remaining = Some(CANCEL_GRACE_INSTRUCTIONS),
            }
        } else {
            self.cancel_grace_instructions_remaining = None;
        }

        if let Some(&(deadline, _)) = self.deadlines.last() {
            if Instant::now() >= deadline {
                self.deadlines.pop();
                return Some(Self::deadline_exceeded_error());
            }
        }

        None
    }

    fn clear_cancel_request(&mut self) {
        if let Some(token) = &self.cancel_token {
            token.store(false, std::sync::atomic::Ordering::SeqCst);
        }
        self.cancel_grace_instructions_remaining = None;
    }

    pub(crate) fn take_host_interrupt_signal(&mut self) -> Option<String> {
        self.interrupt_signal_token
            .as_ref()
            .and_then(|token| token.lock().ok().and_then(|mut guard| guard.take()))
    }

    pub(crate) fn interrupted_error(signal: &str) -> VmError {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "kind:interrupted:{signal}"
        ))))
    }

    pub(crate) fn interrupt_handler_timeout_error() -> VmError {
        VmError::Thrown(VmValue::String(Rc::from(
            "kind:interrupted:handler_timeout",
        )))
    }
}

fn parse_signal_list(opts: Option<&VmValue>) -> Result<Vec<String>, VmError> {
    let Some(VmValue::Dict(opts)) = opts else {
        return Ok(vec!["SIGINT".to_string()]);
    };
    let Some(value) = opts.get("signals") else {
        return Ok(vec!["SIGINT".to_string()]);
    };
    match value {
        VmValue::Nil => Ok(vec!["SIGINT".to_string()]),
        VmValue::String(signal) => Ok(vec![normalize_signal(signal.as_ref())?]),
        VmValue::List(items) => {
            if items.is_empty() {
                return Err(VmError::Runtime(
                    "on_interrupt: signals must not be empty".to_string(),
                ));
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items.iter() {
                let VmValue::String(signal) = item else {
                    return Err(VmError::TypeError(format!(
                        "on_interrupt: signals entries must be strings, got {}",
                        item.type_name()
                    )));
                };
                out.push(normalize_signal(signal.as_ref())?);
            }
            out.sort();
            out.dedup();
            Ok(out)
        }
        other => Err(VmError::TypeError(format!(
            "on_interrupt: signals must be a string or list<string>, got {}",
            other.type_name()
        ))),
    }
}

fn parse_bool_option(opts: Option<&VmValue>, key: &str) -> Result<Option<bool>, VmError> {
    let Some(VmValue::Dict(opts)) = opts else {
        return Ok(None);
    };
    match opts.get(key) {
        Some(VmValue::Bool(value)) => Ok(Some(*value)),
        Some(VmValue::Nil) | None => Ok(None),
        Some(other) => Err(VmError::TypeError(format!(
            "on_interrupt: {key} must be bool, got {}",
            other.type_name()
        ))),
    }
}

fn parse_non_negative_int_option(
    opts: Option<&VmValue>,
    key: &str,
) -> Result<Option<i64>, VmError> {
    let Some(VmValue::Dict(opts)) = opts else {
        return Ok(None);
    };
    match opts.get(key) {
        Some(VmValue::Int(value)) if *value >= 0 => Ok(Some(*value)),
        Some(VmValue::Duration(value)) if *value >= 0 => Ok(Some(*value)),
        Some(VmValue::Nil) | None => Ok(None),
        Some(other) => Err(VmError::TypeError(format!(
            "on_interrupt: {key} must be a non-negative int or duration, got {}",
            other.type_name()
        ))),
    }
}

fn parse_handle(value: &VmValue) -> Result<i64, VmError> {
    match value {
        VmValue::Int(handle) => Ok(*handle),
        VmValue::Dict(map) => match map.get("handle") {
            Some(VmValue::Int(handle)) => Ok(*handle),
            Some(other) => Err(VmError::TypeError(format!(
                "off_interrupt: handle field must be int, got {}",
                other.type_name()
            ))),
            None => Err(VmError::Runtime(
                "off_interrupt: handle dict is missing `handle`".to_string(),
            )),
        },
        other => Err(VmError::TypeError(format!(
            "off_interrupt: expected handle int or dict, got {}",
            other.type_name()
        ))),
    }
}

fn normalize_signal(signal: &str) -> Result<String, VmError> {
    match signal {
        "SIGINT" | "SIGTERM" | "SIGHUP" => Ok(signal.to_string()),
        other => Err(VmError::Runtime(format!(
            "on_interrupt: unsupported signal '{other}'"
        ))),
    }
}
