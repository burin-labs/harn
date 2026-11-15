use super::*;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

struct RecordingKiller {
    calls: AtomicUsize,
}

impl RecordingKiller {
    fn calls(&self) -> usize {
        self.calls.load(AtomicOrdering::SeqCst)
    }
}

impl ProcessKiller for RecordingKiller {
    fn kill(&self) -> process_handle::ProcessCleanupReport {
        self.calls.fetch_add(1, AtomicOrdering::SeqCst);
        process_handle::ProcessCleanupReport::for_signal(Some(42), 9)
    }
}

fn recording_killer() -> Arc<RecordingKiller> {
    Arc::new(RecordingKiller {
        calls: AtomicUsize::new(0),
    })
}

#[test]
fn output_reader_preserves_read_failure_beside_partial_bytes() {
    struct PartialFailure(bool);
    impl Read for PartialFailure {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if std::mem::replace(&mut self.0, true) {
                return Err(std::io::Error::other("controlled read failure"));
            }
            buffer[..5].copy_from_slice(b"ready");
            Ok(5)
        }
    }
    let (receiver, thread) = spawn_reader_thread(Some(Box::new(PartialFailure(false))));
    let captured =
        harn_clock::test_support::recv_within("partial reader terminal result", &receiver.unwrap());
    thread.unwrap().join().unwrap();
    assert_eq!(captured.bytes, b"ready");
    assert!(captured.failed);
    let (receiver, thread) = spawn_reader_thread(Some(Box::new(std::io::Cursor::new(b"ready"))));
    let captured = harn_clock::test_support::recv_within(
        "successful reader terminal result",
        &receiver.unwrap(),
    );
    thread.unwrap().join().unwrap();
    assert_eq!(captured.bytes, b"ready");
    assert!(!captured.failed);
}

#[test]
fn absent_output_reader_does_not_kill_a_completed_command_after_its_deadline() {
    let (receiver, thread) = spawn_reader_thread(None);
    assert!(thread.is_none());
    let killer = recording_killer();
    let killer_trait: Arc<dyn ProcessKiller> = killer.clone();
    let expired = std::time::Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap();

    let result = drain_output(receiver, Some(expired), false, &killer_trait);

    assert_eq!(killer.calls(), 0);
    assert!(result.bytes.is_empty());
    assert!(!result.read_failed);
    assert!(!result.timed_out);
    assert!(!result.interrupted);
    assert!(result.process_cleanup.is_none());
}

#[test]
fn drain_output_kills_immediately_when_deadline_already_elapsed() {
    let (_tx, rx) = mpsc::channel::<ReadCapture>();
    let killer = recording_killer();
    let killer_trait: Arc<dyn ProcessKiller> = killer.clone();
    let expired_deadline = std::time::Instant::now()
        .checked_sub(Duration::from_millis(1))
        .expect("subtracting one millisecond from Instant::now() should be representable");

    let result = drain_output(Some(rx), Some(expired_deadline), false, &killer_trait);

    assert!(result.timed_out);
    assert!(!result.interrupted);
    assert_eq!(result.bytes, Vec::<u8>::new());
    assert_eq!(killer.calls(), 1);
    assert_eq!(result.process_cleanup.unwrap().root_pid, Some(42));
}

#[test]
fn drain_output_collects_all_available_chunks() {
    let (tx, rx) = mpsc::channel::<ReadCapture>();
    tx.send(ReadCapture {
        bytes: b"alpha".to_vec(),
        failed: false,
    })
    .unwrap();
    tx.send(ReadCapture {
        bytes: b"beta".to_vec(),
        failed: false,
    })
    .unwrap();
    drop(tx);
    let killer = recording_killer();
    let killer_trait: Arc<dyn ProcessKiller> = killer.clone();

    let result = drain_output(Some(rx), None, false, &killer_trait);

    assert_eq!(result.bytes, b"alphabeta");
    assert!(!result.timed_out);
    assert!(!result.interrupted);
    assert!(result.process_cleanup.is_none());
    assert_eq!(killer.calls(), 0);
}
