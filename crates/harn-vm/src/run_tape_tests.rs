use super::*;
use crate::value::VmValue;
use harn_builtin_meta::{EffectAccess, EffectKind, EffectSpec};

#[path = "run_tape_prompt_locations_tests.rs"]
mod prompt_locations;

#[path = "run_tape_overlap_tests.rs"]
mod overlap;

#[path = "run_tape_session_reset_tests.rs"]
mod session_reset;

#[path = "run_tape_reminder_tests.rs"]
mod reminders;

#[path = "run_tape_store_tests.rs"]
mod store_changes;

/// Every test that touches the process tape or branch holds this lock: both
/// are process-global by design.
use super::TEST_PROCESS_LOCK as PROCESS_LOCK;

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
        state_roots: Vec::new(),
    };
    start_recording(dir, RecordingLevel::Full, Some(workspace))
        .unwrap()
        .unwrap()
}

#[test]
fn version_four_transcripts_remain_readable_without_claiming_lifecycle_evidence() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let guard = record_full(dir.path());
    let legacy = serde_json::json!({
        "session_id": "legacy-session",
        "fields": {"messages": {"append": [{"role": "user", "content": "legacy"}]}}
    });
    let change: TranscriptChange = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(serde_json::to_value(&change).unwrap(), legacy);
    let mut recording =
        begin_tool_call("legacy-tool", "read", &serde_json::json!({}), true).unwrap();
    recording.transcript_changes.push(change.clone());
    finish_tool_call(recording, Ok(ok(serde_json::json!("read"))));
    drop(guard);
    let path = dir.path().join(TAPE_FILE_NAME);
    let mut tape = EventTape::load(&path).unwrap();
    tape.header.version = 4;
    tape.persist(&path).unwrap();
    let loaded = EventTape::load(&path).expect("format four remains inspectable");
    assert!(loaded.records.iter().any(|record| {
        matches!(&record.kind, TapeRecordKind::ToolCall { transcript_changes, .. } if transcript_changes == &[change.clone()])
    }));
    let result = start_branch(BranchPlan {
        tape: loaded,
        live_after_calls: 1,
        workspace_root: dir.path().to_path_buf(),
        state_root: dir.path().join("branch-state"),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    });
    let Err(error) = result else {
        panic!("old recording cannot claim lifecycle evidence")
    };
    assert!(error.contains("session lifecycle"), "{error}");
}

/// A branch entered on this thread for the life of the value.
struct TestBranch {
    guard: BranchGuard,
    _scope: crate::persistent_state::ScopedPersistentStateRoot,
    _state: tempfile::TempDir,
}

impl std::ops::Deref for TestBranch {
    type Target = BranchGuard;

    fn deref(&self) -> &BranchGuard {
        &self.guard
    }
}

fn branch(dir: &Path, k: u64, workspace: &Path) -> TestBranch {
    std::fs::create_dir_all(workspace).unwrap();
    let state = tempfile::tempdir().unwrap();
    let guard = start_branch(BranchPlan {
        tape: EventTape::load(&dir.join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: k,
        workspace_root: workspace.to_path_buf(),
        state_root: state.path().join("branch"),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    })
    .unwrap();
    let _scope = guard.scope().enter();
    TestBranch {
        guard,
        _scope,
        _state: state,
    }
}

/// A new state root for each branch, as `start_branch` requires.
fn fresh_branch_state(workspace: &Path) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    workspace.join(format!("branch-state-{n}"))
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
        ToolServe::Served(outcome, _, _) => assert_eq!(outcome, ok(serde_json::json!("done"))),
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
fn full_run_tape_redacts_prompt_llm_tool_transcript_and_cache_secrets() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD";
    let tape_dir = tmp.path().join(secret);
    std::fs::create_dir_all(&tape_dir).unwrap();
    let text = format!("Bearer {secret}");
    struct SecretEffectObserver;
    impl ToolEffectObserver for SecretEffectObserver {
        fn begin(&self, _root: &Path, _call_id: &str) {}
        fn end(&self, _root: &Path, _call_id: &str) -> EffectScan {
            EffectScan {
                changes: vec![(
                    "secret.txt".to_string(),
                    Some(b"Bearer ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD".to_vec()),
                )],
                incomplete: false,
            }
        }
    }
    let guard = start_recording(
        &tape_dir,
        RecordingLevel::Full,
        Some(TapeWorkspace {
            root: tape_dir.clone(),
            observer: Box::new(SecretEffectObserver),
            state_roots: Vec::new(),
        }),
    )
    .unwrap()
    .unwrap();

    let parent = crate::stdlib::template::PromptSourceSpan {
        template_line: 1,
        template_col: 1,
        output_start: 0,
        output_end: text.len(),
        kind: crate::stdlib::template::PromptSpanKind::Expr,
        bound_value: Some(text.clone()),
        parent_span: None,
        template_uri: "prompt://fixture".to_string(),
        template_revision: blake3::hash(text.as_bytes()).to_hex().to_string(),
    };
    let span = crate::stdlib::template::PromptSourceSpan {
        parent_span: Some(Box::new(parent.clone())),
        ..parent
    };
    record_prompt_render(&format!("prompt://{secret}"), &text, &[span]);
    record_llm_call(&request(&text), &result(&text, None));
    let mut recording = begin_tool_call("secret-tool", "write", &serde_json::json!({}), false)
        .expect("active tape recording");
    recording.store_changes = crate::store::decode_dispatch(
        &serde_json::to_vec(&serde_json::json!([{
            "store": "store.json", "operation": {"kind": "set", "entry": {"sensitive": text}}
        }]))
        .unwrap(),
    )
    .unwrap();
    recording.transcript_changes.push(TranscriptChange::Fields {
        session_id: "session-1".to_string(),
        fields: BTreeMap::from([(
            "messages".to_string(),
            crate::testbench::tape::FieldChange::Set(serde_json::json!(text)),
        )]),
    });
    recording
        .transcript_changes
        .push(TranscriptChange::Reminders {
            session_id: "session-1".to_string(),
            before_reminders: crate::testbench::tape::ReminderStateSnapshot::default(),
            after_reminders: crate::testbench::tape::ReminderStateSnapshot {
                revoked_ids: std::iter::once(text.clone()).collect(),
                expired_ids: Default::default(),
            },
        });
    finish_tool_call(recording, Ok(ok(serde_json::json!({"output": text}))));
    cache_lookup(secret, secret, || {
        Ok(Some(serde_json::json!({"result": text})))
    })
    .unwrap();
    let mut opaque = vec![0xff];
    opaque.extend_from_slice(format!("Bearer {secret}\n{}", "x".repeat(8192)).as_bytes());
    process_recorder().unwrap().payload_cas(opaque);
    assert_eq!(guard.error(), None);
    drop(guard);

    let tape_text = std::fs::read_to_string(tape_dir.join(TAPE_FILE_NAME)).unwrap();
    assert!(
        !tape_text.contains(secret),
        "secret leaked into tape: {tape_text}"
    );
    assert!(
        tape_text.contains("<redacted:"),
        "expected redaction in tape"
    );
    let cas_dir = tape_dir.join(format!("{TAPE_FILE_NAME}.cas"));
    for entry in std::fs::read_dir(cas_dir).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        let payload = String::from_utf8_lossy(&bytes);
        assert!(
            !payload.contains(secret),
            "secret leaked into CAS: {payload}"
        );
    }
    let tape = EventTape::load(&tape_dir.join(TAPE_FILE_NAME)).unwrap();
    assert!(tape
        .records
        .iter()
        .any(|record| matches!(record.kind, TapeRecordKind::CacheLookup { .. })));
    assert!(tape
        .records
        .iter()
        .any(|record| matches!(record.kind, TapeRecordKind::PayloadRedaction)));
    assert!(
        tape.records.iter().any(|record| match &record.kind {
            TapeRecordKind::ToolCall {
                transcript_changes, ..
            } => {
                transcript_changes.iter().any(|change| match change {
                    TranscriptChange::Reminders {
                        after_reminders, ..
                    } => {
                        after_reminders.revoked_ids.len() == 1
                            && after_reminders
                                .revoked_ids
                                .iter()
                                .all(|id| !id.contains(secret))
                    }
                    _ => false,
                })
            }
            _ => false,
        }),
        "the sanitized reminder state must remain recorded"
    );
    let result = start_branch(BranchPlan {
        tape,
        live_after_calls: 1,
        workspace_root: tmp.path().join("branch"),
        state_root: tmp.path().join("branch-state"),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    });
    assert!(result
        .err()
        .is_some_and(|error| error.contains("redacted payloads")));
}

#[cfg(unix)]
#[test]
fn branch_file_effects_do_not_follow_symlinked_paths() {
    use std::os::unix::fs::symlink;

    let branch = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("victim.txt");
    std::fs::write(&victim, b"outside sentinel").unwrap();

    symlink(outside.path(), branch.path().join("parent-link")).unwrap();
    assert!(apply_effect(
        branch.path(),
        "parent-link/victim.txt",
        Some(b"must not escape"),
    )
    .is_err());
    assert_eq!(std::fs::read(&victim).unwrap(), b"outside sentinel");
    assert!(apply_effect(branch.path(), "parent-link/victim.txt", None).is_err());
    assert_eq!(std::fs::read(&victim).unwrap(), b"outside sentinel");

    symlink(&victim, branch.path().join("leaf-link")).unwrap();
    apply_effect(branch.path(), "leaf-link", Some(b"branch file")).unwrap();
    assert_eq!(std::fs::read(&victim).unwrap(), b"outside sentinel");
    assert_eq!(
        std::fs::read(branch.path().join("leaf-link")).unwrap(),
        b"branch file"
    );

    symlink(&victim, branch.path().join("deleted-link")).unwrap();
    apply_effect(branch.path(), "deleted-link", None).unwrap();
    assert_eq!(std::fs::read(&victim).unwrap(), b"outside sentinel");
    assert!(!branch.path().join("deleted-link").exists());
}

#[test]
fn a_branch_requires_an_existing_workspace_copy() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    record_run(tmp.path());
    let missing = tmp.path().join("missing-workspace");
    let result = start_branch(BranchPlan {
        tape: EventTape::load(&tmp.path().join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: 1,
        workspace_root: missing.clone(),
        state_root: tmp.path().join("branch-state"),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    });
    assert!(result
        .err()
        .is_some_and(|error| error.contains("branch workspace must exist")));
    assert!(!missing.exists());
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
    // Read at run time: a packaged crate cannot include a sibling's source.
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../harn-llm/src/request.rs");
    let source = std::fs::read_to_string(path).expect("read the request payload source");
    let struct_body = source
        .split("pub struct LlmRequestPayload {")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("LlmRequestPayload is in request.rs");
    let mut skipped = Vec::new();
    let mut lines = struct_body.lines();
    // `skip_serializing_if` omits only a value it judges empty, so it stays in.
    let skips_serializing = |line: &str| {
        line.trim()
            .strip_prefix("#[serde(")
            .and_then(|args| args.strip_suffix(")]"))
            .is_some_and(|args| {
                args.split(',')
                    .map(str::trim)
                    .any(|arg| arg == "skip" || arg == "skip_serializing")
            })
    };
    assert!(skips_serializing("    #[serde(default, skip)]"));
    assert!(!skips_serializing(
        r#"#[serde(skip_serializing_if = "Option::is_none")]"#
    ));
    while let Some(line) = lines.next() {
        if !skips_serializing(line) {
            continue;
        }
        let field = lines
            .by_ref()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with("//"))
            .map(|line| {
                ["pub ", "pub(crate) "]
                    .iter()
                    .find_map(|vis| line.strip_prefix(vis))
                    .unwrap_or(line)
            })
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
        state_root: fresh_branch_state(tmp.path()),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
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
        state_root: fresh_branch_state(tmp.path()),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
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
fn an_unconsumed_prefix_cannot_start_live_inference() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    record_run(tmp.path());
    let skipped = branch(tmp.path(), 1, &tmp.path().join("branch"));
    serve_llm_call(&request("first")).unwrap().unwrap();
    let error = serve_llm_call(&request("second"))
        .expect("an incomplete prefix must not reach a provider")
        .unwrap_err();
    assert!(error.to_string().contains("`write` call t1 was never made"));
    assert_eq!(skipped.report().live_calls, 0);
    assert!(serve_llm_call(&request("third")).unwrap().is_err());
    assert_eq!(skipped.report().live_calls, 0);
}

#[test]
fn a_tool_with_nested_inference_is_refused_before_applying_its_effects() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let args = serde_json::json!({"path": "src/t1.txt"});
    {
        let _guard = record_full(tmp.path());
        record_llm_call(&request("first"), &result("calling", Some("t1")));
        let tool = begin_tool_call("t1", "write", &args, false).unwrap();
        record_llm_call(&request("nested"), &result("nested result", None));
        finish_tool_call(tool, Ok(ok(serde_json::json!("done"))));
    }
    let workspace = tmp.path().join("branch");
    let replay = branch(tmp.path(), 1, &workspace);
    serve_llm_call(&request("first")).unwrap().unwrap();
    assert!(matches!(
        serve_tool_call("t1", "write", &args),
        ToolServe::Diverged(_)
    ));
    assert!(replay
        .report()
        .divergence
        .unwrap()
        .detail
        .contains("nested inference"));
    assert!(!workspace.join("src/t1.txt").exists());
    assert!(serve_llm_call(&request("continuation")).unwrap().is_err());
    assert_eq!(replay.report().live_calls, 0);
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
fn a_prefix_refuses_direct_host_tools_before_the_bridge_runs() {
    use crate::stdlib::host::{host_call_ready, HostCallBridge, HostCallDispatchFuture};
    use crate::value::VmError;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingBridge(Arc<AtomicUsize>);
    impl HostCallBridge for CountingBridge {
        fn dispatch<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            _: &'a crate::value::DictMap,
        ) -> HostCallDispatchFuture<'a> {
            host_call_ready(Ok(None))
        }
        fn call_tool(&self, _: &str, _: &VmValue) -> Result<Option<VmValue>, VmError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Some(VmValue::Nil))
        }
    }
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let _bridge =
        crate::stdlib::host::install_host_call_bridge(Arc::new(CountingBridge(calls.clone())));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let invoke = || crate::stdlib::host::dispatch_host_tool_call("Write", &VmValue::Nil);
    {
        let _guard = record_full(tmp.path());
        rt.block_on(invoke()).unwrap();
        record_llm_call(&request("first"), &result("ok", None));
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "live host tool must actually fire"
    );
    let branch = branch(tmp.path(), 1, tmp.path());
    let outcome = rt.block_on(invoke());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "replayed prefix must never repeat the host mutation"
    );
    assert!(outcome.unwrap_err().to_string().contains("host_tool_call"));
    assert_eq!(branch.finish().divergence.unwrap().what, "refused effect");
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
    let path = tmp.path().join(crate::runtime_paths::STORE_FILE);
    std::fs::write(&path, r#"{"memo": "recorded"}"#).unwrap();
    {
        let _guard = record_full(tmp.path());
        let store = parking_lot::Mutex::new(StoreState::at_state_root(tmp.path()));
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
    // Built for the recording's root, as a host reusing its configuration would.
    let store = parking_lot::Mutex::new(StoreState::at_state_root(tmp.path()));
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
    // This store lives at the workspace root, so it lands under the branch's
    // copy of the workspace tree.
    let branch_root = crate::run_tape::current_branch().unwrap();
    assert!(
        std::fs::read_to_string(
            branch_root
                .state_root()
                .join("workspace")
                .join(crate::runtime_paths::STORE_FILE)
        )
        .unwrap()
        .contains("branch"),
        "the branch saved into its own root"
    );
}

#[test]
fn a_blocking_job_and_a_thread_resolve_writes_inside_the_branch() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    {
        let _guard = record_full(tmp.path());
        record_llm_call(&request("work"), &result("done", None));
    }
    let _branch = branch(tmp.path(), 1, tmp.path());
    let path = tmp.path().join("written-off-thread.json");
    let resolve = |path: PathBuf| move || crate::run_tape::durable_path(&path, "probe").unwrap();
    let expected = resolve(path.clone())();
    assert_ne!(expected, path, "the branch remaps the workspace");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let blocking = runtime
        .block_on(async { crate::task_spawn::spawn_blocking(resolve(path.clone())).await })
        .unwrap();
    assert_eq!(blocking, expected, "a blocking job keeps the branch");
    let threaded = crate::task_spawn::thread(resolve(path.clone()))
        .join()
        .unwrap();
    assert_eq!(threaded, expected, "a thread keeps the branch");

    // The raw spawn drops the branch: the write would reach the recording's file.
    let raw = crate::runtime_stack::spawn(resolve(path.clone()))
        .join()
        .unwrap();
    assert_eq!(raw, path);
}

#[test]
fn a_local_task_writes_branch_state_without_a_child_vm() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = tempfile::tempdir().unwrap();
    let recording_state = crate::runtime_paths::state_root(recorded.path());
    let original = recording_state.join(crate::runtime_paths::STORE_FILE);
    std::fs::create_dir_all(&recording_state).unwrap();
    std::fs::write(&original, r#"{"original":true}"#).unwrap();
    {
        let _guard = record_full(recorded.path());
        crate::store::StoreState::at_state_root(&recording_state)
            .set("original".to_string(), serde_json::json!(true))
            .unwrap();
        record_llm_call(&request("work"), &result("done", None));
    }
    let original_bytes = std::fs::read(&original).unwrap();
    let state = tempfile::tempdir().unwrap();
    let guard = start_branch(BranchPlan {
        tape: EventTape::load(&recorded.path().join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: 1,
        workspace_root: recorded.path().to_path_buf(),
        state_root: state.path().join("branch"),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    })
    .unwrap();
    let branch_scope = guard.scope();
    harn_clock::test_support::current_thread_runtime().block_on(
        harn_clock::test_support::with_local_tasks(async {
            let inherited = {
                let _inside = branch_scope.enter();
                crate::orchestration::AmbientExecutionScope::capture_inherited()
            };
            assert!(crate::run_tape::state_branch().is_none());
            crate::task_spawn::spawn_local_in(inherited, async move {
                tokio::task::yield_now().await;
                crate::store::StoreState::at_state_root(&recording_state)
                    .set("child".to_string(), serde_json::json!("written"))
            })
            .await
            .unwrap()
            .unwrap();
            assert!(crate::run_tape::state_branch().is_none());
        }),
    );
    assert_eq!(std::fs::read(&original).unwrap(), original_bytes);
    let branch_store: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            state
                .path()
                .join("branch")
                .join(crate::runtime_paths::STORE_FILE),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(branch_store["original"], true);
    assert_eq!(branch_store["child"], "written");
}

/// Work that outlives the host's guard, such as lifecycle cleanup finishing
/// late, serves nothing but still writes only into the branch.
#[test]
fn a_write_after_the_branch_ends_stays_out_of_the_recording() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    {
        let _guard = record_full(tmp.path());
        record_llm_call(&request("work"), &result("done", None));
    }
    let state = tempfile::tempdir().unwrap();
    let guard = start_branch(BranchPlan {
        tape: EventTape::load(&tmp.path().join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: 1,
        workspace_root: tmp.path().to_path_buf(),
        state_root: state.path().join("branch"),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    })
    .unwrap();
    let scope = guard.scope();
    let path = tmp.path().join("late.json");
    let during = {
        let _in = scope.enter();
        crate::run_tape::durable_path(&path, "probe").unwrap()
    };
    drop(guard);

    let _in = scope.enter();
    assert!(
        crate::run_tape::current_branch().is_none(),
        "an ended branch serves nothing"
    );
    assert_eq!(
        crate::run_tape::durable_path(&path, "probe").unwrap(),
        during,
        "a late write lands where the branch put it"
    );
    let outside = tempfile::tempdir().unwrap();
    assert!(
        crate::run_tape::durable_path(&outside.path().join("x"), "probe").is_err(),
        "a late write outside the branch is refused"
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let cleanup = runtime
        .block_on(crate::task_spawn::spawn_lifecycle_cleanup(async move {
            crate::run_tape::durable_path(&path, "probe").unwrap()
        }))
        .unwrap();
    assert_eq!(cleanup, during, "late cleanup keeps the branch");
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
        ToolServe::Served(..)
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

/// `harness.agent` methods that write state the prefix gate refuses, each for
/// a reason. Together with [`prefix::SESSION_ENTRY_METHODS`] this classifies
/// every state-writing agent method, so a new one fails the test below until
/// someone decides which side it is on.
const REFUSED_AGENT_STATE: &[(&str, &str)] = &[
    ("attach", "routes a live client"),
    ("cancel_in_flight_tool_call", "cancels a live tool call"),
    ("client_inject_prompt", "routes a live client"),
    ("daemon_resume", "controls a daemon worker"),
    ("daemon_spawn", "starts a daemon worker"),
    ("daemon_stop", "controls a daemon worker"),
    ("daemon_trigger", "controls a daemon worker"),
    ("detach", "routes a live client"),
    ("heartbeat", "routes a live client"),
    (
        "record_changed_path",
        "writes the durable session store directly",
    ),
    (
        "route_permission",
        "routes a permission prompt to a live client",
    ),
    (
        "self_review",
        "writes the durable review audit and trust graph",
    ),
    ("session_append", "writes the host's durable session store"),
    ("session_close", "writes the host's durable session store"),
    ("session_fork", "writes the host's durable session store"),
    ("session_open", "writes the host's durable session store"),
    (
        "session_store_append",
        "writes the durable session store directly",
    ),
    ("session_update", "writes the host's durable session store"),
    ("state_delete", "durable agent state"),
    ("state_handoff", "durable agent state"),
    ("state_write", "durable agent state"),
    ("takeover", "routes a live client"),
];

/// Every `harness.agent` method whose contract writes state is classified:
/// allowed as session-table state, allowed by a constant process-local
/// resource, or refused with a reason. Read-only methods need no entry.
/// The registry contract behind `harness.agent.<method>`.
fn agent_method_contract(method: &str) -> Option<&'static harn_builtin_meta::BuiltinContract> {
    crate::stdlib::capability_method_manifest_entry(harn_builtin_meta::CapabilityId::Agent, method)
        .map(|entry| &entry.contract)
        .or_else(|| crate::stdlib::builtin_policy_metadata(&format!("__cap_agent_{method}")).1)
}

#[test]
fn every_state_writing_agent_method_is_classified_for_the_prefix() {
    use harn_builtin_meta::{BuiltinExposure, CapabilityId, ResourceSelector};
    let mut unclassified = Vec::new();
    let mut no_contract = Vec::new();
    for method in harn_capability_contracts::declared_capability_method_names("agent") {
        let name = format!("harness.agent.{method}");
        let Some(contract) = agent_method_contract(method) else {
            no_contract.push(method);
            continue;
        };
        assert!(
            matches!(
                contract.exposure,
                BuiltinExposure::HarnessMethod { capability: CapabilityId::Agent, method: m } if m == method
            ),
            "`{name}` is not harness.agent.{method}"
        );
        let writes: Vec<_> = contract
            .effects
            .iter()
            .filter(|effect| {
                effect.kind == EffectKind::State
                    && !matches!(effect.access, EffectAccess::Read | EffectAccess::Observe)
            })
            .collect();
        if writes.is_empty() || writes.iter().all(|effect| prefix::process_local(effect)) {
            continue;
        }
        let allowed = prefix::SESSION_ENTRY_METHODS.contains(&method);
        let refused = REFUSED_AGENT_STATE.iter().any(|(name, _)| *name == method);
        assert!(
            !(allowed && refused),
            "`{method}` is both allowed and refused"
        );
        if allowed {
            for effect in &writes {
                assert!(
                    effect.resources.iter().all(|resource| matches!(
                        resource,
                        ResourceSelector::Argument(_)
                            | ResourceSelector::Constant("agent-sessions")
                    )),
                    "`{method}` writes state outside the session table: {effect:?}"
                );
            }
        } else if !refused {
            unclassified.push(method);
        }
    }
    assert!(
        unclassified.is_empty(),
        "classify these state-writing harness.agent methods in SESSION_ENTRY_METHODS or \
         REFUSED_AGENT_STATE: {unclassified:?}"
    );
    // A method with no registry contract has unknown effects, so the gate
    // refuses it: checked through the same lookup the dispatch makes.
    assert!(
        !no_contract.is_empty(),
        "the scan found no uncontracted method"
    );
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    {
        let _guard = record_full(tmp.path());
        record_llm_call(&request("one"), &result("a", None));
    }
    let _branch = branch(tmp.path(), 1, tmp.path());
    for method in &no_contract {
        let effects = crate::stdlib::capability_method_manifest_entry(CapabilityId::Agent, method)
            .map(|entry| entry.contract.effects);
        assert!(
            matches!(
                serve_read(Some((CapabilityId::Agent, method)), method, effects, &[]),
                Some(Err(_))
            ),
            "uncontracted `harness.agent.{method}` would run in a prefix"
        );
    }
    for method in prefix::SESSION_ENTRY_METHODS
        .iter()
        .chain(REFUSED_AGENT_STATE.iter().map(|(name, _)| name))
    {
        assert!(
            agent_method_contract(method).is_some(),
            "`{method}` is not a harness.agent method"
        );
    }
}

/// Named state a capability method writes that persists or leaves the
/// process, so the prefix gate refuses it.
const REFUSED_STATE_RESOURCES: &[(&str, &str)] = &[
    ("checkpoints", "writes checkpoints to disk"),
    ("llm-cache-stats", "lives with the persistent LLM cache"),
    ("llm-file-upload-cache", "uploads to the provider"),
    ("oauth-registration", "registers a client with the provider"),
    ("oauth-storage", "writes stored tokens"),
    ("package-snapshot", "writes the package snapshot to disk"),
    ("review-audit", "appends to the event log"),
    ("run-store", "writes the run store to disk"),
    ("trust-graph", "appends to the event log"),
    ("verification-profiles", "persists the profile record set"),
    ("vision-ocr-audit", "appends to the event log"),
];

#[test]
fn every_named_state_a_capability_method_writes_is_classified_for_the_prefix() {
    use harn_builtin_meta::{CapabilityId, ResourceSelector};
    let mut unclassified = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for capability in CapabilityId::ALL {
        let field = capability.field_name();
        for method in harn_capability_contracts::declared_capability_method_names(field) {
            let Some(contract) =
                crate::stdlib::capability_method_manifest_entry(*capability, method)
                    .map(|entry| &entry.contract)
                    .or_else(|| {
                        crate::stdlib::builtin_policy_metadata(&format!("__cap_{field}_{method}")).1
                    })
            else {
                continue;
            };
            if prefix::branch_owns_state(Some((*capability, method))) {
                continue;
            }
            for effect in contract.effects.iter().filter(|effect| {
                effect.kind == EffectKind::State
                    && !matches!(effect.access, EffectAccess::Read | EffectAccess::Observe)
            }) {
                for resource in effect.resources {
                    let ResourceSelector::Constant(resource) = resource else {
                        continue;
                    };
                    seen.insert(*resource);
                    let local = prefix::PROCESS_LOCAL_STATE.contains(resource);
                    let refused = REFUSED_STATE_RESOURCES.iter().any(|(r, _)| r == resource);
                    assert!(
                        !(local && refused),
                        "`{resource}` is both local and refused"
                    );
                    if !local && !refused {
                        unclassified.push(format!("{resource} ({field}.{method})"));
                    }
                }
            }
        }
    }
    assert!(
        unclassified.is_empty(),
        "classify this state in PROCESS_LOCAL_STATE or REFUSED_STATE_RESOURCES: {unclassified:?}"
    );
    assert!(
        seen.contains("egress-policy"),
        "the scan reached no capability contracts"
    );
}

/// Hosted providers never probe, so a branch passes through; a self-hosted
/// probe is not on the tape and is refused rather than sent.
#[test]
fn a_prefix_refuses_only_a_capability_probe_that_would_reach_the_network() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    {
        let _guard = record_full(tmp.path());
        record_llm_call(&request("one"), &result("a", None));
    }
    let _branch = branch(tmp.path(), 1, tmp.path());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        assert!(
            !crate::llm::capabilities::ensure_runtime_probe("openai", "gpt")
                .await
                .unwrap()
        );
        let refused = crate::llm::capabilities::ensure_runtime_probe("ollama", "qwen")
            .await
            .expect_err("a self-hosted probe is refused");
        assert!(
            format!("{refused:?}").contains("capability probe"),
            "{refused:?}"
        );
    });
}
