use std::collections::{BTreeMap, BTreeSet};

use harn_vm::orchestration::{eval_pack_case_fingerprint, EvalPackCase, EvalPackCommandSpec};
use serde_json::Value as JsonValue;

pub(in super::super) fn coding_agent_live_verify_cases(
    python: &str,
) -> Result<Vec<EvalPackCase>, String> {
    let test_cmd = format!("{python} -m unittest discover -s tests");
    let mut cases = vec![
        coding_agent_live_verify_case(
            python,
            "python-add",
            "Python add repair",
            "multi-tool",
            "One-file Python bug fix verified by unittest output.",
            format!(
                "Fix the repository so the test suite passes. Inspect files before editing, make the smallest correct code change, then run `{test_cmd}`."
            ),
        ),
        coding_agent_live_verify_case(
            python,
            "cli-help-flag",
            "CLI help flag",
            "multi-tool",
            "Add a tiny CLI flag, update help-facing docs, and verify behavior.",
            "Add a `--shout` flag to the greeting CLI. The flag should print the greeting in uppercase, appear in `--help`, and be documented in README.md. Verify it with the Python CLI."
                .to_string(),
        ),
        coding_agent_live_verify_case(
            python,
            "test-output-first",
            "Test-output-first repair",
            "multi-tool",
            "Run a failing test first, then edit the implementation and re-run it.",
            format!(
                "Run the unittest suite first and use the failing output to choose the fix. Then make the smallest implementation change and re-run `{test_cmd}`."
            ),
        ),
        coding_agent_live_verify_case(
            python,
            "docs-symbol-rename",
            "Docs symbol rename",
            "multi-tool",
            "Update docs and an example after a symbol rename without touching implementation.",
            "The public helper was renamed to `format_greeting`. Update the docs and example to use the renamed symbol. Do not edit `greeter.py`; verify the example runs."
                .to_string(),
        ),
        coding_agent_live_verify_case(
            python,
            "read-only-audit",
            "Read-only audit",
            "one-tool",
            "Inspect a file and report that no edits are needed.",
            "Read README.md. If README.md says the default timeout is 30 seconds, do not edit files and reply exactly AUDIT_OK."
                .to_string(),
        ),
        coding_agent_live_verify_case(
            python,
            "no-tool-diagnosis",
            "No-tool diagnosis",
            "no-tool",
            "Answer from prompt-only context without any tools.",
            "No tools are available. Given this snippet: `def add(a, b): return a - b`, and this failing expectation: `add(2, 3) == 5`, state the smallest code change. Include the exact token PATCH_HINT."
                .to_string(),
        ),
    ];
    for case in &mut cases {
        case.case_fingerprint =
            eval_pack_case_fingerprint(case).map_err(|error| error.to_string())?;
    }
    Ok(cases)
}

fn coding_agent_live_verify_case(
    python: &str,
    id: &str,
    name: &str,
    tool_sequence: &str,
    description: &str,
    task: String,
) -> EvalPackCase {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "group".to_string(),
        JsonValue::String("coding-agent".to_string()),
    );
    metadata.insert(
        "tool_sequence".to_string(),
        JsonValue::String(tool_sequence.to_string()),
    );
    EvalPackCase {
        id: Some(id.to_string()),
        name: Some(name.to_string()),
        description: Some(description.to_string()),
        kind: Some("live-verify".to_string()),
        task: Some(task),
        workspace: Some(".".to_string()),
        verify_command: Some(coding_agent_summary_verify_command(python)),
        expected_output_paths: vec![
            "summary.json".to_string(),
            "result.json".to_string(),
            "transcript_events.jsonl".to_string(),
        ],
        required_output_snippets: vec![format!("\"fixture_id\": \"{id}\"")],
        metadata,
        ..EvalPackCase::default()
    }
}

fn coding_agent_summary_verify_command(python: &str) -> EvalPackCommandSpec {
    EvalPackCommandSpec::Argv(vec![
        python.to_string(),
        "-c".to_string(),
        "import json, pathlib, sys; p = pathlib.Path('summary.json'); sys.exit(0 if p.exists() and json.loads(p.read_text(encoding='utf-8')).get('passed') is True else 1)"
            .to_string(),
    ])
}

pub(in super::super) fn fixture_id(fixture: &EvalPackCase) -> &str {
    fixture.id.as_deref().unwrap_or("<unnamed>")
}

pub(in super::super) fn fixture_name(fixture: &EvalPackCase) -> String {
    fixture
        .name
        .clone()
        .or_else(|| fixture.id.clone())
        .unwrap_or_else(|| "<unnamed>".to_string())
}

pub(in super::super) fn fixture_description(fixture: &EvalPackCase) -> String {
    fixture.description.clone().unwrap_or_default()
}

pub(in super::super) fn fixture_tool_sequence(fixture: &EvalPackCase) -> String {
    fixture
        .metadata
        .get("tool_sequence")
        .and_then(JsonValue::as_str)
        .unwrap_or("unspecified")
        .to_string()
}

pub(in super::super) fn resolve_fixtures(
    raw_fixtures: &[String],
    python: &str,
) -> Result<Vec<EvalPackCase>, String> {
    let definitions = coding_agent_live_verify_cases(python)?;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for raw in raw_fixtures {
        let fixture = raw.trim().to_ascii_lowercase();
        if fixture.is_empty() {
            continue;
        }
        if fixture == "all" {
            return Ok(definitions);
        }
        let Some(definition) = fixture_definition(&definitions, &fixture) else {
            return Err(format!(
                "unsupported --fixture `{fixture}`; expected one of: all, {}",
                definitions
                    .iter()
                    .map(fixture_id)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };
        if seen.insert(fixture_id(&definition).to_string()) {
            out.push(definition);
        }
    }
    if out.is_empty() {
        return Err("at least one coding-agent fixture must be selected".to_string());
    }
    Ok(out)
}

fn fixture_definition(definitions: &[EvalPackCase], id: &str) -> Option<EvalPackCase> {
    definitions
        .iter()
        .find(|definition| fixture_id(definition) == id)
        .cloned()
}
