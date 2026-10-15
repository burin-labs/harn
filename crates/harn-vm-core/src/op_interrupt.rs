//! Cooperative interrupt sources shared by the interpreter and host operations.

mod context;
pub use context::{
    install, installed, requested, with_deadline, InterruptSources, OpInterruptGuard,
};
pub use context::{operation_budget_expired, requested_error, with_operation_budget};

#[cfg(test)]
mod tests {
    use super::context::install_for_vm;
    use super::*;
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        time::{Duration, Instant},
    };

    #[test]
    fn requested_is_false_without_context() {
        assert!(!requested());
    }

    #[test]
    fn installed_tracks_guard_lifetime() {
        assert!(!installed());
        let guard = install(None, None);
        assert!(installed());
        drop(guard);
        assert!(!installed());
    }

    #[test]
    fn cancel_token_trips_requested_and_guard_restores() {
        let token = Arc::new(AtomicBool::new(false));
        let guard = install(Some(token.clone()), None);
        assert!(!requested());
        token.store(true, Ordering::SeqCst);
        assert!(requested());
        drop(guard);
        assert!(!requested());
    }

    #[test]
    fn deadline_trips_requested() {
        let expired = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("monotonic clock supports a 1ms test lookback");
        let _guard = install(None, Some(expired));
        assert!(requested());
    }

    #[test]
    fn requested_error_follows_the_vm_interrupt_precedence() {
        let expired = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("monotonic clock supports a 1ms test lookback");
        let thrown = || match requested_error() {
            Some(crate::VmError::Thrown(value)) => value.display(),
            other => panic!("expected a thrown interrupt error, got {other:?}"),
        };
        let cancel = Arc::new(AtomicBool::new(true));
        let _all = install_for_vm(Some(cancel.clone()), Some(expired), Some(expired));
        assert_eq!(thrown(), "kind:interrupted:handler_timeout");
        // An operation bound keeps the caller's handler window.
        let _bounded = with_deadline(Instant::now() + Duration::from_mins(1));
        assert_eq!(thrown(), "kind:interrupted:handler_timeout");
        let _cancel_and_scope = install_for_vm(Some(cancel), Some(expired), None);
        assert!(requested_error().is_some_and(|error| crate::cancellation::is_cancellation(&error)));
        let _scope = install_for_vm(None, Some(expired), None);
        assert_eq!(thrown(), "Deadline exceeded");
    }

    #[test]
    fn an_operation_budget_is_not_a_caller_interrupt() {
        let expired = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("monotonic clock supports a 1ms test lookback");
        let thrown = || match requested_error() {
            Some(crate::VmError::Thrown(value)) => value.display(),
            other => panic!("expected a thrown interrupt error, got {other:?}"),
        };
        {
            let _budget = with_operation_budget(expired);
            assert!(requested(), "an expired budget still stops blocking waits");
            if let Some(error) = requested_error() {
                panic!("an internal budget surfaced as the caller's interrupt: {error}");
            }
            assert!(operation_budget_expired());
        }
        assert!(!installed(), "the budget guard restores an empty slot");
        // Every caller interrupt keeps its own kind inside an expired budget.
        let cancel = Arc::new(AtomicBool::new(true));
        let _handler = install_for_vm(Some(cancel.clone()), Some(expired), Some(expired));
        let _budget = with_operation_budget(expired);
        assert!(!operation_budget_expired());
        assert_eq!(thrown(), "kind:interrupted:handler_timeout");
        let _cancelled = install_for_vm(Some(cancel), Some(expired), None);
        let _budget = with_operation_budget(expired);
        assert!(requested_error().is_some_and(|error| crate::cancellation::is_cancellation(&error)));
        let _scope = install_for_vm(None, Some(expired), None);
        let _budget = with_operation_budget(expired);
        assert_eq!(thrown(), "Deadline exceeded");
        // A live caller deadline is not shortened into a caller error.
        let _live = install_for_vm(None, Some(Instant::now() + Duration::from_mins(1)), None);
        let _budget = with_operation_budget(expired);
        assert!(operation_budget_expired());
        assert!(requested_error().is_none());
    }

    #[test]
    fn nested_installs_restore_in_order() {
        let outer_token = Arc::new(AtomicBool::new(true));
        let _outer = install(Some(outer_token), None);
        assert!(requested());
        {
            let _inner = install(None, None);
            assert!(!requested());
        }
        assert!(requested());
    }
}
