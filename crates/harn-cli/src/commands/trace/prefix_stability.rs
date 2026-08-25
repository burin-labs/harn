use crate::cli::TracePrefixStabilityArgs;
use serde::Serialize;
use serde_json::Value;
use std::path::{Component, Path};

const REQUEST_SCHEMA: &str = "harn.llm.raw_provider_request.v1";
const REPORT_SCHEMA: &str = "harn.trace.prefix_stability.v1";

#[derive(Debug)]
struct CapturedRequest {
    path: String,
    iteration: u64,
    body: Value,
}

#[derive(Debug, Serialize)]
struct PrefixStabilityReport {
    schema_version: &'static str,
    stable: bool,
    request_count: usize,
    pairs: Vec<PairReport>,
}

#[derive(Debug, Serialize)]
struct PairReport {
    previous_iteration: u64,
    next_iteration: u64,
    previous_request: String,
    next_request: String,
    leading_identical_messages: usize,
    append_only: bool,
    system_stable: bool,
    tools_stable: bool,
    modified_message: Option<MessageChange>,
    system_change: Option<ValueChange>,
    tool_change: Option<IndexedValueChange>,
}

#[derive(Debug, Serialize)]
struct MessageChange {
    index: usize,
    role: Option<String>,
    first_differing_byte: usize,
    before: Option<Value>,
    after: Option<Value>,
}

#[derive(Debug, Serialize)]
struct ValueChange {
    first_differing_byte: usize,
    before: Option<Value>,
    after: Option<Value>,
}

#[derive(Debug, Serialize)]
struct IndexedValueChange {
    index: Option<usize>,
    first_differing_byte: usize,
    before: Option<Value>,
    after: Option<Value>,
}

pub(super) fn run(args: &TracePrefixStabilityArgs) -> Result<(), String> {
    let requests = load_requests(&args.transcript_dir)?;
    let report = analyze(&requests)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("could not encode prefix stability report: {error}"))?
        );
    } else {
        print_human_report(&report);
    }
    if report.stable {
        Ok(())
    } else {
        Err("captured request prefix is unstable".to_string())
    }
}

fn load_requests(transcript_dir: &Path) -> Result<Vec<CapturedRequest>, String> {
    let transcript_path = transcript_dir.join("llm_transcript.jsonl");
    let transcript = std::fs::read_to_string(&transcript_path)
        .map_err(|error| format!("could not read {}: {error}", transcript_path.display()))?;
    let mut requests = Vec::new();
    for (line_index, line) in transcript.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event: Value = serde_json::from_str(line).map_err(|error| {
            format!(
                "{}:{} is not valid JSON: {error}",
                transcript_path.display(),
                line_index + 1
            )
        })?;
        if event.get("type").and_then(Value::as_str) != Some("provider_raw_capture")
            || event.get("capture").and_then(Value::as_str) != Some("request")
        {
            continue;
        }
        let relative = event.get("path").and_then(Value::as_str).ok_or_else(|| {
            format!(
                "{}:{} request capture has no path",
                transcript_path.display(),
                line_index + 1
            )
        })?;
        validate_relative_path(relative)?;
        requests.push(load_request(transcript_dir, relative)?);
    }
    if requests.len() < 2 {
        return Err(format!(
            "{} has {} raw request capture(s); at least 2 are required",
            transcript_path.display(),
            requests.len()
        ));
    }
    Ok(requests)
}

fn validate_relative_path(path: &str) -> Result<(), String> {
    let safe = !path.is_empty()
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)));
    if safe {
        Ok(())
    } else {
        Err(format!(
            "raw request path must stay within the transcript directory: {path}"
        ))
    }
}

fn load_request(transcript_dir: &Path, relative: &str) -> Result<CapturedRequest, String> {
    let path = transcript_dir.join(relative);
    let encoded = std::fs::read_to_string(&path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let envelope: Value = serde_json::from_str(&encoded)
        .map_err(|error| format!("{} is not valid JSON: {error}", path.display()))?;
    if envelope.get("schema_version").and_then(Value::as_str) != Some(REQUEST_SCHEMA) {
        return Err(format!(
            "{} is not a {REQUEST_SCHEMA} capture",
            path.display()
        ));
    }
    if envelope.get("kind").and_then(Value::as_str) != Some("request") {
        return Err(format!("{} is not a request capture", path.display()));
    }
    let iteration = envelope
        .get("iteration")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{} has no integer iteration", path.display()))?;
    let body = envelope
        .get("body")
        .filter(|body| body.is_object())
        .cloned()
        .ok_or_else(|| format!("{} has no request body object", path.display()))?;
    Ok(CapturedRequest {
        path: relative.to_string(),
        iteration,
        body,
    })
}

fn analyze(requests: &[CapturedRequest]) -> Result<PrefixStabilityReport, String> {
    let mut pairs = Vec::with_capacity(requests.len().saturating_sub(1));
    for window in requests.windows(2) {
        pairs.push(analyze_pair(&window[0], &window[1])?);
    }
    let stable = pairs
        .iter()
        .all(|pair| pair.append_only && pair.system_stable && pair.tools_stable);
    Ok(PrefixStabilityReport {
        schema_version: REPORT_SCHEMA,
        stable,
        request_count: requests.len(),
        pairs,
    })
}

fn analyze_pair(previous: &CapturedRequest, next: &CapturedRequest) -> Result<PairReport, String> {
    let previous_messages = messages(&previous.body, &previous.path)?;
    let next_messages = messages(&next.body, &next.path)?;
    let leading_identical_messages = previous_messages
        .iter()
        .zip(next_messages)
        .take_while(|(left, right)| left == right)
        .count();
    let append_only = previous_messages.len() <= next_messages.len()
        && leading_identical_messages == previous_messages.len();
    let modified_message = (!append_only).then(|| {
        let index = leading_identical_messages;
        let before = previous_messages.get(index).cloned();
        let after = next_messages.get(index).cloned();
        MessageChange {
            index,
            role: before.as_ref().or(after.as_ref()).and_then(message_role),
            first_differing_byte: first_differing_byte(before.as_ref(), after.as_ref()),
            before,
            after,
        }
    });
    let previous_system = system_block(&previous.body, previous_messages);
    let next_system = system_block(&next.body, next_messages);
    let system_stable = previous_system == next_system;
    let system_change = (!system_stable).then(|| ValueChange {
        first_differing_byte: first_differing_byte(previous_system.as_ref(), next_system.as_ref()),
        before: previous_system,
        after: next_system,
    });
    let previous_tools = tools(&previous.body, &previous.path)?;
    let next_tools = tools(&next.body, &next.path)?;
    let tools_stable = previous_tools == next_tools;
    let tool_change = (!tools_stable).then(|| {
        let index = match (&previous_tools, &next_tools) {
            (Some(left), Some(right)) => left
                .iter()
                .zip(right.iter())
                .position(|(before, after)| before != after)
                .or_else(|| (left.len() != right.len()).then_some(left.len().min(right.len()))),
            _ => None,
        };
        let before = match index {
            Some(index) => previous_tools.and_then(|tools| tools.get(index).cloned()),
            None => previous.body.get("tools").cloned(),
        };
        let after = match index {
            Some(index) => next_tools.and_then(|tools| tools.get(index).cloned()),
            None => next.body.get("tools").cloned(),
        };
        IndexedValueChange {
            index,
            first_differing_byte: first_differing_byte(before.as_ref(), after.as_ref()),
            before,
            after,
        }
    });
    Ok(PairReport {
        previous_iteration: previous.iteration,
        next_iteration: next.iteration,
        previous_request: previous.path.clone(),
        next_request: next.path.clone(),
        leading_identical_messages,
        append_only,
        system_stable,
        tools_stable,
        modified_message,
        system_change,
        tool_change,
    })
}

fn messages<'a>(body: &'a Value, path: &str) -> Result<&'a [Value], String> {
    for key in ["messages", "contents", "input"] {
        if let Some(value) = body.get(key) {
            return value
                .as_array()
                .map(Vec::as_slice)
                .ok_or_else(|| format!("{path} request body `{key}` is not an array"));
        }
    }
    Err(format!(
        "{path} request body has no messages, contents, or input array"
    ))
}

fn system_block(body: &Value, messages: &[Value]) -> Option<Value> {
    for key in [
        "system",
        "systemInstruction",
        "system_instruction",
        "instructions",
    ] {
        if let Some(value) = body.get(key) {
            return Some(value.clone());
        }
    }
    let leading: Vec<Value> = messages
        .iter()
        .take_while(|message| {
            matches!(
                message.get("role").and_then(Value::as_str),
                Some("system" | "developer")
            )
        })
        .cloned()
        .collect();
    (!leading.is_empty()).then_some(Value::Array(leading))
}

fn tools<'a>(body: &'a Value, path: &str) -> Result<Option<&'a [Value]>, String> {
    body.get("tools")
        .map(|value| {
            value
                .as_array()
                .map(Vec::as_slice)
                .ok_or_else(|| format!("{path} request body `tools` is not an array"))
        })
        .transpose()
}

fn message_role(message: &Value) -> Option<String> {
    message
        .get("role")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn first_differing_byte(before: Option<&Value>, after: Option<&Value>) -> usize {
    let before = before
        .and_then(|value| serde_json::to_vec(value).ok())
        .unwrap_or_default();
    let after = after
        .and_then(|value| serde_json::to_vec(value).ok())
        .unwrap_or_default();
    before
        .iter()
        .zip(&after)
        .position(|(left, right)| left != right)
        .unwrap_or_else(|| before.len().min(after.len()))
}

fn print_human_report(report: &PrefixStabilityReport) {
    println!("pair\tleading messages\tsystem\ttools\tappend-only");
    for pair in &report.pairs {
        println!(
            "{} -> {}\t{}\t{}\t{}\t{}",
            pair.previous_iteration,
            pair.next_iteration,
            pair.leading_identical_messages,
            state(pair.system_stable),
            state(pair.tools_stable),
            state(pair.append_only)
        );
        if let Some(change) = &pair.modified_message {
            let role = change.role.as_deref().unwrap_or("unknown role");
            println!(
                "  message {} ({role}) changed at byte {}:",
                change.index, change.first_differing_byte
            );
            print_value('-', change.before.as_ref());
            print_value('+', change.after.as_ref());
        }
        if let Some(change) = &pair.system_change {
            println!(
                "  system block changed at byte {}:",
                change.first_differing_byte
            );
            print_value('-', change.before.as_ref());
            print_value('+', change.after.as_ref());
        }
        if let Some(change) = &pair.tool_change {
            match change.index {
                Some(index) => println!(
                    "  tool {index} changed at byte {}:",
                    change.first_differing_byte
                ),
                None => println!(
                    "  tool list changed at byte {}:",
                    change.first_differing_byte
                ),
            }
            print_value('-', change.before.as_ref());
            print_value('+', change.after.as_ref());
        }
    }
    let status = if report.stable { "stable" } else { "unstable" };
    println!(
        "prefix {status}: {} requests, {} consecutive pairs",
        report.request_count,
        report.pairs.len()
    );
}

fn state(stable: bool) -> &'static str {
    if stable {
        "stable"
    } else {
        "changed"
    }
}

fn print_value(marker: char, value: Option<&Value>) {
    let encoded = value
        .and_then(|value| serde_json::to_string_pretty(value).ok())
        .unwrap_or_else(|| "<missing>".to_string());
    for line in encoded.lines() {
        println!("  {marker} {line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(iteration: u64, body: Value) -> CapturedRequest {
        CapturedRequest {
            path: format!("raw-provider/request-{iteration}.json"),
            iteration,
            body,
        }
    }

    #[test]
    fn append_only_growth_keeps_the_prefix_stable() {
        let system = serde_json::json!({"role": "system", "content": "help"});
        let user = serde_json::json!({"role": "user", "content": "start"});
        let tools = serde_json::json!([{"type": "function", "name": "read"}]);
        let first = request(
            0,
            serde_json::json!({"messages": [system, user], "tools": tools}),
        );
        let second = request(
            1,
            serde_json::json!({
                "messages": [system, user, {"role": "assistant", "content": "done"}],
                "tools": tools
            }),
        );

        let report = analyze(&[first, second]).expect("analyze requests");

        assert!(report.stable);
        assert_eq!(report.pairs[0].leading_identical_messages, 2);
        assert!(report.pairs[0].modified_message.is_none());
    }

    #[test]
    fn one_changed_prefix_message_reports_its_index_role_and_values() {
        let first = request(
            0,
            serde_json::json!({
                "messages": [{"role": "system", "content": "time: 10:00"}],
                "tools": []
            }),
        );
        let second = request(
            1,
            serde_json::json!({
                "messages": [{"role": "system", "content": "time: 10:01"}],
                "tools": []
            }),
        );

        let report = analyze(&[first, second]).expect("analyze requests");
        let change = report.pairs[0]
            .modified_message
            .as_ref()
            .expect("changed message");

        assert!(!report.stable);
        assert_eq!(change.index, 0);
        assert_eq!(change.role.as_deref(), Some("system"));
        assert_eq!(change.before.as_ref().unwrap()["content"], "time: 10:00");
        assert_eq!(change.after.as_ref().unwrap()["content"], "time: 10:01");
    }

    #[test]
    fn reordered_tools_fail_even_when_messages_are_append_only() {
        let messages = serde_json::json!([{"role": "user", "content": "start"}]);
        let first = request(
            0,
            serde_json::json!({"messages": messages, "tools": [{"name": "read"}, {"name": "write"}]}),
        );
        let second = request(
            1,
            serde_json::json!({"messages": messages, "tools": [{"name": "write"}, {"name": "read"}]}),
        );

        let report = analyze(&[first, second]).expect("analyze requests");

        assert!(!report.stable);
        assert!(report.pairs[0].append_only);
        assert!(!report.pairs[0].tools_stable);
        let change = report.pairs[0].tool_change.as_ref().expect("changed tool");
        assert_eq!(change.index, Some(0));
        assert_eq!(change.before.as_ref().unwrap()["name"], "read");
        assert_eq!(change.after.as_ref().unwrap()["name"], "write");
    }
}
