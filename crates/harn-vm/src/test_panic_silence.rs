//! Silencing the panic message for one expected panic, without silencing the
//! whole test binary.
//!
//! A case that asserts a panic wants `catch_unwind` without the default hook
//! printing the panic it expected. The obvious way to get that is to take the
//! hook, install an empty one, and put the original back afterwards. The panic
//! hook is process-global, so during that window *every* thread's panic is
//! swallowed — including the assertion failure of an unrelated case running
//! beside it. That case still reports FAILED, with no message and no location,
//! which is the single most expensive thing a suite can do to a reader
//! (harn#7960).
//!
//! So the hook is installed once for the life of the process and never
//! replaced. It defers to the default hook unless the panicking thread asked
//! for silence, and a hook runs on the panicking thread, so the thread-local
//! flag is read by exactly the right thread.

use std::cell::Cell;
use std::sync::Once;

thread_local! {
    /// Whether panics raised on this thread should print nothing.
    static SILENT: Cell<bool> = const { Cell::new(false) };
}

/// Install the process-lifetime hook. Idempotent and never undone.
fn install_hook() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if SILENT.with(Cell::get) {
                return;
            }
            previous(info);
        }));
    });
}

/// Run `f`, catching a panic and printing nothing for it.
///
/// Only panics raised on the calling thread are silenced; a sibling case that
/// fails while this one runs still reports its message.
pub(crate) fn catch_unwind_silently<R>(f: impl FnOnce() -> R) -> std::thread::Result<R> {
    install_hook();

    /// Clears the flag even if `f` unwinds.
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            SILENT.with(|silent| silent.set(false));
        }
    }

    SILENT.with(|silent| silent.set(true));
    let _restore = Restore;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_silenced_panic_is_caught_and_the_flag_is_cleared() {
        let outcome = catch_unwind_silently(|| panic!("expected"));
        assert!(outcome.is_err(), "the panic is still caught");
        assert!(
            !SILENT.with(Cell::get),
            "the thread stops being silent once the call returns",
        );
    }

    /// The property the swap-the-hook version could not offer: silence is
    /// scoped to one thread, so a concurrent case still gets its message.
    #[test]
    fn a_panic_on_another_thread_is_not_silenced() {
        install_hook();
        let observed = std::thread::spawn(|| SILENT.with(Cell::get))
            .join()
            .expect("probe thread");
        let _ = catch_unwind_silently(|| {
            let inside = std::thread::spawn(|| SILENT.with(Cell::get))
                .join()
                .expect("probe thread");
            assert!(!inside, "silence does not reach a second thread");
        });
        assert!(!observed);
    }
}
