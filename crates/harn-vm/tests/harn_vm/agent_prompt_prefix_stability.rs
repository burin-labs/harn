//! Append-only prefix invariant for the provider-visible message array.
//!
//! Every production prompt cache is a *prefix* cache: Anthropic hashes the
//! prefix ending at each breakpoint, OpenAI matches an initial run of tokens,
//! vLLM chains a hash per KV block over the tokens before it, and a
//! llama.cpp slot keeps only the longest common prefix and re-prefills from
//! the first divergent token onward. None of them can reuse anything after
//! the first byte that changed.
//!
//! The invariant that makes an agent loop cacheable is therefore mechanical:
//!
//!   the serialized message array at request N+1 begins with the serialized
//!   message array at request N.
//!
//! Deliberate compaction is the documented exception; it starts a new prefix
//! on purpose and pays one cache write to buy a large token reduction.
//!
//! These tests capture `call.opts.messages` from a deterministic `llm_caller`
//! on consecutive iterations of a real `agent_loop` and compare the arrays
//! element by element. No provider and no model inference is involved.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use harn_vm::bridge::HostBridge;
use harn_vm::value::VmError;

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

fn fresh_session_id(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        std::process::id(),
        SESSION_COUNTER.fetch_add(1, Ordering::Relaxed),
    )
}

fn run_with_bridge(source: &str) -> Result<String, String> {
    harn_vm::reset_thread_local_state();
    let session_store_root = tempfile::tempdir().map_err(|e| e.to_string())?;
    let session_store_root_path = session_store_root
        .path()
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let source = source.replace("__HARN_TEST_SESSION_STORE_ROOT__", &session_store_root_path);
    let source = format!("import {{ agent_loop }} from \"std/agent/loop\"\n{source}");
    let chunk = harn_vm::compile_source(&source)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let bridge = Arc::new(HostBridge::from_parts(
                    Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(Mutex::new(())),
                    1,
                ));
                harn_vm::llm::install_current_host_bridge(bridge.clone());
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                let result = vm
                    .execute(&chunk)
                    .await
                    .map_err(|e: VmError| format!("{e:?}"));
                harn_vm::llm::clear_current_host_bridge();
                result?;
                Ok(vm.output().to_string())
            })
            .await
    })
}

fn out_lines(raw: &str) -> Vec<String> {
    raw.lines()
        .filter_map(|l| l.strip_prefix("[harn] "))
        .map(|s| s.to_string())
        .collect()
}

/// Two-iteration `agent_loop` with one pending reminder alive across both
/// turns. Iteration 0 returns a tool call, so iteration 1 sees the durable
/// history grow by an assistant turn plus a tool result — the ordinary shape
/// of every agent loop.
///
/// The mock caller JSON-encodes `call.opts.messages` for each iteration and
/// logs it on one line behind a `request ` marker. Encoding in Harn keeps the
/// captured bytes exactly the array the loop handed the transport, and keeps
/// embedded newlines (the directive envelope has several) from splitting the
/// capture across log lines.
fn prefix_probe_pipeline(session_id: &str) -> String {
    format!(
        r#"
pipeline main(harness: Harness, task: unknown) {{
  harness.tools.clear_hooks()
  const registry = tool_registry()
  const tools = tool_define(
    registry,
    "inspect",
    "Deterministic stand-in tool.",
    {{parameters: {{}}, handler: {{ _args -> return "inspected" }}}},
  )

  // One reminder, injected before the first turn and alive for both turns.
  // `ttl_turns: 4` keeps it pending across the whole run so the arrays under
  // test differ only in where the directive envelope lands.
  harness.agent.session_push_bridge_injection(
    "{session_id}",
    {{
      body: "Re-read the file before editing it.",
      mode: "finish_step",
      dedupe_key: "prefix-probe",
      ttl_turns: 4,
    }},
  )

  const iteration_state = harness.runtime.shared_cell(
    {{scope: "task_group", key: "iter-{session_id}", initial: 0}},
  )
  const mock_llm = {{ call ->
    const snap = harness.runtime.shared_snapshot(iteration_state)
    const n = snap.value
    harness.runtime.shared_cas(iteration_state, snap, n + 1)
    let captured = []
    for message in (call?.opts?.messages ?? []) {{
      captured = captured.appending(
        {{role: to_string(message?.role ?? ""), content: to_string(message?.content ?? "")}},
      )
    }}
    harness.stdio.log("request " + json_stringify(captured))
    if n == 0 {{
      return {{
        ok: true,
        value: {{
          text: "",
          tool_calls: [{{id: "call_1", name: "inspect", arguments: {{}}}}],
          provider: "mock",
          model: "mock",
        }},
      }}
    }}
    return {{
      ok: true,
      value: {{text: "finished ##DONE##", tool_calls: [], provider: "mock", model: "mock"}},
    }}
  }}

  const result = agent_loop(
    harness,
    "summarize the module",
    nil,
    {{
      provider: "mock",
      tools: tools,
      tool_format: "native",
      root: "__HARN_TEST_SESSION_STORE_ROOT__",
      max_iterations: 4,
      loop_until_done: true,
      session_id: "{session_id}",
      llm_caller: mock_llm,
    }},
  )
  harness.stdio.log("status " + result.status)
}}
"#
    )
}

/// Three-iteration `agent_loop` where one durable reminder provider is
/// evaluated after every request. Each evaluation constructs a fresh reminder
/// instance with the same logical key and model-visible contract body.
fn durable_provider_reassertion_pipeline(session_id: &str) -> String {
    format!(
        r#"
import {{ agent_reminder_providers_fire }} from "std/agent/state"

pipeline main(harness: Harness, task: unknown) {{
  harness.tools.clear_hooks()
  harness.agent.clear_reminder_providers()
  harness.agent.register_reminder_provider(
    {{
      id: "durable_contract_provider",
      subscribes_to: ["session_idle"],
      evaluate: {{ _ctx ->
        return {{
          reminder: {{
            body: "DURABLE_CONTRACT_MARKER",
            tags: ["durable-contract"],
            dedupe_key: "durable-contract",
            preserve_on_compact: true,
            authority: "contract",
          }},
        }}
      }},
    }},
  )
  const session_id = harness.agent.open("{session_id}")
  harness.agent.reset(session_id)
  const registry = tool_registry()
  const tools = tool_define(
    registry,
    "inspect",
    "Deterministic stand-in tool.",
    {{parameters: {{}}, handler: {{ _args -> return "inspected" }}}},
  )

  let _ = agent_reminder_providers_fire(
    harness.agent,
    session_id,
    "session_idle",
    {{session: {{id: session_id}}, iteration: 0, wake_interval_ms: 0}},
    {{}},
  )

  const iteration_state = harness.runtime.shared_cell(
    {{scope: "task_group", key: "durable-iter-{session_id}", initial: 0}},
  )
  const mock_llm = {{ call ->
    const snap = harness.runtime.shared_snapshot(iteration_state)
    const n = snap.value
    harness.runtime.shared_cas(iteration_state, snap, n + 1)
    let captured = []
    for message in (call?.opts?.messages ?? []) {{
      captured = captured.appending(
        {{role: to_string(message?.role ?? ""), content: to_string(message?.content ?? "")}},
      )
    }}
    harness.stdio.log("request " + json_stringify(captured))
    if n < 2 {{
      let _ = agent_reminder_providers_fire(
        harness.agent,
        session_id,
        "session_idle",
        {{session: {{id: session_id}}, iteration: n + 1, wake_interval_ms: 0}},
        {{}},
      )
      return {{
        ok: true,
        value: {{
          text: "",
          tool_calls: [{{
            id: "call_" + to_string(n),
            name: "inspect",
            arguments: {{}},
          }}],
          provider: "mock",
          model: "mock",
        }},
      }}
    }}
    return {{
      ok: true,
      value: {{text: "finished ##DONE##", tool_calls: [], provider: "mock", model: "mock"}},
    }}
  }}

  const result = agent_loop(
    harness,
    "summarize the module",
    nil,
    {{
      provider: "mock",
      tools: tools,
      tool_format: "native",
      root: "__HARN_TEST_SESSION_STORE_ROOT__",
      max_iterations: 4,
      loop_until_done: true,
      session_id: session_id,
      llm_caller: mock_llm,
    }},
  )
  harness.agent.clear_reminder_providers()
  harness.stdio.log("status " + result.status)
}}
"#
    )
}

/// One captured provider request: the message array exactly as the loop
/// handed it to the transport, as `(role, content)` pairs.
type CapturedRequest = Vec<(String, String)>;

fn captured_requests(lines: &[String]) -> Vec<CapturedRequest> {
    lines
        .iter()
        .filter_map(|line| line.strip_prefix("request "))
        .map(|encoded| {
            let messages: Vec<serde_json::Value> =
                serde_json::from_str(encoded).expect("captured request must be a JSON array");
            messages
                .into_iter()
                .map(|message| {
                    let field = |key: &str| {
                        message
                            .get(key)
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    };
                    (field("role"), field("content"))
                })
                .collect()
        })
        .collect()
}

/// Report the first index at which `next` stops agreeing with `previous`,
/// or `None` when `next` extends `previous` without changing it.
fn first_divergence(previous: &CapturedRequest, next: &CapturedRequest) -> Option<usize> {
    if next.len() < previous.len() {
        return Some(next.len().min(previous.len()));
    }
    previous
        .iter()
        .zip(next.iter())
        .position(|(before, after)| before != after)
}

fn assert_append_only(requests: &[CapturedRequest]) {
    assert!(
        requests.len() >= 2,
        "the probe must capture at least two provider requests; got {}",
        requests.len()
    );
    // "N+1 begins with N" is trivially true when every capture is identical,
    // which is exactly what a loop that appended nothing would produce. Require
    // the transcript to have actually grown, or the walk below proves nothing.
    let (first, last) = (
        requests.first().expect("checked non-empty above"),
        requests.last().expect("checked non-empty above"),
    );
    assert!(
        last.len() > first.len(),
        "the captured requests must differ: the last request has {} messages and the first has \
         {}, so an append-only walk over them is vacuous",
        last.len(),
        first.len(),
    );
    for window in requests.windows(2) {
        let (previous, next) = (&window[0], &window[1]);
        if let Some(index) = first_divergence(previous, next) {
            let before = previous
                .get(index)
                .map(|(role, content)| format!("{role}: {content:?}"))
                .unwrap_or_else(|| "<absent>".to_string());
            let after = next
                .get(index)
                .map(|(role, content)| format!("{role}: {content:?}"))
                .unwrap_or_else(|| "<absent>".to_string());
            panic!(
                "append-only prefix invariant violated at message index {index}: request N+1 \
                 must begin with request N.\n  request N   [{index}] = {before}\n  request N+1 \
                 [{index}] = {after}\n  request N   len = {}, request N+1 len = {}",
                previous.len(),
                next.len(),
            );
        }
    }
}

/// The contract. Before append-only placement this failed at message index 0: the directive
/// envelope was folded into the trailing `user` turn while that turn was last,
/// then dropped from it once the conversation grew past it. The very first
/// message of the transcript was rewritten between consecutive requests, so
/// every provider re-prefilled the entire prompt on every turn.
#[test]
fn reminder_placement_keeps_the_request_prefix_append_only() {
    let raw = run_with_bridge(&prefix_probe_pipeline(&fresh_session_id(
        "prefix-append-only",
    )))
    .expect("script must run");
    let lines = out_lines(&raw);
    assert!(
        lines.iter().any(|line| line == "status done"),
        "loop must reach a terminal `done` status; lines: {lines:?}"
    );
    let requests = captured_requests(&lines);
    // Pin the iteration count exactly. The mock answers the first call with a
    // tool call and only then finishes, so a run that reached `done` on one
    // provider call means the loop never took a second turn and the invariant
    // was never actually exercised.
    assert_eq!(
        requests.len(),
        2,
        "the loop must have taken exactly two turns; requests: {requests:#?}"
    );
    assert_append_only(&requests);

    // The invariant is satisfiable by a run that emitted no directive at all,
    // so pin that the directive really fired, that its bytes survive into the
    // later request unchanged, and that it is carried rather than re-emitted.
    let occurrences = |request: &CapturedRequest| {
        request
            .iter()
            .filter(|(_, content)| content.contains("Re-read the file before editing it."))
            .count()
    };
    assert_eq!(
        occurrences(&requests[0]),
        1,
        "the directive must fire on the first request; requests: {requests:#?}"
    );
    let last = requests.last().expect("at least one request");
    assert_eq!(
        occurrences(last),
        1,
        "an unchanged directive must be carried, not re-issued each turn; requests: {requests:#?}"
    );
    // It rides as its own trailing user turn, never folded into the task.
    assert_eq!(requests[0][0].1, "summarize the module");
    let envelope = requests[0]
        .iter()
        .find(|(_, content)| content.contains("context-directives"))
        .expect("the envelope must be present on the first request");
    assert_eq!(envelope.0, "user");
    assert!(
        last.contains(envelope),
        "the committed envelope must reappear verbatim in the later request"
    );
}

/// Regression for harn#7397. A durable reminder provider may re-evaluate its
/// unchanged contract every turn. Fresh evaluation ids must not grow the
/// provider prompt with byte-identical directive copies.
#[test]
fn durable_provider_reassertion_is_committed_once_across_three_turns() {
    let raw = run_with_bridge(&durable_provider_reassertion_pipeline(&fresh_session_id(
        "durable-provider-reassertion",
    )))
    .expect("script must run");
    let lines = out_lines(&raw);
    assert!(
        lines.iter().any(|line| line == "status done"),
        "loop must reach a terminal `done` status; lines: {lines:?}"
    );
    let requests = captured_requests(&lines);
    assert_eq!(
        requests.len(),
        3,
        "two tool calls must force exactly three provider requests; requests: {requests:#?}"
    );
    assert_append_only(&requests);
    for window in requests.windows(2) {
        assert!(
            window[1].len() > window[0].len(),
            "each tool turn must grow durable history; requests: {requests:#?}"
        );
    }

    let occurrences: Vec<usize> = requests
        .iter()
        .map(|request| {
            request
                .iter()
                .map(|(_, content)| content.matches("DURABLE_CONTRACT_MARKER").count())
                .sum()
        })
        .collect();
    assert_eq!(
        occurrences,
        vec![1, 1, 1],
        "the unchanged durable directive must fire and remain exactly once per request; \
         requests: {requests:#?}"
    );
}

/// The comparator has to be able to fail, or the contract test above proves
/// nothing. Feed it the exact shape the old placement produced — a first user
/// turn that carries the envelope on one request and loses it on the next —
/// and require it to report the divergence at index 0.
#[test]
fn the_prefix_comparator_reports_a_rewritten_first_turn() {
    let rewritten: CapturedRequest = vec![(
        "user".to_string(),
        "summarize the module\n\n<context-directives>…</context-directives>".to_string(),
    )];
    let plain: CapturedRequest = vec![
        ("user".to_string(), "summarize the module".to_string()),
        ("assistant".to_string(), String::new()),
    ];
    assert_eq!(first_divergence(&rewritten, &plain), Some(0));

    // And it must not cry wolf on a genuine append.
    let appended: CapturedRequest = vec![
        rewritten[0].clone(),
        ("assistant".to_string(), "ok".to_string()),
    ];
    assert_eq!(first_divergence(&rewritten, &appended), None);
}
