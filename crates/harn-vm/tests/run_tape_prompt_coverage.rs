//! Prompt provenance on a recorded agent run: a host reviewing a run tape can
//! map the system prompt of every model call back to the `.harn.prompt` lines
//! and filled-in values that produced it.
// The workspace fixture uses unix paths.
#![cfg(unix)]

use std::path::Path;
use std::sync::Mutex;

use harn_vm::run_tape::{start_recording, RecordingLevel, TAPE_FILE_NAME};
use harn_vm::testbench::tape::{CallAttribution, CallFacts, EventTape, TapeRecordKind};
use harn_vm::value::VmError;

/// Recording is process-global.
static SERIAL: Mutex<()> = Mutex::new(());

const HOST_PROMPT: &str = "\
You are the coding agent for {{ project }}, working for {{ user }}.

## How to work

- Read the relevant files before you change them.
- Run the project's tests after every change and report what they printed.
- Keep each change small enough to review in one sitting.
{{ if strict }}
- Never commit, push, or open a pull request yourself.
{{ end }}
Today is {{ today }}.
";

fn quote(value: &Path) -> String {
    serde_json::to_string(&value.display().to_string()).unwrap()
}

/// A host's agent turn: its system prompt from a prompt asset, the detected
/// project profile, two annotated native tools and a scripted model.
fn sample_run(prompt: &Path, workspace: &Path) -> String {
    format!(
        r#"
import {{ agent_loop }} from "std/agent/loop"
import {{ llm_text, with_llm_script }} from "std/testing"

pipeline main(harness: Harness, task: unknown) {{
  const system = harness.fs.render_prompt(
    {prompt},
    {{project: "lantern", user: "Ada", strict: true, today: "2026-10-29"}},
  )
  const profile = harness.project.context_profile({workspace}, {{include_env_credentials: false}})
  let tools = tool_define(
    tool_registry(),
    "read_file",
    "Read a UTF-8 file from the workspace.",
    {{
      parameters: {{path: {{type: "string"}}}},
      annotations: {{title: "Read a file", kind: "read", readOnlyHint: true}},
      handler: {{ args -> return "fn main() {{}}\n" }},
    }},
  )
  tools = tool_define(
    tools,
    "run_tests",
    "Run the project's test suite.",
    {{
      parameters: {{}},
      annotations: {{title: "Run the tests", kind: "execute"}},
      handler: {{ _args -> return "test result: ok. 3 passed" }},
    }},
  )
  const result = with_llm_script(
    harness.llm,
    [
      {{tool_calls: [{{id: "read-1", name: "read_file", arguments: {{path: "src/main.rs"}}}}]}},
      {{tool_calls: [{{id: "test-1", name: "run_tests", arguments: {{}}}}]}},
      llm_text("<user_response>All 3 tests pass.</user_response><done>##DONE##</done>"),
    ],
    {{ ->
      return agent_loop(
        harness,
        "Check that the tests pass.",
        system,
        {{
          provider: "mock",
          tools: tools,
          tool_format: "native",
          max_iterations: 4,
          loop_until_done: true,
          context_profile: profile,
          effort: "medium",
          verify_completion: {{ _payload -> return true }},
          session_id: "prompt-coverage",
        }},
      )
    }},
  )
  harness.stdio.log(result.status)
}}
"#,
        prompt = quote(prompt),
        workspace = quote(workspace),
    )
}

fn run(source: &str, root: &Path) -> Result<String, String> {
    let source = source.to_string();
    let root = root.to_path_buf();
    harn_vm::on_vm_stack(move || {
        harn_vm::reset_thread_local_state();
        let chunk = harn_vm::compile_source(&source)?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        rt.block_on(harn_clock::test_support::with_local_tasks(async move {
            harn_vm::stdlib::process::set_thread_execution_context(Some(
                harn_vm::orchestration::RunExecutionRecord {
                    cwd: Some(root.display().to_string()),
                    project_root: Some(root.display().to_string()),
                    ..Default::default()
                },
            ));
            let mut vm = harn_vm::new_vm();
            harn_runtime::register_vm_stdlib(&mut vm);
            harn_vm::register_store_builtins(&mut vm, &root);
            let result = vm
                .execute(&chunk)
                .await
                .map_err(|error: VmError| format!("{error:?}"));
            harn_vm::stdlib::process::set_thread_execution_context(None);
            result?;
            Ok(vm.output().to_string())
        }))
    })
}

/// Record the sample run and return its tape.
fn record_sample() -> EventTape {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let tapes = root.path().join("tapes");
    std::fs::create_dir_all(workspace.join(".git")).unwrap();
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    std::fs::create_dir_all(&tapes).unwrap();
    std::fs::write(
        workspace.join("Cargo.toml"),
        "[package]\nname = \"lantern\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let prompt = root.path().join("host.harn.prompt");
    std::fs::write(&prompt, HOST_PROMPT).unwrap();
    // SAFETY: tests here hold SERIAL, so no other thread reads the env.
    unsafe { std::env::set_var("HARN_SESSION_STORE_ROOT", root.path().join("state")) };

    let guard = start_recording(&tapes, RecordingLevel::Full, None)
        .unwrap()
        .expect("a full tape records");
    let output = run(&sample_run(&prompt, &workspace), root.path()).expect("the run succeeds");
    assert!(output.contains("[harn] done"), "run output: {output}");
    assert_eq!(guard.error(), None, "the tape wrote cleanly");
    drop(guard);
    EventTape::load(&tapes.join(TAPE_FILE_NAME)).unwrap()
}

fn attributions(tape: &EventTape) -> Vec<CallAttribution> {
    tape.records
        .iter()
        .filter_map(|record| match &record.kind {
            TapeRecordKind::LlmCall { attribution, .. } => attribution.clone(),
            _ => None,
        })
        .collect()
}

/// The system prompt text no attributed segment covers, per call.
fn unmapped_system_text(tape: &EventTape) -> Vec<String> {
    tape.records
        .iter()
        .filter_map(|record| match &record.kind {
            TapeRecordKind::LlmCall {
                request: Some(request),
                attribution: Some(attribution),
                ..
            } => Some((request, attribution)),
            _ => None,
        })
        .map(|(request, attribution)| {
            let request: serde_json::Value =
                serde_json::from_slice(&tape.resolve_payload(request).unwrap()).unwrap();
            let system = request["system"].as_str().unwrap_or_default().to_string();
            let mut covered = vec![false; system.len()];
            for segment in attribution.segments.iter().filter(|s| s.field == "system") {
                covered[segment.start as usize..segment.end as usize].fill(true);
            }
            let mut out = String::new();
            for (i, ch) in system.char_indices() {
                if !covered[i] {
                    out.push(ch);
                } else if !out.ends_with('|') {
                    out.push('|');
                }
            }
            out
        })
        .collect()
}

fn percent(part: u64, whole: u64) -> f64 {
    100.0 * part as f64 / whole as f64
}

/// The sample run's system prompt is all template text or filled-in values,
/// but for the blank lines the assembler joins fragments with.
#[test]
fn a_recorded_agent_run_maps_its_system_prompt_to_prompt_sources() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tape = record_sample();
    let calls = attributions(&tape);
    assert_eq!(calls.len(), 3, "one attribution per model call");
    let bytes: u64 = calls.iter().map(|call| call.system_bytes).sum();
    let template: u64 = calls.iter().map(|call| call.system_template_bytes).sum();
    let value: u64 = calls.iter().map(|call| call.system_value_bytes).sum();
    let unmapped = unmapped_system_text(&tape);
    eprintln!(
        "system prompt: {bytes} bytes, template text {template} ({:.1}%), filled values {value} \
         ({:.1}%), unmapped {:.1}%",
        percent(template, bytes),
        percent(value, bytes),
        percent(bytes - template - value, bytes),
    );
    assert!(
        percent(template, bytes) > 95.0,
        "template text {:.1}% of the system prompt; unmapped: {unmapped:#?}",
        percent(template, bytes)
    );
    assert!(
        value > 0,
        "the host prompt's filled-in values count as values"
    );
    for text in &unmapped {
        assert!(
            text.chars().all(|ch| ch == '\n' || ch == '|'),
            "only the assembler's blank-line joins stay unmapped: {text:?}"
        );
    }
}

/// Plain text in a rendered template points at its real line, column and
/// template revision, not line 0.
#[test]
fn a_recorded_render_locates_plain_text_in_its_template() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tape = record_sample();
    let (rendered, spans) = tape
        .records
        .iter()
        .find_map(|record| match &record.kind {
            TapeRecordKind::PromptRender {
                template_uri,
                rendered,
                spans,
                ..
            } if template_uri.ends_with("host.harn.prompt") => Some((
                String::from_utf8(tape.resolve_payload(rendered).unwrap()).unwrap(),
                serde_json::from_slice::<Vec<harn_vm::PromptSourceSpan>>(
                    &tape.resolve_payload(spans).unwrap(),
                )
                .unwrap(),
            )),
            _ => None,
        })
        .expect("the host prompt render is on the tape");
    let texts: Vec<_> = spans
        .iter()
        .filter(|span| span.kind == harn_vm::PromptSpanKind::Text)
        .collect();
    assert!(texts.len() > 3, "{spans:#?}");
    for span in &texts {
        let text = rendered
            .get(span.output_start..span.output_end)
            .expect("a span is a char range of its render");
        let at = source_offset(HOST_PROMPT, span.template_line, span.template_col);
        assert!(
            HOST_PROMPT
                .get(at..)
                .is_some_and(|rest| rest.starts_with(text)),
            "{text:?} is not at line {} column {} of its template",
            span.template_line,
            span.template_col
        );
    }
    assert!(
        spans
            .iter()
            .all(|span| span.template_revision == blake3_hex(HOST_PROMPT)),
        "every span names the template revision it was rendered from: {spans:#?}"
    );
}

/// Byte offset of a 1-based line and character column.
fn source_offset(source: &str, line: usize, col: usize) -> usize {
    let line_start: usize = source
        .split_inclusive('\n')
        .take(line - 1)
        .map(str::len)
        .sum();
    line_start
        + source
            .get(line_start..)
            .unwrap_or_default()
            .char_indices()
            .nth(col - 1)
            .map_or(0, |(offset, _)| offset)
}

fn blake3_hex(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

/// Each call record names the reasoning effort, the offered tools with their
/// titles, and the tool results the model read, without provider shapes.
#[test]
fn a_recorded_call_states_effort_tools_and_tool_results() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tape = record_sample();
    let facts: Vec<CallFacts> = tape
        .records
        .iter()
        .filter_map(|record| match &record.kind {
            TapeRecordKind::LlmCall { facts, .. } => facts.as_deref().cloned(),
            _ => None,
        })
        .collect();
    assert_eq!(facts.len(), 3, "one fact record per model call");
    let reasoning = facts[0]
        .reasoning
        .as_ref()
        .expect("the call asked for reasoning");
    assert_eq!(reasoning.effort.as_deref(), Some("medium"), "{reasoning:?}");
    let titles: Vec<(&str, Option<&str>)> = facts[0]
        .tools
        .iter()
        .map(|tool| (tool.name.as_str(), tool.title.as_deref()))
        .collect();
    assert!(
        titles.contains(&("read_file", Some("Read a file")))
            && titles.contains(&("run_tests", Some("Run the tests"))),
        "offered tools: {titles:?}"
    );
    assert!(facts[0].tool_results.is_empty(), "{:?}", facts[0]);
    let last = &facts[2].tool_results;
    let seen: Vec<(Option<&str>, &str)> = last
        .iter()
        .map(|result| (result.tool.as_deref(), result.text.as_str()))
        .collect();
    assert_eq!(
        seen.len(),
        2,
        "both results reach the third call: {last:#?}"
    );
    assert_eq!(seen[0].0, Some("read_file"), "{last:#?}");
    assert!(seen[0].1.contains("fn main()"), "{last:#?}");
    assert_eq!(seen[1].0, Some("run_tests"), "{last:#?}");
    assert!(seen[1].1.contains("3 passed"), "{last:#?}");
}
