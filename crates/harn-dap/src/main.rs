mod debugger;
mod protocol;

use std::io::{self, BufRead, Read, Write};

use debugger::Debugger;
use protocol::{DapMessage, DapResponse};

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut debugger = Debugger::new();

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
                let responses = debugger.handle_message(msg);
                for response in responses {
                    send_response(&mut stdout, &response);
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
                    // LSP-style framing: blank line ends the header block.
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

fn send_response(stdout: &mut io::Stdout, response: &DapResponse) {
    if let Ok(body) = serde_json::to_string(response) {
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        let _ = stdout.write_all(header.as_bytes());
        let _ = stdout.write_all(body.as_bytes());
        let _ = stdout.flush();
    }
}
