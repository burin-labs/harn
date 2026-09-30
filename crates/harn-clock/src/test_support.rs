//! One hang ceiling for tests that wait on an event.
//!
//! A test that waits for something a running component produces by
//! construction (a terminal task event, a JSON-RPC reply, a watcher publish)
//! needs a bound only so that a hang fails instead of wedging the suite. That
//! bound is [`HANG_CEILING`], and it is the same for every such wait.
//!
//! The ceiling detects hangs. It never asserts latency. A tight literal such
//! as `timeout(Duration::from_secs(2), rx.recv())` turns "this event arrives"
//! into "this event arrives within two seconds on whatever machine runs the
//! suite", which fails under CPU load while the code is correct. A claim about
//! how long something takes belongs in a paused-clock test
//! ([`crate::PausedClock`] or `#[tokio::test(start_paused = true)]`), where
//! the duration is exact and load cannot move it.
//!
//! The deadline is wall-clock time kept by a helper thread, not the tokio
//! timer. A paused tokio clock auto-advances whenever the runtime is idle, so
//! a tokio timeout around an event produced by an OS thread would expire at
//! once under `start_paused`; the wall-clock deadline cannot. It also works on
//! a runtime built without the time driver.
//!
//! Enabled by the `test-support` feature. Crates depend on it only from
//! `[dev-dependencies]`.

use std::future::Future;
use std::pin::{pin, Pin};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::task::Poll;
use std::time::{Duration, Instant};

/// The longest any in-process test waits for an event before calling it a
/// hang.
///
/// 45 seconds sits below the shortest per-test hard kill in
/// `.config/nextest.toml`: the default and `flake-detection` profiles
/// terminate a test after 15s x 4 = 60s, and every other profile and override
/// allows longer. A hang therefore fails here, with the label naming what was
/// awaited, before nextest kills the process without one. It is also far
/// above the short budgets it replaces, leaving headroom for compilation
/// and OS-thread scheduling under load rather than asserting performance.
pub const HANG_CEILING: Duration = Duration::from_secs(45);

/// Await `fut`, panicking if it has not completed within [`HANG_CEILING`].
///
/// `label` names what the test is waiting for; it appears in the panic along
/// with the elapsed time. Use this only around events that arrive by
/// construction. When the expiry itself is the behavior under test, keep an
/// explicit timeout on a paused clock instead.
pub async fn within<F: Future>(label: &str, fut: F) -> F::Output {
    within_limit(label, fut, HANG_CEILING).await
}

async fn within_limit<F: Future>(label: &str, fut: F, ceiling: Duration) -> F::Output {
    let started = Instant::now();
    let (expired_tx, mut expired_rx) = tokio::sync::oneshot::channel::<()>();
    // Dropping `_cancel` when this future completes or is dropped wakes the
    // deadline thread so it exits without waiting out the ceiling.
    let (_cancel, cancelled) = mpsc::channel::<()>();
    std::thread::Builder::new()
        .name("harn-hang-ceiling".to_string())
        .spawn(move || {
            if cancelled.recv_timeout(ceiling) == Err(RecvTimeoutError::Timeout) {
                let _ = expired_tx.send(());
            }
        })
        .expect("start hang-ceiling deadline thread");

    let mut fut = pin!(fut);
    std::future::poll_fn(|cx| {
        if let Poll::Ready(output) = fut.as_mut().poll(cx) {
            return Poll::Ready(output);
        }
        if Pin::new(&mut expired_rx).poll(cx) == Poll::Ready(Ok(())) {
            panic!(
                "hang: {label} did not complete within the {ceiling:?} test hang ceiling \
                 (waited {:?})",
                started.elapsed()
            );
        }
        Poll::Pending
    })
    .await
}

/// Block on `rx` until a value arrives, panicking after [`HANG_CEILING`] or
/// if every sender is dropped first.
///
/// The blocking-channel counterpart of [`within`], for events produced on OS
/// threads. `label` names what the test is waiting for.
pub fn recv_within<T>(label: &str, rx: &mpsc::Receiver<T>) -> T {
    let started = Instant::now();
    match rx.recv_timeout(HANG_CEILING) {
        Ok(value) => value,
        Err(RecvTimeoutError::Timeout) => panic!(
            "hang: {label} did not arrive within the {HANG_CEILING:?} test hang ceiling \
             (waited {:?})",
            started.elapsed()
        ),
        Err(RecvTimeoutError::Disconnected) => panic!(
            "{label}: channel closed before a value arrived (waited {:?})",
            started.elapsed()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn within_returns_the_output_of_a_ready_future() {
        assert_eq!(within("ready value", async { 7 }).await, 7);
    }

    #[test]
    #[should_panic(expected = "hang: missing event did not complete within")]
    fn within_names_a_hang_without_a_tokio_timer() {
        // A pending future cannot win the race, and a zero deadline requires
        // no real-time delay or virtual-time advancement.
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime without a time driver")
            .block_on(within_limit(
                "missing event",
                std::future::pending::<()>(),
                Duration::ZERO,
            ));
    }

    /// A paused tokio clock auto-advances past any tokio timer while the
    /// runtime idles. The ceiling must not: an event produced by an OS thread
    /// still arrives, however far virtual time has jumped meanwhile.
    #[tokio::test(start_paused = true)]
    async fn within_ignores_paused_tokio_time() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let producer = std::thread::spawn(move || {
            if go_rx.recv().is_ok() {
                let _ = tx.send("done");
            }
        });
        let value = within("event from an OS thread", async {
            // Move virtual time far past the ceiling, then let the OS thread
            // produce while the runtime idles with no timer pending.
            tokio::time::advance(HANG_CEILING * 4).await;
            go_tx.send(()).expect("producer waiting");
            rx.await.expect("sender kept")
        })
        .await;
        assert_eq!(value, "done");
        producer.join().expect("producer thread");
    }

    #[test]
    fn recv_within_returns_a_sent_value() {
        let (tx, rx) = mpsc::channel();
        tx.send(3).expect("send");
        assert_eq!(recv_within("sent value", &rx), 3);
    }

    #[test]
    #[should_panic(expected = "closed channel: channel closed before a value arrived")]
    fn recv_within_names_a_closed_channel() {
        let (tx, rx) = mpsc::channel::<()>();
        drop(tx);
        recv_within("closed channel", &rx);
    }
}
