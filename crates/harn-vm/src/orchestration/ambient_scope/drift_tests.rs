use std::collections::BTreeSet;

use super::{AmbientScoping, AMBIENT_THREAD_LOCAL_CATALOG, AUDITED_LATENT_CAPABILITIES};

/// Walk the crate source and require every ambient-shape thread-local to have
/// an explicit captured or uncaptured decision in the owning catalog.
#[test]
fn every_ambient_shape_thread_local_is_cataloged() {
    fn is_ambient_shape(name: &str) -> bool {
        name == "VM_SOURCE_DIR"
            || name == "CURRENT_HOST_BRIDGE"
            || name.ends_with("_STACK")
            || name.ends_with("_DEPTH")
            || name.ends_with("_CONTEXT")
            || name.ends_with("_SESSION")
            || name.ends_with("_CTX")
    }

    fn collect(dir: &std::path::Path, shaped: &mut BTreeSet<String>, all: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).expect("read_dir src") {
            let path = entry.expect("dir entry").path();
            // Test-support thread-locals do not affect production task scope.
            // Match the component, not the absolute path: a worktree name can
            // itself end in `-tests`.
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name == "tests" || name == "test_util" || name.ends_with("_tests")
                })
            {
                continue;
            }
            if path.is_dir() {
                collect(&path, shaped, all);
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                let content = std::fs::read_to_string(&path).expect("read src file");
                let mut pending_static: Option<String> = None;
                for line in content.lines() {
                    // Thread-locals are the only `static _: RefCell<_>`
                    // declarations: a bare static RefCell is not Sync.
                    let Some(index) = line.find("static ") else {
                        if line.contains("RefCell") {
                            if let Some(name) = pending_static.take() {
                                if is_ambient_shape(&name) {
                                    shaped.insert(name.clone());
                                }
                                all.insert(name);
                            }
                        }
                        continue;
                    };
                    #[expect(
                        clippy::string_slice,
                        reason = "index is a find offset plus an ASCII literal length"
                    )]
                    let after = &line[index + "static ".len()..];
                    let name: String = after
                        .chars()
                        .take_while(|character| {
                            character.is_ascii_alphanumeric() || *character == '_'
                        })
                        .collect();
                    if name.is_empty() {
                        continue;
                    }
                    if line.contains("RefCell") {
                        if is_ambient_shape(&name) {
                            shaped.insert(name.clone());
                        }
                        all.insert(name);
                    } else {
                        pending_static = Some(name);
                    }
                }
            }
        }
    }

    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    // `shaped` drives the forcing function: a new ambient-shape thread-local
    // must be classified. `all` drives the staleness check, so a catalog entry
    // for a thread-local whose NAME is off-shape (a captured registry handle,
    // say) is still caught when it is renamed or deleted.
    let mut shaped = BTreeSet::new();
    let mut all = BTreeSet::new();
    collect(&src, &mut shaped, &mut all);

    let cataloged: BTreeSet<String> = AMBIENT_THREAD_LOCAL_CATALOG
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect();
    let missing: Vec<_> = shaped.difference(&cataloged).cloned().collect();
    assert!(
        missing.is_empty(),
        "new ambient-shape thread-local(s) not classified in \
         AMBIENT_THREAD_LOCAL_CATALOG (orchestration/ambient_scope.rs): {missing:?}. Decide \
         whether each must be Captured into AmbientExecutionScope or is safely Uncaptured."
    );

    let stale: Vec<_> = cataloged.difference(&all).cloned().collect();
    assert!(
        stale.is_empty(),
        "AMBIENT_THREAD_LOCAL_CATALOG names thread-local(s) no longer in src: {stale:?}"
    );
}

/// The catalog's `Captured` set must mirror the scope's fields and swaps.
#[test]
fn captured_catalog_matches_scope_fields() {
    let captured: BTreeSet<&str> = AMBIENT_THREAD_LOCAL_CATALOG
        .iter()
        .filter(|(_, scoping)| matches!(scoping, AmbientScoping::Captured))
        .map(|(name, _)| *name)
        .collect();
    let expected = BTreeSet::from([
        "EXECUTION_POLICY_STACK",
        "EXECUTION_APPROVAL_POLICY_STACK",
        "OPERATOR_APPROVAL_GRANT_STACK",
        "COMMAND_POLICY_STACK",
        "DYNAMIC_PERMISSION_STACK",
        "RUNTIME_CONTEXT_OVERLAY_STACK",
        "AUTONOMY_POLICY_STACK",
        "PERSONA_STACK",
        "STEP_STACK",
        "ACTIVE_CONTEXT_SUSPENSION_STACK",
        "LLM_RENDER_STACK",
        "ACTIVE_HARN_CONNECTOR_CTX",
        "TRUSTED_BRIDGE_CALL_DEPTH",
        "COMMAND_POLICY_HOOK_DEPTH",
        "TOOL_PRECHECK_STACK",
        "TOOL_PRECHECK_DEPTH",
        "VM_EXECUTION_CONTEXT",
        "VM_SOURCE_DIR",
        "CURRENT_MUTATION_SESSION",
        "SESSION_ENVIRONMENT_CONTEXT",
        "PROCESS_ADMISSION_CONTEXT",
        "CURRENT_HOST_BRIDGE",
        "CURRENT_SESSION_STACK",
        "LLM_CONFIG_OVERRIDES_CONTEXT",
        "LLM_RUNTIME_PROVIDER_ENDPOINTS_CONTEXT",
        "LLM_CAPABILITY_OVERRIDES_CONTEXT",
        "LLM_MOCK_CONTEXT",
        "EGRESS_POLICY_CONTEXT",
        "ACTIVE_EXECUTION_SCOPE_STACK",
        "RUN_EVENT_SINK_CONTEXT",
        "TRANSCRIPT_DIR_STACK",
        "SUBTASK_PLACEMENT_CONTEXT",
        "SECURITY_POLICY_STACK",
        "REQUIRE_EXPLICIT_EGRESS_POLICY_DEPTH",
        "REQUIRE_SSRF_GUARD_DEPTH",
        "CUSTOM_PATTERNS",
        "ACTIVE_EVENT_LOG",
        "MCP_CALL_BUDGET",
        "PG_QUERY_BUDGET",
        "ACTIVE_TOOL_CALL_CANCELLATION_REGISTRY",
    ]);
    assert_eq!(
        captured, expected,
        "the catalog's Captured set diverged from AmbientExecutionScope's swapped fields"
    );
}

/// Audited latent capability/identity thread-locals remain explicit until a
/// future read-across-await requires task-local capture.
#[test]
fn audited_latent_capabilities_are_cataloged() {
    for latent in AUDITED_LATENT_CAPABILITIES {
        let found = AMBIENT_THREAD_LOCAL_CATALOG
            .iter()
            .find(|(name, _)| name == latent);
        let Some((_, scoping)) = found else {
            panic!("{latent} missing from AMBIENT_THREAD_LOCAL_CATALOG");
        };
        match scoping {
            AmbientScoping::Uncaptured(reason) => assert!(
                reason.contains("[latent-capability]"),
                "{latent} must keep its [latent-capability] reason tag"
            ),
            AmbientScoping::Captured => panic!(
                "{latent} is now Captured; wire it fully and drop it from \
                 AUDITED_LATENT_CAPABILITIES"
            ),
        }
    }
}
