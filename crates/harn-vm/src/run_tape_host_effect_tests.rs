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
    let mut unclassified = Vec::new();
    for method in harn_capability_contracts::declared_capability_method_names("code_index") {
        let writes = contract(CapabilityId::CodeIndex, method).is_some_and(|effects| {
            effects
                .iter()
                .any(|effect| !matches!(effect.access, EffectAccess::Read | EffectAccess::Observe))
        });
        let admitted = prefix::WORKSPACE_INDEX_METHODS.contains(&method);
        let refused = REFUSED_CODE_INDEX_METHODS.contains(&method);
        assert!(
            !(admitted && refused),
            "`{method}` is both admitted and refused"
        );
        if writes && !admitted && !refused {
            unclassified.push(method);
        }
    }
    assert!(
        unclassified.is_empty(),
        "classify these code_index methods in WORKSPACE_INDEX_METHODS or \
         REFUSED_CODE_INDEX_METHODS: {unclassified:?}"
    );

    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
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
        refused(call(CapabilityId::CodeIndex, "rebuild", root(&elsewhere))),
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
