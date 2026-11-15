use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use harn_vm::session_inputs::{OutputFilter, StreamRedactor};

use super::OutputFeed;
use crate::tools::proc::stage_output::StageReaders;

pub(super) struct DrainContext<'a> {
    pub(super) filter: &'a OutputFilter,
    pub(super) feed: Arc<OutputFeed>,
    pub(super) stdout_path: PathBuf,
    pub(super) stderr_path: PathBuf,
    pub(super) combined_file: Option<Arc<Mutex<std::fs::File>>>,
}

pub(super) struct CompletedOutput {
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
    #[cfg(unix)]
    pub(super) stage_stdout: Vec<u8>,
    #[cfg(unix)]
    pub(super) stage_stderr: Vec<u8>,
    pub(super) capture_read_failed: bool,
}

/// Snapshot the output after the owning waiter has joined every drain.
pub(super) fn completed(feed: &OutputFeed) -> CompletedOutput {
    let state = feed
        .state
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    CompletedOutput {
        stdout: state.stdout.clone(),
        stderr: state.stderr.clone(),
        #[cfg(unix)]
        stage_stdout: state.stage_stdout.clone(),
        #[cfg(unix)]
        stage_stderr: state.stage_stderr.clone(),
        capture_read_failed: state.capture_read_failed,
    }
}

struct LogicalStream {
    redactor: StreamRedactor,
    readers: usize,
    file: Option<std::fs::File>,
    combined_file: Option<Arc<Mutex<std::fs::File>>>,
    feed: Arc<OutputFeed>,
    stdout: bool,
}

impl LogicalStream {
    fn deliver(&mut self, chunk: &[u8]) {
        if let Some(file) = self.file.as_mut() {
            let _ = file.write_all(chunk);
        }
        let mut state = self.feed.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(combined) = self.combined_file.as_ref() {
            let mut file = combined.lock().unwrap_or_else(|p| p.into_inner());
            let _ = file.write_all(chunk);
        }
        if self.stdout {
            state.stdout.extend_from_slice(chunk);
        } else {
            state.stderr.extend_from_slice(chunk);
        }
        state.combined.extend_from_slice(chunk);
        drop(state);
        self.feed.notify.notify_waiters();
    }
}

pub(super) fn spawn_drains(
    stdout: Option<Box<dyn Read + Send>>,
    stderr: Option<Box<dyn Read + Send>>,
    stage: Option<StageReaders>,
    context: DrainContext<'_>,
) -> Vec<std::thread::JoinHandle<()>> {
    let (stage_stdout, stage_stderr) = match stage {
        Some(stage) => (Some(stage.stdout), Some(stage.stderr)),
        None => (None, None),
    };
    let mut threads = Vec::new();
    for (stdout_stream, ordinary, stage, path) in [
        (true, stdout, stage_stdout, context.stdout_path),
        (false, stderr, stage_stderr, context.stderr_path),
    ] {
        let shared = Arc::new(Mutex::new(LogicalStream {
            redactor: context.filter.stream(),
            readers: usize::from(ordinary.is_some()) + usize::from(stage.is_some()),
            file: std::fs::OpenOptions::new().append(true).open(path).ok(),
            combined_file: context.combined_file.clone(),
            feed: context.feed.clone(),
            stdout: stdout_stream,
        }));
        for (reader, stage_stream) in [(ordinary, false), (stage, true)] {
            if let Some(reader) = reader {
                threads.push(spawn_drain(
                    reader,
                    shared.clone(),
                    stage_stream.then(|| context.filter.stream()),
                ));
            }
        }
    }
    threads
}

fn deliver_stage(stream: &LogicalStream, chunk: &[u8]) {
    let mut state = stream.feed.state.lock().unwrap_or_else(|p| p.into_inner());
    if stream.stdout {
        state.stage_stdout.extend_from_slice(chunk);
    } else {
        state.stage_stderr.extend_from_slice(chunk);
    }
}

fn spawn_drain(
    mut reader: Box<dyn Read + Send>,
    shared: Arc<Mutex<LogicalStream>>,
    mut stage_redactor: Option<StreamRedactor>,
) -> std::thread::JoinHandle<()> {
    harn_parser::runtime_stack::spawn(move || {
        let mut buffer = [0; 8192];
        loop {
            let read = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => {
                    let stream = shared.lock().unwrap_or_else(|p| p.into_inner());
                    stream
                        .feed
                        .state
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .capture_read_failed = true;
                    break;
                }
            };
            let mut stream = shared.lock().unwrap_or_else(|p| p.into_inner());
            stream
                .feed
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .last_output_at = Some(std::time::Instant::now());
            if let Some(redactor) = stage_redactor.as_mut() {
                deliver_stage(&stream, &redactor.push(&buffer[..read]));
            } else {
                let chunk = stream.redactor.push(&buffer[..read]);
                stream.deliver(&chunk);
            }
        }
        let mut stream = shared.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(redactor) = stage_redactor.as_mut() {
            deliver_stage(&stream, &redactor.finish());
        }
        stream.readers -= 1;
        if stream.readers == 0 {
            let tail = stream.redactor.finish();
            stream.deliver(&tail);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    struct OrderedReader {
        bytes: Option<&'static [u8]>,
        before_first: Option<mpsc::Receiver<()>>,
        after_first: Option<mpsc::Sender<()>>,
        before_eof: Option<mpsc::Receiver<()>>,
    }

    impl Read for OrderedReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if let Some(bytes) = self.bytes.take() {
                if let Some(receiver) = self.before_first.take() {
                    harn_clock::test_support::recv_within(
                        "earlier stream chunk delivered",
                        &receiver,
                    );
                }
                buffer[..bytes.len()].copy_from_slice(bytes);
                return Ok(bytes.len());
            }
            if let Some(sender) = self.after_first.take() {
                sender.send(()).unwrap();
            }
            if let Some(receiver) = self.before_eof.take() {
                harn_clock::test_support::recv_within("other stream chunk delivered", &receiver);
            }
            Ok(0)
        }
    }

    #[test]
    fn prefix_and_final_stage_share_aggregate_redaction_without_sharing_stage_bytes() {
        assert_aggregate_redaction(false);
    }

    #[test]
    fn stage_read_ahead_cannot_reorder_prefix_or_bypass_aggregate_redaction() {
        assert_aggregate_redaction(true);
    }

    fn assert_aggregate_redaction(stage_first: bool) {
        let session = harn_vm::agent_sessions::open_or_create(None).unwrap();
        let current = harn_vm::agent_sessions::enter_current_session(session.clone());
        let secret = "final-stage-private-canary";
        assert!(harn_vm::session_inputs::filter_granted_value(secret));
        let filter = OutputFilter::capture_for_spawn();
        let feed = Arc::new(OutputFeed::default());
        let stdout = tempfile::NamedTempFile::new().unwrap();
        let stderr = tempfile::NamedTempFile::new().unwrap();
        let (prefix_tx, prefix_rx) = mpsc::channel();
        let (stage_tx, stage_rx) = mpsc::channel();
        let (prefix_before, prefix_after, prefix_eof, stage_before, stage_after) = if stage_first {
            (Some(stage_rx), None, None, None, Some(stage_tx))
        } else {
            (
                None,
                Some(prefix_tx),
                Some(stage_rx),
                Some(prefix_rx),
                Some(stage_tx),
            )
        };
        let prefix = OrderedReader {
            // The guardian relay has already put both raw writes on this
            // one aggregate stream; the private stage reader may run first.
            bytes: Some(secret.as_bytes()),
            before_first: prefix_before,
            after_first: prefix_after,
            before_eof: prefix_eof,
        };
        let stage = OrderedReader {
            bytes: Some(b"canary"),
            before_first: stage_before,
            after_first: stage_after,
            before_eof: None,
        };
        let drains = spawn_drains(
            Some(Box::new(prefix)),
            None,
            Some(StageReaders {
                stdout: Box::new(stage),
                stderr: Box::new(std::io::empty()),
            }),
            DrainContext {
                filter: &filter,
                feed: feed.clone(),
                stdout_path: stdout.path().to_owned(),
                stderr_path: stderr.path().to_owned(),
                combined_file: None,
            },
        );
        for drain in drains {
            drain.join().unwrap();
        }
        let state = feed.state.lock().unwrap();
        assert_eq!(
            state.stdout,
            filter.redact_bytes(secret.as_bytes()).as_ref()
        );
        assert_eq!(std::fs::read(stdout.path()).unwrap(), state.stdout);
        assert_eq!(state.stage_stdout, b"canary");
        assert!(!state.capture_read_failed);
        drop(state);
        drop(current);
        harn_vm::agent_sessions::close(&session);
    }

    #[test]
    fn ordinary_reader_failure_cannot_supply_complete_invocation_capture() {
        struct PartialReader(bool);
        impl Read for PartialReader {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if std::mem::replace(&mut self.0, false) {
                    buffer[..7].copy_from_slice(b"partial");
                    Ok(7)
                } else {
                    Err(std::io::Error::other("controlled read failure"))
                }
            }
        }
        for failed in [false, true] {
            let feed = Arc::new(OutputFeed::default());
            let stdout = tempfile::NamedTempFile::new().unwrap();
            let stderr = tempfile::NamedTempFile::new().unwrap();
            let reader: Box<dyn Read + Send> = if failed {
                Box::new(PartialReader(true))
            } else {
                Box::new(std::io::Cursor::new(b"partial"))
            };
            let drains = spawn_drains(
                Some(reader),
                None,
                None,
                DrainContext {
                    filter: &OutputFilter::capture_for_spawn(),
                    feed: feed.clone(),
                    stdout_path: stdout.path().to_owned(),
                    stderr_path: stderr.path().to_owned(),
                    combined_file: None,
                },
            );
            for drain in drains {
                drain.join().unwrap();
            }
            let state = feed.state.lock().unwrap();
            assert_eq!(state.stdout, b"partial");
            assert_eq!(state.capture_read_failed, failed);
        }
    }
}
