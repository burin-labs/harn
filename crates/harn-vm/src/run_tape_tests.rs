use super::*;
use crate::value::VmValue;
use harn_builtin_meta::{EffectAccess, EffectKind, EffectSpec};

/// Every test that touches the process tape or branch holds this lock: both
/// are process-global by design.
static PROCESS_LOCK: Mutex<()> = Mutex::new(());

fn request(text: &str) -> LlmRequestPayload {
    LlmRequestPayload {
        provider: "anthropic".to_string(),
        model: "model-a".to_string(),
        messages: vec![serde_json::json!({"role": "user", "content": text})],
        system: Some("system".to_string()),
        max_tokens: 100,
        api_key: "secret-never-recorded".to_string(),
        ..LlmRequestPayload::default()
    }
}

fn result(text: &str, tool_call_id: Option<&str>) -> LlmResult {
    LlmResult {
        text: text.to_string(),
        tool_calls: tool_call_id
            .map(|id| vec![serde_json::json!({"id": id, "name": "write", "arguments": {}})])
            .unwrap_or_default(),
        model: "model-a".to_string(),
        provider: "anthropic".to_string(),
        ..LlmResult::default()
    }
}

fn ok(value: serde_json::Value) -> RecordedToolResult {
    RecordedToolResult::Ok { value }
}

struct FixedObserver;

impl ToolEffectObserver for FixedObserver {
    fn begin(&self, _root: &Path, _call_id: &str) {}
    fn end(&self, _root: &Path, call_id: &str) -> EffectScan {
        EffectScan {
            changes: vec![(format!("src/{call_id}.txt"), Some(b"written".to_vec()))],
            incomplete: false,
        }
    }
}

/// A full tape whose tool calls a branch may serve.
fn record_full(dir: &Path) -> RunTapeGuard {
    let workspace = TapeWorkspace {
        root: dir.to_path_buf(),
        observer: Box::new(FixedObserver),
    };
    start_recording(dir, RecordingLevel::Full, Some(workspace))
        .unwrap()
        .unwrap()
}

fn branch(dir: &Path, k: u64, workspace: &Path) -> BranchGuard {
    start_branch(BranchPlan {
        tape: EventTape::load(&dir.join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: k,
        workspace_root: workspace.to_path_buf(),
    })
    .unwrap()
}

fn record_tool(id: &str, args: &serde_json::Value, outcome: Result<RecordedToolResult, String>) {
    let recording = begin_tool_call(id, "write", args, false).unwrap();
    finish_tool_call(recording, outcome);
}

/// Record a two-call run: call 1 asks for tool `t1`, call 2 finishes.
fn record_run(dir: &Path) {
    let guard = record_full(dir);
    record_llm_call(&request("first"), &result("calling", Some("t1")));
    record_tool(
        "t1",
        &serde_json::json!({"path": "src/t1.txt"}),
        Ok(ok(serde_json::json!("done"))),
    );
    record_llm_call(&request("second"), &result("finished", None));
    assert_eq!(guard.error(), None);
}

#[test]
fn a_zero_edit_branch_serves_the_prefix_and_reapplies_tool_effects() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let (tape_dir, branch_ws) = (tmp.path().join("tape"), tmp.path().join("branch"));
    std::fs::create_dir_all(&branch_ws).unwrap();
    record_run(&tape_dir);

    let tape_text = std::fs::read_to_string(tape_dir.join(TAPE_FILE_NAME)).unwrap();
    assert!(!tape_text.contains("secret-never-recorded"));

    let branch = branch(&tape_dir, 1, &branch_ws);
    let served = serve_llm_call(&request("first")).unwrap().unwrap();
    assert_eq!(served.text, "calling");
    assert_eq!(served.telemetry.source, "branch_served");

    // The tool named by the served response returns its recorded result and
    // writes its recorded effect into the branch workspace without running.
    match serve_tool_call("t1", "write", &serde_json::json!({"path": "src/t1.txt"})) {
        ToolServe::Served(outcome) => assert_eq!(outcome, ok(serde_json::json!("done"))),
        _ => panic!("expected a served tool call"),
    }
    assert_eq!(
        std::fs::read(branch_ws.join("src/t1.txt")).unwrap(),
        b"written"
    );

    // Past K the call is live.
    assert!(serve_llm_call(&request("anything")).is_none());
    let report = branch.finish();
    assert_eq!(
        (
            report.served_calls,
            report.served_tool_calls,
            report.live_calls
        ),
        (1, 1, 1)
    );
    assert_eq!(report.divergence, None);
}

#[test]
fn a_prefix_request_mismatch_fails_closed_and_never_goes_live() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    record_run(tmp.path());

    let branch = branch(tmp.path(), 2, &tmp.path().join("branch"));
    let error = serve_llm_call(&request("edited")).unwrap().unwrap_err();
    let message = error.to_string();
    assert!(message.contains("model call 1"), "{message}");
    assert!(message.contains("messages[0]"), "{message}");
    // Every later call, including one past K, is refused rather than sent.
    assert!(serve_llm_call(&request("first")).unwrap().is_err());
    assert_eq!(branch.report().divergence.unwrap().call, 1);
}

#[test]
fn the_request_digest_covers_every_request_setting_but_the_credential_and_session() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    {
        let _guard = record_full(tmp.path());
        record_llm_call(&request("first"), &result("ok", None));
    }
    type Edit = fn(&mut LlmRequestPayload);
    let edits: [(&str, Edit); 7] = [
        ("temperature", |r| r.temperature = Some(0.5)),
        ("reasoning_effort_source", |r| {
            r.reasoning_effort_source = harn_llm::effective_effort::ReasoningEffortSource::Operator;
        }),
        ("portable_option_intent", |r| {
            r.portable_option_intent
                .insert(harn_provider_catalog::capabilities::PortableOption::Seed);
        }),
        ("provider", |r| r.provider = "openai".to_string()),
        ("output_schema", |r| {
            r.output_schema = Some(serde_json::json!({"type": "object"}));
        }),
        ("done_sentinel", |r| {
            r.done_sentinel = Some("DONE".to_string());
        }),
        ("fast", |r| r.fast = true),
    ];
    for (field, edit) in edits {
        let _branch = branch(tmp.path(), 1, tmp.path());
        let mut edited = request("first");
        edit(&mut edited);
        let error = serve_llm_call(&edited).unwrap().unwrap_err().to_string();
        assert!(error.contains(field), "{field}: {error}");
    }
    let _branch = branch(tmp.path(), 1, tmp.path());
    let mut same = request("first");
    same.api_key = "another-key".to_string();
    same.session_id = Some("another-session".to_string());
    assert!(serve_llm_call(&same).unwrap().is_ok());
}

/// A field serde skips never reaches the digest by default, so each one
/// must be named as covered or as deliberately left out.
#[test]
fn every_unserialized_request_field_is_covered_or_excluded_by_the_digest() {
    let source = include_str!("../../harn-llm/src/request.rs");
    let struct_body = source
        .split("pub struct LlmRequestPayload {")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("LlmRequestPayload is in request.rs");
    let mut skipped = Vec::new();
    let mut lines = struct_body.lines();
    while let Some(line) = lines.next() {
        if line.trim() != "#[serde(skip_serializing)]" {
            continue;
        }
        let field = lines
            .by_ref()
            .map(str::trim)
            .find(|line| line.starts_with("pub "))
            .and_then(|line| line.strip_prefix("pub "))
            .and_then(|line| line.split(':').next())
            .expect("a skipped attribute precedes a field");
        skipped.push(field.to_string());
    }
    assert!(skipped.len() >= 9, "parsed skipped fields: {skipped:?}");
    for field in &skipped {
        assert!(
            REQUEST_DIGEST_UNSERIALIZED.contains(&field.as_str())
                ^ REQUEST_DIGEST_UNSERIALIZED_EXCLUDED.contains(&field.as_str()),
            "`{field}` is skipped by serde: list it in exactly one of \
             REQUEST_DIGEST_UNSERIALIZED or REQUEST_DIGEST_UNSERIALIZED_EXCLUDED"
        );
    }
    for field in REQUEST_DIGEST_UNSERIALIZED
        .iter()
        .chain(REQUEST_DIGEST_UNSERIALIZED_EXCLUDED)
    {
        assert!(
            skipped.iter().any(|s| s == field),
            "`{field}` is not skipped"
        );
    }
}

#[test]
fn a_metadata_tape_refuses_to_branch() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    {
        let _guard = start_recording(tmp.path(), RecordingLevel::Metadata, None)
            .unwrap()
            .unwrap();
        record_llm_call(&request("first"), &result("done", None));
    }
    let error = start_branch(BranchPlan {
        tape: EventTape::load(&tmp.path().join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: 1,
        workspace_root: tmp.path().to_path_buf(),
    })
    .err()
    .unwrap();
    assert!(error.contains("can be read but not branched"), "{error}");
}

#[test]
fn a_tape_recorded_without_a_workspace_observer_refuses_to_branch() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    {
        let _guard = start_recording(tmp.path(), RecordingLevel::Full, None)
            .unwrap()
            .unwrap();
        record_llm_call(&request("first"), &result("done", None));
    }
    let error = start_branch(BranchPlan {
        tape: EventTape::load(&tmp.path().join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: 1,
        workspace_root: tmp.path().to_path_buf(),
    })
    .err()
    .unwrap();
    assert!(
        error.contains("without a workspace effect observer"),
        "{error}"
    );
}

#[test]
fn an_unservable_prefix_tool_call_diverges_instead_of_running() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    {
        let _guard = record_full(tmp.path());
        record_llm_call(&request("first"), &result("calling", Some("t1")));
        record_tool(
            "t1",
            &serde_json::json!({}),
            Err("tool error has no recorded shape".to_string()),
        );
    }
    let branch = branch(tmp.path(), 1, &tmp.path().join("branch"));
    serve_llm_call(&request("first")).unwrap().unwrap();
    assert!(matches!(
        serve_tool_call("t1", "write", &serde_json::json!({})),
        ToolServe::Diverged(_)
    ));
    let divergence = branch.finish().divergence.unwrap();
    assert_eq!(divergence.what, "unservable tool call");
    assert!(divergence.detail.contains("no recorded shape"));
}

#[test]
fn a_run_that_ends_before_its_prefix_is_consumed_diverges() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    record_run(tmp.path());
    // Fewer model calls than K.
    let short = branch(tmp.path(), 2, &tmp.path().join("branch"));
    serve_llm_call(&request("first")).unwrap().unwrap();
    serve_tool_call("t1", "write", &serde_json::json!({"path": "src/t1.txt"}));
    let divergence = short.finish().divergence.unwrap();
    assert_eq!(
        (divergence.what.as_str(), divergence.call),
        ("under-run prefix", 2)
    );
    drop(short);
    // Every model call served, but a recorded tool call never made.
    let skipped = branch(tmp.path(), 1, &tmp.path().join("branch"));
    serve_llm_call(&request("first")).unwrap().unwrap();
    let divergence = skipped.finish().divergence.unwrap();
    assert!(divergence.detail.contains("`write` call t1 was never made"));
}

#[test]
fn a_prefix_refuses_effects_no_recording_serves_and_serves_recorded_reads() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    const CLOCK: &[EffectSpec] = &[EffectSpec {
        kind: EffectKind::Clock,
        access: EffectAccess::Read,
        resources: &[],
    }];
    const EXEC: &[EffectSpec] = &[EffectSpec {
        kind: EffectKind::Process,
        access: EffectAccess::Write,
        resources: &[],
    }];
    {
        let _guard = record_full(tmp.path());
        record_read(None, "now_ms", Some(CLOCK), &[], &Ok(VmValue::Int(1234)));
        record_llm_call(&request("first"), &result("ok", None));
    }
    let branch = branch(tmp.path(), 1, tmp.path());
    // The clock read answers with the recorded value, not the time now.
    let served = serve_read(None, "now_ms", Some(CLOCK), &[])
        .unwrap()
        .unwrap();
    assert_eq!(served.display(), "1234");
    // A pure call runs.
    assert!(serve_read(None, "len", Some(&[]), &[]).is_none());
    // An exec would act on the world again: refused, and the branch diverged.
    let refused = serve_read(None, "process_run", Some(EXEC), &[]).unwrap();
    assert!(refused.unwrap_err().to_string().contains("process_run"));
    let report = branch.finish();
    assert_eq!(report.served_reads, 1);
    assert_eq!(report.divergence.unwrap().what, "refused effect");
}

#[test]
fn a_prefix_read_the_recording_never_made_diverges() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    const RANDOM: &[EffectSpec] = &[EffectSpec {
        kind: EffectKind::Random,
        access: EffectAccess::Read,
        resources: &[],
    }];
    {
        let _guard = record_full(tmp.path());
        record_llm_call(&request("first"), &result("ok", None));
    }
    let _branch = branch(tmp.path(), 1, tmp.path());
    let read = serve_read(None, "uuid", Some(RANDOM), &[]).unwrap();
    assert!(read.is_err());
}

#[test]
fn a_branch_store_starts_from_the_recording_and_never_writes_its_file() {
    use crate::store::{store_get, StoreState};
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("store.json");
    std::fs::write(&path, r#"{"memo": "recorded"}"#).unwrap();
    {
        let _guard = record_full(tmp.path());
        let store = parking_lot::Mutex::new(StoreState::at_path(path.clone()));
        assert_eq!(store_get(&store, "memo").unwrap().display(), "recorded");
        store
            .lock()
            .set("memo".to_string(), serde_json::json!("later"))
            .unwrap();
        record_llm_call(&request("work"), &result("done", None));
    }
    // The recorded run, and others since, left the file holding a later value.
    std::fs::write(&path, r#"{"memo": "live"}"#).unwrap();
    let _branch = branch(tmp.path(), 1, tmp.path());
    let store = parking_lot::Mutex::new(StoreState::at_path(path.clone()));
    assert_eq!(store_get(&store, "memo").unwrap().display(), "recorded");
    store
        .lock()
        .set("memo".to_string(), serde_json::json!("branch"))
        .unwrap();
    assert_eq!(store_get(&store, "memo").unwrap().display(), "branch");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        r#"{"memo": "live"}"#
    );
}

#[test]
fn a_torn_final_tape_record_loads_the_complete_ones() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    record_run(tmp.path());
    let path = tmp.path().join(TAPE_FILE_NAME);
    let mut body = std::fs::read_to_string(&path).unwrap();
    let complete = EventTape::load(&path).unwrap().records.len();
    body.push_str(r#"{"type":"record","seq":99,"kind":{"llm_c"#);
    std::fs::write(&path, &body).unwrap();
    assert_eq!(EventTape::load(&path).unwrap().records.len(), complete);
    // A malformed line anywhere else still rejects the tape.
    body.push('\n');
    std::fs::write(&path, &body).unwrap();
    assert!(EventTape::load(&path).is_err());
}

#[test]
fn a_model_call_attributes_its_system_prompt_to_the_renders_it_contains() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let guard = start_recording(tmp.path(), RecordingLevel::Full, None)
        .unwrap()
        .unwrap();
    let floor = "You are a careful coding agent. Read before you edit.";
    let tools = "Call one tool at a time and wait for its result.";
    let floor_id = record_prompt_render("@/prompts/floor.harn.prompt", floor, &[]);
    record_prompt_render("@/prompts/tools.harn.prompt", tools, &[]);
    let mut first = request("task");
    first.system = Some(format!("{floor}\n\n{tools}\nunattributed tail"));
    record_llm_call(&first, &result("ok", None));
    // The floor is not re-rendered, yet the second call that resends it still
    // attributes it.
    let mut second = request("task");
    second.system = Some(floor.to_string());
    record_llm_call(&second, &result("ok", None));
    assert_eq!(guard.error(), None);
    assert_eq!(render_consumers(&floor_id), vec![1, 2]);
    drop(guard);

    let tape = EventTape::load(&tmp.path().join(TAPE_FILE_NAME)).unwrap();
    let attributions: Vec<_> = tape
        .records
        .iter()
        .filter_map(|record| match &record.kind {
            TapeRecordKind::LlmCall { attribution, .. } => attribution.clone(),
            _ => None,
        })
        .collect();
    let first = &attributions[0];
    assert_eq!(first.segments.len(), 2);
    assert_eq!(first.segments[0].field, "system");
    assert_eq!(first.segments[0].start, 0);
    assert_eq!(
        first.system_attributed,
        (floor.len() + tools.len()) as u64,
        "the separator and tail stay unattributed"
    );
    assert_eq!(
        attributions[1].system_attributed,
        attributions[1].system_bytes
    );
    let renders = tape
        .records
        .iter()
        .filter(|record| matches!(record.kind, TapeRecordKind::PromptRender { .. }))
        .count();
    assert_eq!(renders, 2);
}

#[test]
fn a_branch_replays_recorded_cache_misses_even_when_the_cache_now_hits() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    {
        let _guard = record_full(tmp.path());
        assert_eq!(cache_lookup("intent", "k1", || Ok(None)).unwrap(), None);
        record_llm_call(&request("classify"), &result("feature", None));
    }
    let branch_one = branch(tmp.path(), 1, tmp.path());
    // Since the recording, another run stored an answer under k1. The branch
    // must still see the recorded miss, or it skips the call it must serve.
    let read = cache_lookup("intent", "k1", || Ok(Some(serde_json::json!("stale"))));
    assert_eq!(read.unwrap(), None);
    assert!(serve_llm_call(&request("classify")).unwrap().is_ok());
    drop(branch_one);
    // A key the recording never read, such as one built from this run's
    // session id, has nothing to replay and is read live.
    let branch_two = branch(tmp.path(), 1, tmp.path());
    let fresh = cache_lookup("intent", "session-b", || {
        Ok(Some(serde_json::json!("live")))
    });
    assert_eq!(fresh.unwrap(), Some(serde_json::json!("live")));
    assert_eq!(branch_two.report().live_cache_reads, 1);
}

#[test]
fn a_prefix_tool_call_with_a_fresh_id_is_served_by_tool_and_arguments() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    record_run(tmp.path());
    let _branch = branch(tmp.path(), 1, &tmp.path().join("branch"));
    serve_llm_call(&request("first")).unwrap().unwrap();
    // A text-format parse names the same call `tc_7`; it must not run live.
    let args = serde_json::json!({"path": "src/t1.txt"});
    assert!(matches!(
        serve_tool_call("tc_7", "write", &args),
        ToolServe::Served(_)
    ));
    // A second identical call has no recording left to serve and diverges.
    assert!(matches!(
        serve_tool_call("tc_8", "write", &args),
        ToolServe::Diverged(_)
    ));
}

#[test]
fn a_reused_tool_call_id_after_the_branch_goes_live_runs_live() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    record_run(tmp.path());
    let _branch = branch(tmp.path(), 1, &tmp.path().join("branch"));
    serve_llm_call(&request("first")).unwrap().unwrap();
    serve_tool_call("t1", "write", &serde_json::json!({"path": "src/t1.txt"}));
    assert!(serve_llm_call(&request("live")).is_none());
    // The live response reuses `t1`; the recorded result must not answer it.
    let args = serde_json::json!({"path": "src/t1.txt"});
    assert!(matches!(
        serve_tool_call("t1", "write", &args),
        ToolServe::Live
    ));
}

#[test]
fn a_prefix_cache_read_is_served_only_where_the_recording_made_it() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    {
        let _guard = record_full(tmp.path());
        cache_lookup("intent", "k", || Ok(Some(serde_json::json!("v1")))).unwrap();
        record_llm_call(&request("one"), &result("a", None));
        record_llm_call(&request("two"), &result("b", None));
        cache_lookup("intent", "k", || Ok(Some(serde_json::json!("v2")))).unwrap();
    }
    let _branch = branch(tmp.path(), 2, tmp.path());
    let read = || cache_lookup("intent", "k", || Ok(None)).unwrap();
    assert_eq!(read(), Some(serde_json::json!("v1")));
    serve_llm_call(&request("one")).unwrap().unwrap();
    // The recording read `k` again only after call 2, so a read here is new.
    assert!(cache_lookup("intent", "k", || Ok(None)).is_err());
}

/// A misspelled entry would silently allow nothing; each must name a real
/// `harness.agent` method whose state effects all target an argument.
#[test]
fn every_session_entry_method_is_a_harness_agent_method_scoped_to_an_argument() {
    use harn_builtin_meta::{BuiltinExposure, CapabilityId, ResourceSelector};
    for method in prefix::SESSION_ENTRY_METHODS {
        let name = format!("__cap_agent_{method}");
        let contract = crate::stdlib::builtin_policy_metadata(&name)
            .1
            .unwrap_or_else(|| panic!("`{name}` has no contract"));
        assert!(
            matches!(
                contract.exposure,
                BuiltinExposure::HarnessMethod { capability: CapabilityId::Agent, method: m } if m == *method
            ),
            "`{name}` is not harness.agent.{method}"
        );
        for effect in contract.effects {
            if effect.kind == EffectKind::State {
                assert!(
                    effect.resources.iter().all(|resource| matches!(
                        resource,
                        ResourceSelector::Argument(_)
                            | ResourceSelector::Constant("agent-sessions")
                    )),
                    "`{name}` writes state outside the session table: {effect:?}"
                );
            }
        }
    }
}
