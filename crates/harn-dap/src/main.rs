mod debugger;
mod protocol;

use std::io::{self, BufRead, Write};

use debugger::Debugger;
use protocol::{DapMessage, DapResponse};

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut debugger = Debugger::new();

    let reader = stdin.lock();
    let mut lines = reader.lines();

    while let Some(header) = read_header(&mut lines) {

        let content_length = header
            .strip_prefix("Content-Length: ")
            .and_then(|s| s.trim().parse::<usize>().ok())
            .unwrap_or(0);

        if content_length == 0 {
            continue;
        }

        // Skip empty line after header
        let _ = lines.next();

        // Read content body
        let mut body = String::new();
        let mut remaining = content_length;
        while remaining > 0 {
            if let Some(Ok(line)) = lines.next() {
                body.push_str(&line);
                body.push('\n');
                if body.len() >= content_length {
                    break;
                }
                remaining = content_length.saturating_sub(body.len());
            } else {
                break;
            }
        }
        let body = body.trim_end().to_string();

        // Parse and handle the message
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

fn read_header(lines: &mut io::Lines<io::StdinLock>) -> Option<String> {
    loop {
        let line = lines.next()?.ok()?;
        let trimmed = line.trim();
        if trimmed.starts_with("Content-Length:") {
            return Some(trimmed.to_string());
        }
        if trimmed.is_empty() {
            continue;
        }
    }
}

fn send_response(stdout: &mut io::Stdout, response: &DapResponse) {
    let body = serde_json::to_string(response).unwrap();
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdout.write_all(header.as_bytes()).ok();
    stdout.write_all(body.as_bytes()).ok();
    stdout.flush().ok();
}
