#![recursion_limit = "256"]

mod debugger;
mod host_bridge;
mod protocol;

use std::io::{self, BufRead, Read, Write};
use std::sync::atomic::AtomicI64;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use debugger::Debugger;
use host_bridge::{deliver_reply, pending_map_new, DapHostBridge, DapHostCallReply, PendingMap};
use protocol::{DapMessage, DapResponse};

const MAX_DAP_FRAME_BYTES: usize = 16 * 1024 * 1024;

fn main() {
    // Defeat rlib dead-code stripping of the linkme distributed slice
    // (linkme issue #36) before reading `all_builtin_signatures()`.
    harn_vm::stdlib::force_link();
    // Install the macro-emitted builtin signature slice into the parser
    // registry so source-file typechecking inside DAP launch covers
    // `#[harn_builtin]`-migrated entries.
    harn_parser::install_builtin_signatures(harn_vm::stdlib::all_builtin_signatures());

    // Shared seq counter spans both forward responses (debugger.next_seq)
    // and reverse requests (DapHostBridge.next_seq) so every adapter-
    // initiated message uses a globally unique seq, matching the DAP spec.
    let seq = Arc::new(AtomicI64::new(1_000_000));

    // Stdout writer behind a mutex — both the main response loop and the
    // host bridge serialize their writes here.
    let stdout: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(Box::new(io::stdout())));
    let pending: PendingMap = pending_map_new();

    // Stdin reader runs on its own OS thread so the bridge can block on
    // reverse-request replies without starving the read loop.
    let (request_tx, request_rx) = channel::<DapMessage>();
    let pending_for_reader = Arc::clone(&pending);
    thread::spawn(move || stdin_reader(request_tx, pending_for_reader));

    let bridge = Arc::new(DapHostBridge::new(
        Arc::clone(&seq),
        Arc::clone(&stdout),
        Arc::clone(&pending),
    ));

    let mut debugger = Debugger::new();
    debugger.attach_host_bridge(Arc::clone(&bridge));

    // Interleaved drive loop. Two phases per iteration:
    //   1. Drain any pending DAP messages from the channel (try_recv —
    //      non-blocking) so commands like pause / disconnect /
    //      setBreakpoints get serviced even mid-run.
    //   2. If the debugger is in a "running" state (after continue /
    //      next / stepIn / stepOut / configurationDone), take ONE VM
    //      step and emit any events. Otherwise block waiting for the
    //      next message — we don't busy-loop while idle.
    //
    // This is what makes pause work during long scripts. The previous
    // model called run_to_breakpoint() inside handle_continue, which
    // monopolized the main thread until the VM voluntarily stopped;
    // any pause / disconnect arriving in the meantime sat in the
    // channel ignored.
    use std::sync::mpsc::TryRecvError;
    loop {
        // Phase 1: drain pending messages.
        loop {
            match request_rx.try_recv() {
                Ok(msg) => {
                    let responses = debugger.handle_message(msg);
                    for response in responses {
                        send_response(&stdout, &response);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        // Phase 2: step the VM if running, else block on next message.
        if debugger.is_running() {
            let responses = debugger.step_running_vm();
            for response in responses {
                send_response(&stdout, &response);
            }
        } else {
            match request_rx.recv() {
                Ok(msg) => {
                    let responses = debugger.handle_message(msg);
                    for response in responses {
                        send_response(&stdout, &response);
                    }
                }
                Err(_) => return,
            }
        }
    }
}

/// Stdin reader: parses LSP-framed DAP messages and demuxes by `type`.
/// `request` and `event`-typed frames flow into the debugger via
/// `request_tx`. `response`-typed frames are matched against pending
/// reverse requests and routed into the bridge's reply channels.
fn stdin_reader(request_tx: Sender<DapMessage>, pending: PendingMap) {
    let stdin = io::stdin();
    let mut reader = io::BufReader::new(stdin.lock());

    loop {
        let content_length = match read_content_length(&mut reader) {
            Ok(Some(content_length)) => content_length,
            Ok(None) => break,
            Err(error) => {
                eprintln!("Failed to read DAP frame header: {error}");
                break;
            }
        };
        if content_length == 0 {
            continue;
        }
        let mut body_bytes = vec![0u8; content_length];
        if reader.read_exact(&mut body_bytes).is_err() {
            break;
        }
        let body = String::from_utf8_lossy(&body_bytes);

        match serde_json::from_str::<DapMessage>(&body) {
            Ok(msg) => {
                if msg.msg_type == "response" {
                    if let Some(request_seq) = msg.request_seq {
                        deliver_reply(
                            &pending,
                            request_seq,
                            DapHostCallReply {
                                success: msg.success.unwrap_or(false),
                                body: msg.body,
                                message: msg.message,
                            },
                        );
                        continue;
                    }
                }
                if request_tx.send(msg).is_err() {
                    break;
                }
            }
            Err(e) => {
                eprintln!("Failed to parse DAP message: {e}");
                eprintln!("Body: {body}");
            }
        }
    }
}

fn read_content_length<R: BufRead>(reader: &mut R) -> io::Result<Option<usize>> {
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return Ok(None),
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    if content_length > 0 {
                        return Ok(Some(content_length));
                    }
                    continue;
                }
                if let Some((name, val)) = trimmed.split_once(':') {
                    if !name.eq_ignore_ascii_case("Content-Length") {
                        continue;
                    }
                    if let Ok(len) = val.trim().parse::<usize>() {
                        content_length = bounded_dap_content_length(len)?;
                    }
                }
            }
            Err(error) => return Err(error),
        }
    }
}

fn bounded_dap_content_length(content_length: usize) -> io::Result<usize> {
    if content_length > MAX_DAP_FRAME_BYTES {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "DAP Content-Length {content_length} exceeds limit {MAX_DAP_FRAME_BYTES} bytes"
            ),
        ))
    } else {
        Ok(content_length)
    }
}

fn send_response(stdout: &Arc<Mutex<Box<dyn Write + Send>>>, response: &DapResponse) {
    let body = match serde_json::to_string(response) {
        Ok(b) => b,
        Err(_) => return,
    };
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut guard = match stdout.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let _ = guard.write_all(header.as_bytes());
    let _ = guard.write_all(body.as_bytes());
    let _ = guard.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_content_length_accepts_case_insensitive_header() {
        let mut reader = io::BufReader::new(b"content-length: 12\r\n\r\n".as_slice());
        assert_eq!(
            read_content_length(&mut reader).expect("content length"),
            Some(12)
        );
    }

    #[test]
    fn read_content_length_rejects_oversized_frame() {
        let header = format!("Content-Length: {}\r\n\r\n", MAX_DAP_FRAME_BYTES + 1);
        let mut reader = io::BufReader::new(header.as_bytes());
        let error = read_content_length(&mut reader).expect_err("oversized frame");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
