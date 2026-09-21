use crate::cancellation::{cancelled_error, HandlerDispatch};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
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

        Ok(VmValue::dict(BTreeMap::from([
            ("handle".to_string(), VmValue::Int(handle)),
            (
                "signals".to_string(),
                match signals {
                    // Nil back out for nil in: no filter. An empty list is
                    // still refused on the way in, so the two cannot be
                    // confused.
                    None => VmValue::Nil,
                    Some(signals) => VmValue::List(std::sync::Arc::new(
                        signals
                            .into_iter()
                            .map(|signal| VmValue::String(arcstr::ArcStr::from(signal)))
                            .collect(),
                    )),
                },
            ),
            ("once".to_string(), VmValue::Bool(once)),
        ])))
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
            .any(|entry| Self::handler_wants(entry, Some(signal)))
    }

    /// Whether any handler runs on a cancellation that carries no signal.
    pub(crate) fn has_unfiltered_interrupt_handler(&self) -> bool {
        self.interrupt_handlers
            .iter()
            .any(|entry| Self::handler_wants(entry, None))
    }

    /// How many handlers are skipped because they asked for a specific signal
    /// and none was delivered. Reported rather than dropped quietly.
    fn filtered_handlers_skipped_without_signal(&self) -> Vec<(i64, String)> {
        self.interrupt_handlers
            .iter()
            .filter_map(|entry| {
                entry
                    .signals
                    .as_ref()
                    .map(|wanted| (entry.handle, wanted.join(",")))
            })
            .collect()
    }

    fn handler_wants(entry: &super::InterruptHandler, signal: Option<&str>) -> bool {
        match (&entry.signals, signal) {
            // No filter: every cancellation, with or without a signal.
            (None, _) => true,
            (Some(wanted), Some(signal)) => wanted.iter().any(|candidate| candidate == signal),
            // Filtered, but nothing was delivered to match against.
            (Some(_), None) => false,
        }
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
    ) -> Pin<Box<dyn Future<Output = Result<bool, VmError>> + Send + 'a>> {
        Box::pin(async move {
            let signal = normalize_signal(signal)?;
            self.dispatch_matching_interrupt_handlers(Some(signal))
                .await
        })
    }

    /// Run the handlers that want this cancellation. `None` is a cancellation
    /// that carries no signal, which only unfiltered handlers want.
    pub(crate) fn dispatch_matching_interrupt_handlers(
        &mut self,
        signal: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<bool, VmError>> + Send + '_>> {
        Box::pin(async move {
            self.interrupted = true;

            if self.dispatching_interrupt {
                // Already inside a dispatch: handlers are running for this
                // cancellation on the frame below.
                return Err(cancelled_error(HandlerDispatch::Dispatched));
            }

            let matching: Vec<(i64, bool, Option<u64>, VmValue)> = self
                .interrupt_handlers
                .iter()
                .rev()
                .filter(|entry| Self::handler_wants(entry, signal.as_deref()))
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

    /// One owner for "a cancellation has been observed, so run its handlers".
    ///
    /// A cancellation is noticed in several independent places: the
    /// between-ops poll, the op wrapper's timed-out branch, and the body of a
    /// blocking operation that watches the flag itself. Before this existed,
    /// each decided separately whether to dispatch, and the self-cancelling
    /// ones did not. A handler then ran only because the observer that skipped
    /// dispatch also left the signal in its slot for a later poll to find,
    /// which made a registered cleanup hook depend on the program continuing
    /// to execute after the throw. A program that stops there ran no handler
    /// and reported nothing.
    ///
    /// Every observer calls this before returning the cancelled error.
    ///
    /// The signal is put back when no handler wants it, because taking it is
    /// destructive and a path that does not dispatch must not be the path that
    /// destroys it. An empty slot yields no dispatch rather than a guess: a
    /// name that was never delivered matches no registered handler, so
    /// inventing one silently discards the handler it fails to match, and not
    /// every cancellation is a signal at all.
    ///
    /// Returns whether handlers ran.
    pub(crate) async fn dispatch_handlers_for_observed_cancel(&mut self) -> Result<bool, VmError> {
        // Replay is refused here, in the one function every observer calls,
        // rather than at each caller. Handlers ran when the run was live and
        // their effects are recorded; running them again repeats those
        // effects. The replay fact is read from its single owner.
        if crate::triggers::dispatcher::is_replay() {
            crate::cancellation::note_not_dispatched(
                crate::cancellation::NotDispatchedReason::ReplayPath,
            );
            return Ok(false);
        }

        let delivered = self
            .pending_interrupt_signal
            .take()
            .or_else(|| self.take_host_interrupt_signal());

        let Some(signal) = delivered else {
            // No signal was delivered. A host stopping the session cancels
            // this way, and a cleanup hook registered without a filter is
            // written for exactly that, so it still runs. A hook that asked
            // for particular signals cannot be matched against nothing, and
            // that skip is named rather than dropped.
            for (handle, wanted) in self.filtered_handlers_skipped_without_signal() {
                eprintln!(
                    "[harn] on_interrupt handler {handle} registered for {wanted} did not \
                     run: this cancellation carried no signal"
                );
            }
            if !self.has_unfiltered_interrupt_handler() {
                return Ok(false);
            }
            return self.dispatch_matching_interrupt_handlers(None).await;
        };

        if !self.has_interrupt_handler_for(&signal) {
            self.pending_interrupt_signal = Some(signal);
            return Ok(false);
        }
        self.dispatch_interrupt_handlers(&signal).await
    }

    pub(crate) async fn pending_scope_interrupt(&mut self) -> Option<VmError> {
        if let Some(code) = self.requested_process_exit() {
            self.cancel_spawned_tasks();
            return Some(VmError::ProcessExit(code));
        }

        if self
            .execution_deadline
            .current()
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.cancel_spawned_tasks();
            return Some(VmError::ExecutionDeadlineExceeded);
        }

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
            match self.dispatch_handlers_for_observed_cancel().await {
                Ok(true) => return None,
                Ok(false) => {}
                Err(error) => return Some(error),
            }

            match self.cancel_grace_instructions_remaining.as_mut() {
                Some(0) => {
                    // The owner was consulted above and reported that no
                    // handler matched; the decision was still its own.
                    self.cancel_spawned_tasks();
                    return Some(cancelled_error(HandlerDispatch::Dispatched));
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
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "kind:interrupted:{signal}"
        ))))
    }

    pub(crate) fn interrupt_handler_timeout_error() -> VmError {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "kind:interrupted:handler_timeout",
        )))
    }
}

/// `None` means the handler asked for no filter and runs on every observed
/// cancellation.
///
/// This used to default to SIGINT, which made the common registration - a
/// cleanup hook with no options - silently specific to one signal. A host
/// stopping a session cancels without any signal at all, so those hooks did
/// not run on the case they were mostly written for.
fn parse_signal_list(opts: Option<&VmValue>) -> Result<Option<Vec<String>>, VmError> {
    let Some(VmValue::Dict(opts)) = opts else {
        return Ok(None);
    };
    let Some(value) = opts.get("signals") else {
        return Ok(None);
    };
    match value {
        VmValue::Nil => Ok(None),
        VmValue::String(signal) => Ok(Some(vec![normalize_signal(signal.as_ref())?])),
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
            Ok(Some(out))
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
