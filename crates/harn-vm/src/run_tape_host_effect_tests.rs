use super::*;

/// Code-index methods that write workspace files, so a prefix refuses them.
const REFUSED_CODE_INDEX_METHODS: &[&str] = &[
    "change_signature",
    "extract_function",
    "move_symbol",
    "rename_symbol",
];

/// A host that warms the code index before its first model call can branch:
/// index maintenance of the restored workspace runs in the prefix, while a
/// rebuild of another root, a source-writing refactor and a machine-global
/// host lease are refused.
#[test]
fn code_index_maintenance_of_the_restored_workspace_runs_in_a_prefix() {
    use harn_builtin_meta::CapabilityId;
    let contract = |capability: CapabilityId, method: &str| {
        let field = capability.field_name();
        crate::stdlib::capability_method_manifest_entry(capability, method)
            .map(|entry| entry.contract.effects)
            .or_else(|| {
                crate::stdlib::builtin_policy_metadata(&format!("__cap_{field}_{method}"))
                    .1
                    .map(|contract| contract.effects)
            })
    };
    let mut file_mutations = 0;
    let mut index_mutations = 0;
    for method in harn_capability_contracts::declared_capability_method_names("code_index") {
        let effects = contract(CapabilityId::CodeIndex, method).expect("index method contract");
        for effect in effects {
            if matches!(effect.access, EffectAccess::Read | EffectAccess::Observe) {
                continue;
            }
            if REFUSED_CODE_INDEX_METHODS.contains(&method) {
                assert_eq!(effect.kind, EffectKind::Fs, "{method} writes source files");
                file_mutations += 1;
            } else {
                assert_eq!(effect.kind, EffectKind::State, "{method} owns index state");
                index_mutations += 1;
            }
        }
    }
    assert_eq!(file_mutations, REFUSED_CODE_INDEX_METHODS.len());
    assert!(index_mutations >= 10);

    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    let elsewhere = tempfile::tempdir().unwrap();
    {
        let _guard = record_full(tmp.path());
        record_llm_call(&request("one"), &result("a", None));
    }
    let _branch = branch(tmp.path(), 1, &workspace);
    let call = |capability: CapabilityId, method: &str, args: serde_json::Value| {
        let args = [crate::schema::json_to_vm_value(&args)];
        serve_read(
            Some((capability, method)),
            method,
            contract(capability, method),
            &args,
        )
    };
    let root = |path: &Path| serde_json::json!({"root": path.display().to_string()});
    assert!(
        call(CapabilityId::CodeIndex, "rebuild", root(&workspace)).is_none(),
        "a rebuild of the restored workspace runs"
    );
    for method in [
        "reindex_file",
        "agent_register",
        "lock_try",
        "version_record",
    ] {
        assert!(
            call(CapabilityId::CodeIndex, method, serde_json::json!({})).is_none(),
            "`code_index.{method}` runs"
        );
    }
    let refused =
        |outcome: Option<Result<VmValue, crate::value::VmError>>| matches!(outcome, Some(Err(_)));
    assert!(
        refused(call(
            CapabilityId::CodeIndex,
            "rebuild",
            root(elsewhere.path())
        )),
        "a rebuild outside the restored workspace is refused"
    );
    assert!(refused(call(
        CapabilityId::CodeIndex,
        "rename_symbol",
        serde_json::json!({})
    )));
    assert!(refused(call(
        CapabilityId::HostLease,
        "acquire",
        serde_json::json!({})
    )));
}

/// A command served in the prefix never ran, so a later read of its output is
/// served from the recording; a recording that never taped the read diverges.
#[test]
fn command_output_reads_are_served_from_the_recording() {
    use harn_builtin_meta::CapabilityId;
    const OBSERVE: &[EffectSpec] = &[EffectSpec {
        kind: EffectKind::Process,
        access: EffectAccess::Observe,
        resources: &[],
    }];
    let owner = Some((CapabilityId::Tools, "read_command_output"));
    let args = [crate::schema::json_to_vm_value(
        &serde_json::json!({"handle_id": "h1"}),
    )];
    let output = crate::schema::json_to_vm_value(&serde_json::json!({"stdout": "ok\n"}));
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    {
        let _guard = record_full(tmp.path());
        record_llm_call(&request("one"), &result("ran", None));
        record_read(
            owner,
            "read_command_output",
            Some(OBSERVE),
            &args,
            &Ok(output),
        );
        record_llm_call(&request("two"), &result("done", None));
    }
    let replay = branch(tmp.path(), 2, tmp.path());
    serve_llm_call(&request("one")).unwrap().unwrap();
    let served = serve_read(owner, "read_command_output", Some(OBSERVE), &args)
        .unwrap()
        .unwrap();
    assert!(served.display().contains("ok"), "{}", served.display());
    let other = [crate::schema::json_to_vm_value(
        &serde_json::json!({"handle_id": "h2"}),
    )];
    assert!(
        matches!(
            serve_read(owner, "read_command_output", Some(OBSERVE), &other),
            Some(Err(_))
        ),
        "a read the recording never made diverges"
    );
    drop(replay);
}

#[test]
fn startup_preflight_lists_every_supplied_refusal_without_executing_calls() {
    use harn_builtin_meta::CapabilityId;
    let temporary = tempfile::tempdir().unwrap();
    let root = [crate::schema::json_to_vm_value(&serde_json::json!({
        "root": temporary.path()
    }))];
    let marker = temporary.path().join("never-executed");
    let command = [crate::schema::json_to_vm_value(&serde_json::json!({
        "argv": ["touch", marker], "cwd": temporary.path()
    }))];
    let calls = [
        PrefixEffectCall {
            capability: Some(CapabilityId::CodeIndex),
            name: "rebuild",
            args: &root,
        },
        PrefixEffectCall {
            capability: Some(CapabilityId::CodeIndex),
            name: "agent_register",
            args: &[],
        },
        PrefixEffectCall {
            capability: Some(CapabilityId::CodeIndex),
            name: "rename_symbol",
            args: &[],
        },
        PrefixEffectCall {
            capability: Some(CapabilityId::HostLease),
            name: "acquire",
            args: &[],
        },
        PrefixEffectCall {
            capability: Some(CapabilityId::HostLease),
            name: "release",
            args: &[],
        },
        PrefixEffectCall {
            capability: None,
            name: "unknown_startup_builtin",
            args: &[],
        },
        PrefixEffectCall {
            capability: Some(CapabilityId::Fs),
            name: "cwd",
            args: &[],
        },
        PrefixEffectCall {
            capability: Some(CapabilityId::Tools),
            name: "run_command",
            args: &command,
        },
    ];
    let report = preflight_prefix_calls(temporary.path(), &calls);
    assert_eq!(report.supplied_calls, 8);
    assert_eq!(report.decisions.len(), 8);
    assert_eq!(report.refused_calls, 4);
    assert_eq!(report.decisions[0].action, PrefixEffectAction::Run);
    assert_eq!(report.decisions[1].action, PrefixEffectAction::Run);
    let refused: Vec<_> = report
        .decisions
        .iter()
        .filter(|decision| decision.action == PrefixEffectAction::Refused)
        .collect();
    assert_eq!(
        refused
            .iter()
            .map(|decision| decision.name.as_str())
            .collect::<Vec<_>>(),
        [
            "rename_symbol",
            "acquire",
            "release",
            "unknown_startup_builtin"
        ]
    );
    assert!(refused[0].reason.as_ref().unwrap().contains("Fs Mutate"));
    assert!(refused[1]
        .reason
        .as_ref()
        .unwrap()
        .contains("outside the replayed prefix"));
    assert!(refused[3]
        .reason
        .as_ref()
        .unwrap()
        .contains("no effect contract"));
    assert_eq!(report.decisions[6].action, PrefixEffectAction::RecordedRead);
    assert_eq!(
        report.decisions[7].action,
        PrefixEffectAction::RecordedCommand
    );
    assert!(!marker.exists());
    let empty = preflight_prefix_calls(temporary.path(), &[]);
    assert_eq!(
        empty.supplied_calls, 0,
        "an empty inventory must remain visibly empty"
    );
    assert!(empty.decisions.is_empty());
}
