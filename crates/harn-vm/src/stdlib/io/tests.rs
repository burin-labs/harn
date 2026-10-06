use crate::value::VmDictExt;
use std::collections::BTreeMap;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::time::Instant;

use crate::value::VmValue;

use super::{
    mock_stdin_builtin, read_line_from_mock_or_real, read_stdin_builtin, render_progress_bar,
    render_progress_line, reserve_stdio_for_current_thread, reset_io_state, set_stdout_passthrough,
    spinner_frame, stdout_passthrough_enabled, ReadLineOptions, ReadLineOutcome, STDIN_ALLOWED,
    STDOUT_ALLOWED,
};

static_assertions::assert_not_impl_any!(super::StdioReservationGuard: Send, Sync);

#[test]
fn stdout_passthrough_state_toggles() {
    reset_io_state();

    assert!(!stdout_passthrough_enabled());
    assert!(!set_stdout_passthrough(true));
    assert!(stdout_passthrough_enabled());

    assert!(set_stdout_passthrough(false));
    assert!(!stdout_passthrough_enabled());
}

#[test]
fn scoped_stdio_reservation_is_repeatable_and_restores_prior_policy() {
    reset_io_state();
    assert!(STDIN_ALLOWED.get());
    assert!(STDOUT_ALLOWED.get());

    {
        let _outer = reserve_stdio_for_current_thread();
        assert_eq!(
            read_line_from_mock_or_real(&ReadLineOptions::default()),
            ReadLineOutcome::Eof
        );
        assert!(matches!(
            read_stdin_builtin(&[], &mut String::new()).unwrap(),
            VmValue::Nil
        ));
        assert!(matches!(
            read_stdin_builtin(&[], &mut String::new()).unwrap(),
            VmValue::Nil
        ));
        mock_stdin_builtin(&[VmValue::string("fixture")], &mut String::new()).unwrap();
        assert_eq!(
            read_stdin_builtin(&[], &mut String::new())
                .unwrap()
                .display(),
            "fixture"
        );
        assert!(matches!(
            read_stdin_builtin(&[], &mut String::new()).unwrap(),
            VmValue::Nil
        ));
        {
            let _inner = reserve_stdio_for_current_thread();
            assert!(!STDIN_ALLOWED.get());
            assert!(!STDOUT_ALLOWED.get());
        }
        assert!(!STDIN_ALLOWED.get());
        assert!(!STDOUT_ALLOWED.get());
    }

    assert!(STDIN_ALLOWED.get());
    assert!(STDOUT_ALLOWED.get());
}

#[test]
fn progress_bar_mode_renders_hash_bar() {
    let mut options = BTreeMap::new();
    options.put_str("mode", "bar");
    options.insert("current".to_string(), VmValue::Int(3));
    options.insert("total".to_string(), VmValue::Int(5));
    options.insert("width".to_string(), VmValue::Int(10));

    let line = render_progress_line(&[
        VmValue::String(arcstr::ArcStr::from("build")),
        VmValue::String(arcstr::ArcStr::from("Compiling")),
        VmValue::dict(options),
    ]);

    assert_eq!(line, "[build] [######----] Compiling (3/5)\n");
}

#[test]
fn progress_spinner_mode_uses_step_to_pick_frame() {
    let mut options = BTreeMap::new();
    options.put_str("mode", "spinner");
    options.insert("step".to_string(), VmValue::Int(2));

    let line = render_progress_line(&[
        VmValue::String(arcstr::ArcStr::from("sync")),
        VmValue::String(arcstr::ArcStr::from("Waiting")),
        VmValue::dict(options),
    ]);

    assert_eq!(line, "[sync] - Waiting\n");
    assert_eq!(spinner_frame(3), "\\");
}

#[test]
fn progress_bar_falls_back_to_empty_bar_for_zero_total() {
    assert_eq!(render_progress_bar(2, 0, 5), "[-----]");
}

#[test]
fn read_line_options_preserve_prompt_whitespace() {
    let mut options = BTreeMap::new();
    options.put_str("prompt", "  > ");
    options.insert("trim".to_string(), VmValue::Bool(false));

    let parsed = super::parse_read_line_options(&[VmValue::dict(options)]).unwrap();

    assert_eq!(parsed.prompt, "  > ");
    assert!(!parsed.trim);
}

#[test]
fn read_line_options_reject_unknown_keys() {
    let mut options = BTreeMap::new();
    options.put_str("promtp", "> ");

    let err = super::parse_read_line_options(&[VmValue::dict(options)]).unwrap_err();

    match err {
        crate::value::VmError::Runtime(message) => assert!(message.contains("promtp")),
        other => panic!("expected Runtime error, got {other:?}"),
    }
}

#[cfg(unix)]
struct FdGuard(libc::c_int);

#[cfg(unix)]
impl Drop for FdGuard {
    fn drop(&mut self) {
        let _ = unsafe { libc::close(self.0) };
    }
}

#[cfg(unix)]
fn pipe_pair() -> (FdGuard, FdGuard) {
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    (FdGuard(fds[0]), FdGuard(fds[1]))
}

#[cfg(unix)]
#[test]
fn read_line_from_fd_times_out_without_data() {
    let (read_fd, _write_fd) = pipe_pair();
    let outcome = super::read_line_from_fd_unix(
        read_fd.0,
        &ReadLineOptions {
            timeout_ms: Some(10),
            ..ReadLineOptions::default()
        },
    );

    assert_eq!(outcome, ReadLineOutcome::Timeout);
}

#[cfg(unix)]
#[test]
fn read_line_from_fd_observes_interrupt_without_stdin_activity() {
    let (read_fd, _write_fd) = pipe_pair();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_from_thread = Arc::clone(&cancel);
    let _guard = crate::op_interrupt::install(Some(cancel), None);
    let interrupter = crate::runtime_stack::spawn(move || {
        // No fixed "wait for the reader to park" sleep: the reader re-checks
        // the cancel flag every `READ_LINE_INTERRUPT_POLL` heartbeat, so it
        // observes this store within one interval regardless of ordering.
        // A blind sleep would only add wall time and reintroduce a race.
        cancel_from_thread.store(true, Ordering::SeqCst);
    });

    let started = Instant::now();
    let outcome = super::read_line_from_fd_unix(read_fd.0, &ReadLineOptions::default());

    interrupter.join().expect("interrupter thread joins");
    assert_eq!(outcome, ReadLineOutcome::Interrupt);
    assert!(
        started.elapsed() < super::READ_LINE_INTERRUPT_POLL * 25,
        "interrupt heartbeat should wake idle read_line within a few poll \
             intervals, took {:?}",
        started.elapsed()
    );
}

#[cfg(unix)]
#[test]
fn read_line_from_fd_honors_trim_option() {
    let (read_fd, write_fd) = pipe_pair();
    let payload = b"  alpha  \n";
    assert_eq!(
        unsafe { libc::write(write_fd.0, payload.as_ptr().cast(), payload.len()) },
        payload.len() as isize
    );
    let outcome = super::read_line_from_fd_unix(
        read_fd.0,
        &ReadLineOptions {
            timeout_ms: Some(100),
            trim: false,
            ..ReadLineOptions::default()
        },
    );

    assert_eq!(outcome, ReadLineOutcome::Ok("  alpha  ".to_string()));
}
