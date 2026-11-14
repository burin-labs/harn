use serde_json::{json, Value};

pub fn completed_verifier_result() -> Value {
    json!({
        "byte_count": 14,
        "cwd": "$WORKSPACE/repo",
        "denial": null,
        "exit_code": 0,
        "line_count": 1,
        "output_sha256": "sha256:edd98eba7f0634541ba7a5062faec83ab42e51572fdb534054df86a64d55bf15",
        "signal": null,
        "status": "completed",
        "stderr": "",
        "stderr_truncated": false,
        "stdout": "1:hello world\n",
        "stdout_truncated": false,
        "timed_out": false
    })
}

#[cfg(unix)]
pub fn verifier_golden_results() -> [Value; 2] {
    [json!(0), Value::Null].map(|survivor_count| {
        let mut result = completed_verifier_result();
        result["process_cleanup"] = json!({
            "observed_child_count": 0,
            "reaped_child_count": 0,
            "survivor_count": survivor_count,
            "omitted_child_count": 0,
            "observed_children": [],
            "reaped_children": [],
            "surviving_children": []
        });
        result
    })
}

#[cfg(unix)]
pub fn select_verifier_golden<'a>(
    observed: Option<&str>,
    allowed: &'a [String; 2],
) -> Option<&'a str> {
    allowed
        .iter()
        .map(String::as_str)
        .find(|known| Some(*known) == observed)
}
