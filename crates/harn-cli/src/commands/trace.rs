use std::fs;
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::cli::{TraceArgs, TraceCommand, TraceImportArgs};

pub(crate) async fn handle(args: TraceArgs) -> Result<(), String> {
    match args.command {
        TraceCommand::Import(import) => run_import(import),
    }
}

fn run_import(args: TraceImportArgs) -> Result<(), String> {
    let content = fs::read_to_string(&args.trace_file)
        .map_err(|error| format!("failed to read {}: {error}", args.trace_file))?;
    let lines = convert_trace_jsonl(&content, args.trace_id.as_deref())?;
    let body = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    if let Some(parent) = Path::new(&args.output).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
        }
    }
    fs::write(&args.output, body)
        .map_err(|error| format!("failed to write {}: {error}", args.output))?;
    println!("{}", args.output);
    Ok(())
}

fn convert_trace_jsonl(content: &str, trace_id: Option<&str>) -> Result<Vec<String>, String> {
    let mut fixtures = Vec::new();
    for (idx, raw_line) in content.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .map_err(|error| format!("invalid JSON in trace line {line_no}: {error}"))?;
        if let Some(expected_trace_id) = trace_id {
            let actual_trace_id = value.get("trace_id").and_then(Value::as_str);
            if actual_trace_id != Some(expected_trace_id) {
                continue;
            }
        }
        fixtures.push(trace_record_to_fixture(&value, line_no)?);
    }
    if trace_id.is_some() && fixtures.is_empty() {
        return Err("trace filter matched no records".to_string());
    }
    Ok(fixtures)
}

fn trace_record_to_fixture(value: &Value, line_no: usize) -> Result<String, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("trace line {line_no} must be a JSON object"))?;
    if object.get("prompt").is_none() {
        return Err(format!("trace line {line_no} is missing `prompt`"));
    }

    let response = object.get("response").unwrap_or(&Value::Null);
    let top_level_tool_calls = parse_tool_calls(object.get("tool_calls"))?;
    let response_tool_calls = parse_tool_calls(response.get("tool_calls"))?;
    let tool_calls = if !top_level_tool_calls.is_empty() {
        top_level_tool_calls
    } else {
        response_tool_calls
    };

    let text = match response {
        Value::String(text) => text.clone(),
        Value::Object(map) => optional_string(map, "text")
            .or_else(|| optional_string(map, "content"))
            .unwrap_or_default(),
        Value::Null => String::new(),
        _ => {
            return Err(format!(
                "trace line {line_no} has unsupported `response`; expected string or object"
            ))
        }
    };

    let mut fixture = Map::new();
    if !text.is_empty() {
        fixture.insert("text".to_string(), Value::String(text));
    }
    if !tool_calls.is_empty() {
        fixture.insert("tool_calls".to_string(), Value::Array(tool_calls));
    }
    fixture.insert(
        "model".to_string(),
        Value::String(
            object
                .get("model")
                .and_then(Value::as_str)
                .or_else(|| response.get("model").and_then(Value::as_str))
                .unwrap_or("imported-trace")
                .to_string(),
        ),
    );
    if let Some(provider) = object
        .get("provider")
        .and_then(Value::as_str)
        .or_else(|| response.get("provider").and_then(Value::as_str))
    {
        fixture.insert("provider".to_string(), Value::String(provider.to_string()));
    }
    if let Some(input_tokens) = object
        .get("input_tokens")
        .and_then(Value::as_i64)
        .or_else(|| response.get("input_tokens").and_then(Value::as_i64))
    {
        fixture.insert("input_tokens".to_string(), json!(input_tokens));
    }
    if let Some(output_tokens) = object
        .get("output_tokens")
        .and_then(Value::as_i64)
        .or_else(|| response.get("output_tokens").and_then(Value::as_i64))
    {
        fixture.insert("output_tokens".to_string(), json!(output_tokens));
    }
    serde_json::to_string(&Value::Object(fixture)).map_err(|error| {
        format!("failed to serialize imported fixture for line {line_no}: {error}")
    })
}

fn parse_tool_calls(value: Option<&Value>) -> Result<Vec<Value>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err("tool_calls must be an array".to_string());
    };
    items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let object = item
                .as_object()
                .ok_or_else(|| format!("tool_calls[{idx}] must be an object"))?;
            let name = optional_string(object, "name")
                .ok_or_else(|| format!("tool_calls[{idx}] is missing `name`"))?;
            Ok(json!({
                "name": name,
                "args": object
                    .get("arguments")
                    .cloned()
                    .or_else(|| object.get("args").cloned())
                    .unwrap_or_else(|| json!({})),
            }))
        })
        .collect()
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::convert_trace_jsonl;

    #[test]
    fn converts_generic_trace_jsonl_to_cli_fixture() {
        let trace = concat!(
            "{\"trace_id\":\"trace-1\",\"prompt\":\"Question\",\"response\":{\"text\":\"Answer\",\"model\":\"gpt-test\"},\"tool_calls\":[{\"name\":\"read_file\",\"arguments\":{\"path\":\"README.md\"}}]}\n",
            "{\"trace_id\":\"trace-2\",\"prompt\":\"Ignored\",\"response\":\"Nope\"}\n"
        );

        let fixtures = convert_trace_jsonl(trace, Some("trace-1")).unwrap();

        assert_eq!(fixtures.len(), 1);
        assert!(fixtures[0].contains("\"text\":\"Answer\""));
        assert!(fixtures[0].contains("\"model\":\"gpt-test\""));
        assert!(fixtures[0].contains("\"name\":\"read_file\""));
    }

    #[test]
    fn rejects_empty_trace_filter_result() {
        let error = convert_trace_jsonl(
            "{\"trace_id\":\"trace-1\",\"prompt\":\"Question\",\"response\":\"Answer\"}\n",
            Some("trace-missing"),
        )
        .unwrap_err();

        assert!(error.contains("matched no records"));
    }
}
