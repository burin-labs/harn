use super::*;
use harn_vm::run_tape::{start_branch, BranchPlan, BranchScope};
use std::sync::atomic::{AtomicUsize, Ordering};

fn indexed_run(
    workspace: &Path,
    index_root: &Path,
    skip_rebuild: bool,
    branch: Option<BranchScope>,
) -> (Result<String, String>, usize) {
    let rebuilt = Arc::new(AtomicUsize::new(0));
    let measured = rebuilt.clone();
    let output = harn_vm::on_vm_stack(move || {
        harn_vm::reset_thread_local_state();
        let mut registry =
            HostlibRegistry::new().with(crate::code_index::CodeIndexCapability::new());
        let entry = registry
            .builtins
            .builtins
            .iter_mut()
            .find(|entry| entry.name == "hostlib_code_index_rebuild")
            .unwrap();
        let original = entry.handler.clone();
        entry.handler = Arc::new(move |args| {
            rebuilt.fetch_add(1, Ordering::SeqCst);
            original(args)
        });
        let path = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/fixtures/index_replay_warmup.harn"
        ));
        let chunk = harn_vm::compile_source_at_path(
            include_str!("fixtures/index_replay_warmup.harn"),
            path,
            None,
        )
        .unwrap();
        let mut vm = harn_vm::new_vm();
        harn_runtime::register_vm_stdlib(&mut vm);
        registry.register_into_vm(&mut vm);
        vm.set_harness(harn_vm::Harness::real());
        vm.set_global(
            "argv",
            harn_vm::VmValue::list(vec![
                harn_vm::VmValue::string(index_root.display().to_string()),
                harn_vm::VmValue::string(if skip_rebuild { "skip" } else { "rebuild" }),
            ]),
        );
        let policy = harn_vm::orchestration::CapabilityPolicy {
            workspace_roots: vec![workspace.display().to_string()],
            sandbox_profile: harn_vm::orchestration::SandboxProfile::WorkspacePaths,
            ..Default::default()
        };
        harn_clock::test_support::current_thread_runtime().block_on(
            harn_clock::test_support::with_local_tasks(async {
                let body = async {
                    harn_vm::orchestration::scope_execution_policy(policy, vm.execute(&chunk))
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok(vm.output().to_string())
                };
                match branch {
                    Some(branch) => branch.run(Box::pin(body)).await,
                    None => Box::pin(body).await,
                }
            }),
        )
    });
    (output, measured.load(Ordering::SeqCst))
}

#[test]
fn a_fresh_index_rebuild_and_query_reproduce_the_recorded_prefix() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let restored = temporary.path().join("pool/arm/workspace");
    let outside = temporary.path().join("outside");
    for root in [&workspace, &restored, &outside] {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(
            root.join("indexed.rs"),
            "pub const replay_index_sentinel: u32 = 1;\n",
        )
        .unwrap();
    }
    let tapes = temporary.path().join("tape");
    let recording = start_recording(
        &tapes,
        RecordingLevel::Full,
        Some(TapeWorkspace {
            root: workspace.clone(),
            observer: Box::new(NoChanges),
            state_roots: vec![],
        }),
    )
    .unwrap()
    .unwrap();
    let (recorded, rebuilt) = indexed_run(&workspace, &workspace, false, None);
    let recorded = recorded.expect("recording rebuilds and queries the real code index");
    assert_eq!(rebuilt, 1);
    assert_eq!(recording.error(), None);
    drop(recording);
    let tape = EventTape::load(&tapes.join(TAPE_FILE_NAME)).unwrap();
    assert_eq!(
        tape.records
            .iter()
            .filter(|record| matches!(record.kind, TapeRecordKind::LlmCall { .. }))
            .count(),
        1
    );

    let plan = |name: &str| BranchPlan {
        tape: tape.clone(),
        live_after_calls: 1,
        workspace: harn_vm::run_tape::ReplayWorkspaceLocation::from_storage_root(
            restored.parent().unwrap(),
        )
        .unwrap(),
        state_root: temporary.path().join(name),
        host_state_roots: vec![],
        prompt_edits: vec![],
    };
    let replay = start_branch(plan("branch-state")).unwrap();
    let (branched, rebuilt) = indexed_run(&restored, &workspace, false, Some(replay.scope()));
    assert_eq!(branched.unwrap(), recorded);
    assert_eq!(
        rebuilt, 1,
        "a fresh capability must actually rebuild in the prefix"
    );
    let report = replay.finish();
    assert_eq!(report.served_calls, 1);
    assert_eq!(report.live_calls, 0);
    assert_eq!(report.divergence, None);
    drop(replay);

    let (unwarmed, rebuilt) = indexed_run(&workspace, &workspace, true, None);
    assert!(
        unwarmed.is_err(),
        "a query without the rebuild must not see another capability's index"
    );
    assert_eq!(rebuilt, 0);
    let refused = start_branch(plan("refused-state")).unwrap();
    let (outside_result, rebuilt) = indexed_run(&restored, &outside, false, Some(refused.scope()));
    assert!(outside_result.is_err());
    assert_eq!(
        rebuilt, 0,
        "the refusal must happen before the actual index handler"
    );
    let report = refused.finish();
    assert_eq!(report.served_calls, 0);
    assert!(report
        .divergence
        .unwrap()
        .detail
        .contains("outside the restored workspace"));
}
