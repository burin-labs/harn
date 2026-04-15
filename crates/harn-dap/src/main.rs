mod debugger;
mod host_bridge;
mod protocol;

use std::collections::BTreeMap;
use std::io::{self, BufRead, Read, Write};
use std::sync::atomic::AtomicI64;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use debugger::Debugger;
use host_bridge::{deliver_reply, DapHostBridge, DapHostCallReply, PendingMap};
use protocol::{DapMessage, DapResponse};

fn main() {
    // Shared seq counter spans both forward responses (debugger.next_seq)
    // and reverse requests (DapHostBridge.next_seq) so every adapter-
    // initiated message uses a globally unique seq, matching the DAP spec.
    let seq = Arc::new(AtomicI64::new(1_000_000));

    // Stdout writer behind a mutex — both the main response loop and the
    // host bridge serialize their writes here.
    let stdout: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(Box::new(io::stdout())));
    let pending: PendingMap = Arc::new(Mutex::new(BTreeMap::new()));

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

    while let Ok(msg) = request_rx.recv() {
        let responses = debugger.handle_message(msg);
        for response in responses {
            send_response(&stdout, &response);
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

    while let Some(content_length) = read_content_length(&mut reader) {
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

fn read_content_length(reader: &mut io::BufReader<io::StdinLock>) -> Option<usize> {
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    if content_length > 0 {
                        return Some(content_length);
                    }
                    continue;
                }
                if let Some(val) = trimmed.strip_prefix("Content-Length:") {
                    if let Ok(len) = val.trim().parse::<usize>() {
                        content_length = len;
                    }
                }
            }
            Err(_) => return None,
        }
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
