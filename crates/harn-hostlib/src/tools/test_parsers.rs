//! Native test-output projections. JUnit decoding and case records are owned
//! by the runtime; plain cargo/go diagnostics remain lenient display helpers.

pub(crate) use harn_vm::stdlib::junit::{
    duration_seconds_to_ms, parse_junit_xml, Status, TestRecord,
};

/// Parse cargo's libtest diagnostics, including failure bodies.
pub(crate) fn parse_cargo_libtest(stdout: &str) -> Vec<TestRecord> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let Some(body) = line.trim_start().strip_prefix("test ") else {
            continue;
        };
        if body.starts_with("result:") {
            continue;
        }
        let Some((name, tail)) = body.split_once(" ... ") else {
            continue;
        };
        let status = match tail.trim() {
            "ok" => Status::Passed,
            "FAILED" => Status::Failed,
            "ignored" | "skipped" => Status::Skipped,
            _ if tail.starts_with("ok") => Status::Passed,
            _ if tail.starts_with("FAILED") => Status::Failed,
            _ => continue,
        };
        out.push(TestRecord::new(name.trim(), status));
    }
    attach_libtest_failure_bodies(&mut out, stdout);
    out
}

fn attach_libtest_failure_bodies(records: &mut [TestRecord], stdout: &str) {
    let Some((_, body)) = stdout.split_once("\nfailures:\n\n") else {
        return;
    };
    for block in body.split("\n\n") {
        let Some(first_line) = block.lines().next() else {
            continue;
        };
        let header = first_line.trim();
        if !header.starts_with("---- ") || !header.ends_with(" stdout ----") {
            continue;
        }
        let name = header
            .trim_start_matches("---- ")
            .trim_end_matches(" stdout ----")
            .trim();
        let message = block.lines().skip(1).collect::<Vec<_>>().join("\n");
        if let Some(record) = records.iter_mut().find(|record| record.name == name) {
            if !message.is_empty() {
                record.message = Some(message);
            }
        }
    }
}

/// Parse plain `go test` diagnostic case lines.
pub(crate) fn parse_go_text(stdout: &str, stderr: &str) -> Vec<TestRecord> {
    let mut out = Vec::new();
    for line in format!("{stdout}\n{stderr}").lines() {
        let line = line.trim_start();
        let parsed = if let Some(rest) = line.strip_prefix("--- PASS: ") {
            parse_go_event(rest, Status::Passed)
        } else if let Some(rest) = line.strip_prefix("--- FAIL: ") {
            parse_go_event(rest, Status::Failed)
        } else if let Some(rest) = line.strip_prefix("--- SKIP: ") {
            parse_go_event(rest, Status::Skipped)
        } else {
            None
        };
        if let Some(record) = parsed {
            out.push(record);
        }
    }
    out
}

fn parse_go_event(rest: &str, status: Status) -> Option<TestRecord> {
    let (name, duration_part) = rest.split_once(" (").unwrap_or((rest, ""));
    let mut record = TestRecord::new(name.trim(), status);
    if let Some(seconds) = duration_part.trim_end_matches(')').strip_suffix('s') {
        if let Ok(value) = seconds.parse::<f64>() {
            record.duration_ms = duration_seconds_to_ms(value);
        }
    }
    Some(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cargo_libtest_pass_and_fail() {
        let output = "running 3 tests\ntest mod_a::passes ... ok\ntest mod_a::fails ... FAILED\ntest mod_b::skipped ... ignored\n\nfailures:\n\n---- mod_a::fails stdout ----\nassertion `left == right` failed\n  left: 1\n  right: 2\n\nfailures:\n    mod_a::fails\n\ntest result: FAILED. 1 passed; 1 failed; 1 ignored\n";
        let records = parse_cargo_libtest(output);
        assert_eq!(records.len(), 3);
        let failure = records
            .iter()
            .find(|record| record.name == "mod_a::fails")
            .unwrap();
        assert_eq!(failure.status, Status::Failed);
        assert!(failure.message.as_deref().unwrap().contains("assertion"));
    }

    #[test]
    fn parses_go_text_blocks() {
        let records = parse_go_text("=== RUN TestA\n--- PASS: TestA (0.01s)\n=== RUN TestB\n--- FAIL: TestB (0.02s)\nPASS\n", "");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].status, Status::Passed);
        assert_eq!(records[0].duration_ms, 10);
        assert_eq!(records[1].status, Status::Failed);
        assert_eq!(records[1].duration_ms, 20);
    }
}
