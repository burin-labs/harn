//! Integration tests asserting that every module's registration surface
//! compiles, that unimplemented methods route through `HostlibError` rather
//! than panicking, and that every registered builtin has a matching schema.
//!
//! These tests are the contract implementation work must keep green:
//! when a module moves beyond scaffolding, the only change here should be
//! that a routed `Unimplemented` becomes a real return value — never a
//! removed builtin.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

#[cfg(feature = "terminal-session")]
use harn_hostlib::terminal_session::TerminalSessionCapability;
use harn_hostlib::{
    ast::AstCapability, code_index::CodeIndexCapability, embed::EmbedCapability, fs::FsCapability,
    fs_snapshot::FsSnapshotCapability, fs_watch::FsWatchCapability,
    host_lease_capability::HostLeaseCapability, scanner::ScannerCapability, schemas,
    secret_store::SecretStoreCapability, tools::permissions, tools::ToolsCapability,
    BuiltinRegistry, HostLeasePriorityClass, HostLeaseRequest, HostLeaseResourceClass,
    HostLeaseStore, HostlibCapability, HostlibError, HostlibRegistry, HOST_LEASE_ROOT_ENV,
};
use harn_lexer::Lexer;
use harn_parser::Parser;
use harn_vm::{register_vm_stdlib, Compiler, Vm, VmError, VmValue};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

static HOST_LEASE_ENV_LOCK: Mutex<()> = Mutex::new(());

/// Restores the process-global lease-root override after one hermetic VM test.
///
/// The host capability resolves its store per invocation, so the test needs to
/// exercise the real environment-owned boundary rather than a test-only
/// injected store. The lock makes that mutation safe under libtest's parallel
/// runner.
struct HostLeaseRootGuard {
    prior: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl HostLeaseRootGuard {
    fn set(root: &Path) -> Self {
        let lock = HOST_LEASE_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prior = std::env::var_os(HOST_LEASE_ROOT_ENV);
        // SAFETY: the process-global mutex above serializes every mutation in
        // this test target and the previous value is restored in Drop.
        unsafe {
            std::env::set_var(HOST_LEASE_ROOT_ENV, root);
        }
        Self { prior, _lock: lock }
    }
}

impl Drop for HostLeaseRootGuard {
    fn drop(&mut self) {
        // SAFETY: `self._lock` is held for the guard's full lifetime, including
        // this restoration, so no sibling test in this target can observe a
        // torn environment value.
        unsafe {
            match &self.prior {
                Some(value) => std::env::set_var(HOST_LEASE_ROOT_ENV, value),
                None => std::env::remove_var(HOST_LEASE_ROOT_ENV),
            }
        }
    }
}

fn collect_into_registry<C: HostlibCapability>(cap: C) -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    cap.register_builtins(&mut registry);
    registry
}

fn execute_harn(source: &str) -> Result<VmValue, VmError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().expect("tokenize");
                let mut parser = Parser::new(tokens);
                let program = parser.parse().expect("parse");
                let chunk = Compiler::new().compile(&program).expect("compile");

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                let _ = harn_hostlib::install_default(&mut vm);
                vm.execute(&chunk).await
            })
            .await
    })
}

fn sha256_label(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn harn_string_literal(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

// Only the unix-gated terminal-session live test calls this; carry the same
// cfg so Windows and feature-lean builds do not trip dead_code.
#[cfg(all(unix, feature = "terminal-session"))]
fn assert_response_schema(module: &str, method: &str, value: &VmValue) {
    let schema = schemas::lookup(module, method, schemas::SchemaKind::Response)
        .unwrap_or_else(|| panic!("missing response schema for {module}.{method}"));
    let schema: serde_json::Value = serde_json::from_str(schema).expect("schema JSON");
    let schema = harn_vm::json_to_vm_value(&schema);
    let schema = harn_vm::schema::canonicalize_json_schema(&schema)
        .unwrap_or_else(|error| panic!("invalid response schema for {module}.{method}: {error}"));
    harn_vm::schema::validate_value_against_canonical_schema(value, &schema, true)
        .unwrap_or_else(|error| panic!("invalid {module}.{method} response: {error}"));
}

#[test]
fn ast_capability_registers_documented_methods() {
    let registry = collect_into_registry(AstCapability);
    let names: Vec<_> = registry.iter().map(|b| b.name).collect();
    assert_eq!(
        names,
        vec![
            "hostlib_ast_parse_file",
            "hostlib_ast_symbols",
            "hostlib_ast_outline",
            "hostlib_ast_parse_errors",
            "hostlib_ast_undefined_names",
            "hostlib_ast_function_body",
            "hostlib_ast_function_bodies",
            "hostlib_ast_extract_imports",
            "hostlib_ast_symbol_extract",
            "hostlib_ast_symbol_delete",
            "hostlib_ast_symbol_replace",
            "hostlib_ast_bracket_balance",
            "hostlib_ast_apply_node",
            "hostlib_ast_insert_at_anchor",
            "hostlib_ast_batch_apply",
            "hostlib_ast_dry_run",
            "hostlib_ast_search",
            "hostlib_ast_structural_diff",
            "hostlib_ast_capabilities",
        ]
    );
    // Each AST builtin must reject empty input with a structured
    // `MissingParameter`. The required field differs per method:
    // file-based builtins want `path`; analysis builtins (#773) accept
    // either `content` or `path`; the source-mutation builtins (#774/#775)
    // take `source`; function_body takes `function_name`; function_bodies
    // takes `names`.
    let expected_missing: &[(&str, &str)] = &[
        ("hostlib_ast_parse_file", "path"),
        ("hostlib_ast_symbols", "path"),
        ("hostlib_ast_outline", "path"),
        ("hostlib_ast_parse_errors", "content_or_path"),
        ("hostlib_ast_undefined_names", "content_or_path"),
        ("hostlib_ast_function_body", "function_name"),
        ("hostlib_ast_function_bodies", "names"),
        ("hostlib_ast_extract_imports", "source"),
        ("hostlib_ast_symbol_extract", "source"),
        ("hostlib_ast_symbol_delete", "source"),
        ("hostlib_ast_symbol_replace", "source"),
        ("hostlib_ast_bracket_balance", "source"),
        ("hostlib_ast_apply_node", "path"),
        ("hostlib_ast_insert_at_anchor", "path"),
        ("hostlib_ast_batch_apply", "paths"),
        ("hostlib_ast_dry_run", "plan"),
        ("hostlib_ast_search", "query"),
        ("hostlib_ast_structural_diff", "path_a"),
    ];
    // `apply_node` / `insert_at_anchor` write edited source to disk and are
    // gated on the deterministic-tools feature (#2548); enable it so the
    // handlers reach their parameter validation rather than the gate.
    permissions::enable_for_test();
    for (name, expected_param) in expected_missing {
        let entry = registry.find(name).expect("registered");
        let err = (entry.handler)(&[]).expect_err("must reject empty args");
        match err {
            HostlibError::MissingParameter { builtin, param } => {
                assert_eq!(builtin, *name);
                assert_eq!(param, *expected_param);
            }
            other => panic!("expected MissingParameter for {name}, got {other:?}"),
        }
    }
}

#[test]
fn code_index_capability_registers_documented_methods() {
    let registry = collect_into_registry(CodeIndexCapability::new());
    let names: Vec<_> = registry.iter().map(|b| b.name).collect();
    assert_eq!(
        names,
        vec![
            // Workspace queries (the original 5).
            "hostlib_code_index_query",
            "hostlib_code_index_rebuild",
            "hostlib_code_index_stats",
            "hostlib_code_index_imports_for",
            "hostlib_code_index_importers_of",
            // Additive read-only secondary roots (#2403 follow-up).
            "hostlib_code_index_add_readonly_roots",
            // File table accessors (#776).
            "hostlib_code_index_path_to_id",
            "hostlib_code_index_id_to_path",
            "hostlib_code_index_file_ids",
            "hostlib_code_index_file_meta",
            "hostlib_code_index_file_hash",
            "hostlib_code_index_file_hash_snapshot",
            // Cached reads (#776).
            "hostlib_code_index_read_range",
            "hostlib_code_index_reindex_file",
            "hostlib_code_index_trigram_query",
            "hostlib_code_index_extract_trigrams",
            "hostlib_code_index_word_get",
            "hostlib_code_index_deps_get",
            "hostlib_code_index_outline_get",
            // Change log (#776).
            "hostlib_code_index_current_seq",
            "hostlib_code_index_changes_since",
            "hostlib_code_index_version_record",
            // Agent registry + locks (#776).
            "hostlib_code_index_agent_register",
            "hostlib_code_index_agent_heartbeat",
            "hostlib_code_index_agent_unregister",
            "hostlib_code_index_lock_try",
            "hostlib_code_index_lock_release",
            "hostlib_code_index_status",
            "hostlib_code_index_current_agent_id",
            // Typed symbol graph + Cypher (#2434).
            "hostlib_code_index_cypher",
            "hostlib_code_index_repo_map",
            "hostlib_code_index_branch_overlay",
            "hostlib_code_index_freshness",
            // Cross-file safe rename (#2508).
            "hostlib_code_index_rename_symbol",
        ]
    );
    // Without a populated workspace, code-index read methods return empty
    // payloads rather than panicking. Assert that contract here so any
    // regression to `unimplemented!()` fails loudly.
    let stats = registry
        .find("hostlib_code_index_stats")
        .expect("registered");
    let value = (stats.handler)(&[]).expect("stats works on an empty index");
    match value {
        harn_vm::VmValue::Dict(_) => {}
        other => panic!("expected dict response from stats, got {other:?}"),
    }
}

#[test]
fn scanner_capability_registers_documented_methods() {
    let registry = collect_into_registry(ScannerCapability);
    let names: Vec<_> = registry.iter().map(|b| b.name).collect();
    assert_eq!(
        names,
        vec![
            "hostlib_scanner_scan_project",
            "hostlib_scanner_scan_incremental"
        ]
    );
    // Implemented scanner methods should refuse an empty payload with
    // `MissingParameter` rather than routing through `Unimplemented`.
    // The full scanner contract is exercised end-to-end in
    // `tests/scanner_e2e.rs`.
    for name in &[
        "hostlib_scanner_scan_project",
        "hostlib_scanner_scan_incremental",
    ] {
        let entry = registry.find(name).expect("registered");
        let err = (entry.handler)(&[]).expect_err("must reject empty args");
        assert!(
            !matches!(err, HostlibError::Unimplemented { .. }),
            "scanner method {name} should be implemented, got {err:?}"
        );
    }
}

#[test]
fn fs_capability_registers_documented_methods() {
    let registry = collect_into_registry(FsCapability);
    let names: Vec<_> = registry.iter().map(|b| b.name).collect();
    assert_eq!(
        names,
        vec![
            "hostlib_fs_set_mode",
            "hostlib_fs_staged_status",
            "hostlib_fs_commit_staged",
            "hostlib_fs_discard_staged",
            "hostlib_fs_safe_text_patch",
            "hostlib_fs_read_text",
            "hostlib_fs_emit_safe_text_patch_result",
        ]
    );
    let expected_missing: &[(&str, &str)] = &[
        ("hostlib_fs_set_mode", "session_id"),
        ("hostlib_fs_staged_status", "session_id"),
        ("hostlib_fs_commit_staged", "session_id"),
        ("hostlib_fs_discard_staged", "session_id"),
        ("hostlib_fs_safe_text_patch", "path"),
        ("hostlib_fs_read_text", "path"),
        ("hostlib_fs_emit_safe_text_patch_result", "path"),
    ];
    // `safe_text_patch` / `read_text` touch arbitrary host paths and are
    // gated on the deterministic-tools feature (#2548); enable it so the
    // handlers reach their parameter validation rather than the gate.
    permissions::enable_for_test();
    for (name, expected_param) in expected_missing {
        let entry = registry.find(name).expect("registered");
        let err = (entry.handler)(&[]).expect_err("must reject empty args");
        match err {
            HostlibError::MissingParameter { builtin, param } => {
                assert_eq!(builtin, *name);
                assert_eq!(param, *expected_param);
            }
            other => panic!("expected MissingParameter for {name}, got {other:?}"),
        }
    }
}

/// Every hostlib builtin that reads or writes arbitrary host paths must
/// refuse to run before `hostlib_enable("tools:deterministic")`, matching
/// the `tools::*` file I/O gate (#2548). This guards against the asymmetry
/// where the std/edit helpers could mutate files a sandboxed script was
/// denied via the `tools` surface.
#[test]
fn fs_and_ast_edit_primitives_require_deterministic_gate() {
    let mut registry = collect_into_registry(FsCapability);
    AstCapability.register_builtins(&mut registry);
    permissions::reset();
    for name in [
        "hostlib_fs_safe_text_patch",
        "hostlib_fs_read_text",
        "hostlib_ast_apply_node",
        "hostlib_ast_insert_at_anchor",
        "hostlib_ast_batch_apply",
    ] {
        let entry = registry.find(name).expect("registered");
        let err = (entry.handler)(&[]).expect_err("gated before enable");
        match err {
            HostlibError::Backend { builtin, message } => {
                assert_eq!(builtin, name);
                assert!(
                    message.contains("hostlib_enable"),
                    "gating error must point users at hostlib_enable: {message}"
                );
            }
            other => panic!("expected Backend gate error for {name}, got {other:?}"),
        }
    }
    // Telemetry routing cannot mutate files, so it stays un-gated: an empty
    // payload must surface parameter validation, not the feature gate.
    let entry = registry
        .find("hostlib_fs_emit_safe_text_patch_result")
        .expect("registered");
    match (entry.handler)(&[]).expect_err("must reject empty args") {
        HostlibError::MissingParameter { builtin, .. } => {
            assert_eq!(builtin, "hostlib_fs_emit_safe_text_patch_result");
        }
        other => panic!("emit result must stay un-gated, got {other:?}"),
    }
}

#[test]
fn fs_snapshot_capability_registers_documented_methods() {
    let registry = collect_into_registry(FsSnapshotCapability);
    let names: Vec<_> = registry.iter().map(|b| b.name).collect();
    assert_eq!(
        names,
        vec![
            "hostlib_fs_snapshot",
            "hostlib_fs_restore",
            "hostlib_fs_list_snapshots",
            "hostlib_fs_drop_snapshot",
        ]
    );
    let expected_missing: &[(&str, &str)] = &[
        ("hostlib_fs_snapshot", "session_id"),
        ("hostlib_fs_restore", "session_id"),
        ("hostlib_fs_list_snapshots", "session_id"),
        ("hostlib_fs_drop_snapshot", "session_id"),
    ];
    for (name, expected_param) in expected_missing {
        let entry = registry.find(name).expect("registered");
        let err = (entry.handler)(&[]).expect_err("must reject empty args");
        match err {
            HostlibError::MissingParameter { builtin, param } => {
                assert_eq!(builtin, *name);
                assert_eq!(param, *expected_param);
            }
            other => panic!("expected MissingParameter for {name}, got {other:?}"),
        }
    }
}

#[test]
fn fs_watch_capability_registers_documented_methods() {
    let registry = collect_into_registry(FsWatchCapability);
    let names: Vec<_> = registry.iter().map(|b| b.name).collect();
    assert_eq!(
        names,
        vec!["hostlib_fs_watch_subscribe", "hostlib_fs_watch_unsubscribe"]
    );
    for entry in registry.iter() {
        let err = (entry.handler)(&[]).expect_err("handler must reject empty args");
        assert!(
            !matches!(err, HostlibError::Unimplemented { .. }),
            "fs_watch method {} should be implemented, got {err:?}",
            entry.name
        );
    }
}

#[test]
fn tools_capability_registers_documented_methods() {
    let registry = collect_into_registry(ToolsCapability);
    let names: Vec<_> = registry.iter().map(|b| b.name).collect();
    assert_eq!(
        names,
        vec![
            // Deterministic tools — implementations live in
            // `crates/harn-hostlib/src/tools/`. Gated by
            // `hostlib_enable("tools:deterministic")`.
            "hostlib_tools_search",
            "hostlib_tools_read_file",
            "hostlib_tools_write_file",
            "hostlib_tools_delete_file",
            "hostlib_tools_list_directory",
            "hostlib_tools_get_file_outline",
            "hostlib_tools_git",
            // Process tools. Also gated by
            // `hostlib_enable("tools:deterministic")`.
            "hostlib_tools_run_command",
            "hostlib_tools_read_command_output",
            "hostlib_tools_wait_command",
            "hostlib_tools_run_test",
            "hostlib_tools_run_build_command",
            "hostlib_tools_inspect_test_results",
            "hostlib_tools_manage_packages",
            "hostlib_tools_cancel_handle",
            "hostlib_tools_toolchain_facts",
            // Per-session opt-in builtin.
            "hostlib_enable",
        ]
    );

    // All implemented tools must refuse to run before
    // `hostlib_enable("tools:deterministic")`. We check each entry so newly
    // wired tools cannot accidentally bypass the opt-in gate.
    harn_hostlib::tools::permissions::reset();
    let gated_methods = [
        "hostlib_tools_search",
        "hostlib_tools_read_file",
        "hostlib_tools_write_file",
        "hostlib_tools_delete_file",
        "hostlib_tools_list_directory",
        "hostlib_tools_get_file_outline",
        "hostlib_tools_git",
        "hostlib_tools_run_command",
        "hostlib_tools_read_command_output",
        "hostlib_tools_wait_command",
        "hostlib_tools_run_test",
        "hostlib_tools_run_build_command",
        "hostlib_tools_inspect_test_results",
        "hostlib_tools_manage_packages",
        "hostlib_tools_cancel_handle",
        "hostlib_tools_toolchain_facts",
    ];
    for name in gated_methods {
        let entry = registry.find(name).expect("registered");
        let err = (entry.handler)(&[]).expect_err("disabled by default");
        match err {
            HostlibError::Backend { builtin, message } => {
                assert_eq!(builtin, name);
                assert!(
                    message.contains("hostlib_enable"),
                    "gating error must point users at hostlib_enable: {message}"
                );
            }
            other => panic!("expected Backend gate error for {name}, got {other:?}"),
        }
    }
}

#[test]
fn secret_store_capability_registers_documented_methods() {
    let registry = collect_into_registry(SecretStoreCapability);
    let names: Vec<_> = registry.iter().map(|b| b.name).collect();
    assert_eq!(
        names,
        vec![
            "hostlib_secret_store_get",
            "hostlib_secret_store_set",
            "hostlib_secret_store_delete",
            "hostlib_secret_store_list",
        ]
    );
    // Each entry must refuse an empty payload with a structured
    // `MissingParameter` rather than panicking.
    let expected_missing: &[(&str, &str)] = &[
        ("hostlib_secret_store_get", "account"),
        ("hostlib_secret_store_set", "account"),
        ("hostlib_secret_store_delete", "account"),
        ("hostlib_secret_store_list", "account"),
    ];
    for (name, expected_param) in expected_missing {
        let entry = registry.find(name).expect("registered");
        let err = (entry.handler)(&[]).expect_err("must reject empty args");
        match err {
            HostlibError::MissingParameter { builtin, param } => {
                assert_eq!(builtin, *name);
                assert_eq!(param, *expected_param);
            }
            other => panic!("expected MissingParameter for {name}, got {other:?}"),
        }
    }
}

#[cfg(feature = "terminal-session")]
#[test]
fn terminal_session_capability_is_complete_and_default_off() {
    let registry = collect_into_registry(TerminalSessionCapability::new());
    let names: Vec<_> = registry.iter().map(|builtin| builtin.name).collect();
    assert_eq!(
        names,
        vec![
            "hostlib_terminal_session_start",
            "hostlib_terminal_session_send_keys",
            "hostlib_terminal_session_capture",
            "hostlib_terminal_session_resize",
            "hostlib_terminal_session_wait_idle",
            "hostlib_terminal_session_end",
        ]
    );
    permissions::reset();
    for builtin in registry.iter() {
        let error = (builtin.handler)(&[]).expect_err("terminal sessions default off");
        match error {
            HostlibError::Backend {
                builtin: name,
                message,
            } => {
                assert_eq!(name, builtin.name);
                assert!(message.contains("terminal:session"));
                assert!(message.contains("hostlib_enable"));
            }
            other => panic!("expected terminal feature gate, got {other:?}"),
        }
    }
}

#[test]
fn install_default_wires_every_module_into_a_vm() {
    let mut vm = harn_vm::Vm::new();
    let registry = harn_hostlib::install_default(&mut vm);

    // `mut` is only needed when the `computer` feature adds a module below; the
    // allow keeps the no-feature build (CI default) warning-clean.
    #[cfg_attr(
        not(any(feature = "computer", feature = "terminal-session")),
        allow(unused_mut)
    )]
    let mut expected = vec![
        "ast",
        "code_index",
        "scanner",
        "embed",
        "fs",
        "fs",
        "fs_watch",
        "tools",
        "secret_store",
        "verdict",
        "host_lease",
    ];
    // The computer-use module is registered only when the `computer` feature is
    // compiled in (it is out of default/full so headless/Linux CI is unaffected).
    #[cfg(feature = "computer")]
    expected.push("computer");
    #[cfg(feature = "terminal-session")]
    expected.push("terminal_session");
    assert_eq!(registry.modules(), expected.as_slice());
    // Builtin count: 15 ast (incl. apply_node + insert_at_anchor) +
    // 29 code_index (incl. add_readonly_roots, #2403 follow-up) + 2 scanner
    // + 4 embed + 4 fs + 4 fs_snapshot + 2 fs_watch + 14 tools
    // + 1 hostlib_enable + 4 secret_store + 1 verdict + 1 host_lease = 81.
    assert!(registry.builtins().len() >= 81);
}

#[test]
fn host_lease_capability_registers_read_only_status() {
    let registry = collect_into_registry(HostLeaseCapability);
    let names: Vec<_> = registry.iter().map(|builtin| builtin.name).collect();
    assert_eq!(names, vec!["hostlib_host_lease_status"]);

    let entry = registry
        .find("hostlib_host_lease_status")
        .expect("registered");
    let error = (entry.handler)(&[]).expect_err("host is required");
    match error {
        HostlibError::MissingParameter { builtin, param } => {
            assert_eq!(builtin, "hostlib_host_lease_status");
            assert_eq!(param, "host");
        }
        other => panic!("expected missing host error, got {other:?}"),
    }

    let whitespace_host = VmValue::dict([("host", VmValue::string("  "))]);
    let error = (entry.handler)(&[whitespace_host]).expect_err("blank host is invalid");
    match error {
        HostlibError::InvalidParameter { builtin, param, .. } => {
            assert_eq!(builtin, "hostlib_host_lease_status");
            assert_eq!(param, "host");
        }
        other => panic!("expected invalid host error, got {other:?}"),
    }
}

#[test]
fn stdlib_host_lease_status_reads_empty_active_and_recovered_state() {
    let root = TempDir::new().expect("lease root");
    let _env = HostLeaseRootGuard::set(root.path());
    let store = HostLeaseStore::for_root(root.path()).expect("store");

    let source = r#"
import { host_lease_status } from "std/host_lease"

pipeline default(task) {
  return host_lease_status("mac-local")
}
"#;

    let empty = expect_dict(execute_harn(source).expect("empty state"));
    assert!(matches!(empty.get("active"), Some(VmValue::Nil)));
    assert!(matches!(
        empty.get("recovered_stale_lease"),
        Some(VmValue::Bool(false))
    ));

    let acquired = store
        .try_acquire(HostLeaseRequest {
            host: "mac-local".to_string(),
            resource_class: HostLeaseResourceClass::WholeMachine,
            execution_context: None,
            owner: "registration-test".to_string(),
            priority_class: HostLeasePriorityClass::Measurement,
            ttl_ms: Some(60_000),
            owner_pid: None,
            reason: Some("pipeline contract test".to_string()),
            metadata: BTreeMap::from([("lane".to_string(), "p7".to_string())]),
        })
        .expect("acquire");
    assert!(acquired.handle.is_some(), "fixture lease acquires");

    let active = expect_dict(execute_harn(source).expect("active state"));
    let Some(VmValue::Dict(handle)) = active.get("active") else {
        panic!("active fixture lease must reach the Harn wrapper");
    };
    assert_eq!(
        handle.get("owner").map(VmValue::display),
        Some("registration-test".to_string())
    );
    assert_eq!(
        handle.get("priority_class").map(VmValue::display),
        Some("measurement".to_string())
    );

    // Make expiry deterministic rather than sleeping. The registry's public
    // status operation remains the only code under test that performs recovery.
    let database = store.root().join("host-leases.sqlite");
    let connection = rusqlite::Connection::open(database).expect("lease database");
    connection
        .execute(
            "UPDATE host_leases SET expires_at_ms = 0 WHERE host = ?1",
            ["mac-local"],
        )
        .expect("expire fixture lease");

    let recovered = expect_dict(execute_harn(source).expect("recovered state"));
    assert!(matches!(recovered.get("active"), Some(VmValue::Nil)));
    assert!(matches!(
        recovered.get("recovered_stale_lease"),
        Some(VmValue::Bool(true))
    ));
}

fn expect_dict(value: VmValue) -> harn_vm::value::DictMap {
    match value {
        VmValue::Dict(dict) => (*dict).clone(),
        other => panic!("expected dict response, got {other:?}"),
    }
}

#[test]
fn registered_hostlib_builtins_validate_request_schema_before_handler() {
    permissions::reset();
    let result = execute_harn(
        r"
pipeline default(task) {
  return hostlib_tools_run_command({argv: [1]})
}
",
    );
    let error = match result {
        Err(VmError::Thrown(VmValue::Dict(error))) => error,
        other => panic!("expected structured hostlib request validation error, got {other:?}"),
    };
    assert_eq!(
        error.get("kind").map(VmValue::display),
        Some("invalid_parameter".to_string())
    );
    assert_eq!(
        error.get("builtin").map(VmValue::display),
        Some("hostlib_tools_run_command".to_string())
    );
    let message = error
        .get("message")
        .map(VmValue::display)
        .unwrap_or_default();
    assert!(
        message.contains("argv[0]") && message.contains("expected type 'string'"),
        "unexpected validation message: {message}"
    );
}

#[test]
fn registered_hostlib_enable_normalizes_legacy_feature_string_before_validation() {
    permissions::reset();
    let result = execute_harn(
        r#"
pipeline default(task) {
  return hostlib_enable("tools:deterministic")
}
"#,
    )
    .expect("hostlib_enable string form remains accepted through schema normalization");
    let dict = result.as_dict().expect("hostlib_enable returns a dict");
    assert_eq!(
        dict.get("feature").map(VmValue::display),
        Some("tools:deterministic".to_string())
    );
    assert!(matches!(dict.get("enabled"), Some(VmValue::Bool(true))));
}

#[cfg(all(unix, feature = "terminal-session"))]
#[test]
fn registered_terminal_session_round_trips_through_harn_and_schemas() {
    permissions::reset();
    let result = execute_harn(
        r#"
pipeline default(task) {
  hostlib_enable("terminal:session")
  let started = hostlib_terminal_session_start({
    argv: ["sh", "-c", "stty -echo; printf READY; IFS= read -r line; printf GOT:%s \"$line\"; cat"],
    rows: 4,
    columns: 20
  })
  let idle_before = hostlib_terminal_session_wait_idle({
    session_id: started.session_id,
    quiet_ms: 10,
    timeout_ms: 3000
  })
  let before_send = hostlib_terminal_session_capture({session_id: started.session_id})
  let sent = hostlib_terminal_session_send_keys({
    session_id: started.session_id,
    events: [
      {type: "text", text: "hello"},
      {type: "key", key: {kind: "named", name: "enter"}}
    ]
  })
  let idle_after = hostlib_terminal_session_wait_idle({
    session_id: started.session_id,
    after_revision: before_send.revision,
    quiet_ms: 10,
    timeout_ms: 3000
  })
  let resized = hostlib_terminal_session_resize({
    session_id: started.session_id,
    rows: 6,
    columns: 30
  })
  let captured = hostlib_terminal_session_capture({session_id: started.session_id})
  let ended = hostlib_terminal_session_end({
    session_id: started.session_id,
    timeout_ms: 3000
  })
  return {
    started: started,
    idle_before: idle_before,
    before_send: before_send,
    sent: sent,
    idle_after: idle_after,
    resized: resized,
    captured: captured,
    ended: ended
  }
}
"#,
    )
    .expect("terminal hostlib operations should round-trip through the VM");
    let result = result.as_dict().expect("pipeline returns a dict");
    for (field, method) in [
        ("started", "start"),
        ("idle_before", "wait_idle"),
        ("before_send", "capture"),
        ("sent", "send_keys"),
        ("idle_after", "wait_idle"),
        ("resized", "resize"),
        ("captured", "capture"),
        ("ended", "end"),
    ] {
        let value = result
            .get(field)
            .unwrap_or_else(|| panic!("missing {field} response"));
        assert_response_schema("terminal_session", method, value);
    }
    let captured = result
        .get("captured")
        .and_then(VmValue::as_dict)
        .expect("typed capture");
    let rows = match captured.get("text_rows") {
        Some(VmValue::List(rows)) => rows,
        other => panic!("expected text rows list, got {other:?}"),
    };
    let screen = rows
        .iter()
        .map(VmValue::display)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(screen.contains("READY"));
    assert!(screen.contains("GOT:hello"));
    assert_eq!(
        captured.get("rows").map(VmValue::display),
        Some("6".to_string())
    );
    let ended = result
        .get("ended")
        .and_then(VmValue::as_dict)
        .expect("typed end status");
    assert_eq!(
        ended.get("state").map(VmValue::display),
        Some("exited".to_string())
    );
}

#[test]
fn registered_safe_text_patch_validates_dollar_defs_expected_hash() {
    permissions::reset();
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("notes.txt");
    fs::write(&file, "alpha").unwrap();
    let expected_hash = sha256_label(b"alpha");
    let source = format!(
        r#"
pipeline default(task) {{
  hostlib_enable("tools:deterministic")
  return hostlib_fs_safe_text_patch({{
    path: "{}",
    content: "beta",
    expected_hash: "{}"
  }})
}}
"#,
        harn_string_literal(&file.to_string_lossy()),
        expected_hash
    );

    let result = execute_harn(&source)
        .expect("safe_text_patch expected_hash should validate through #/$defs before dispatch");
    let dict = result.as_dict().expect("safe_text_patch returns a dict");
    assert_eq!(
        dict.get("result").map(VmValue::display),
        Some("applied".to_string())
    );
    assert!(matches!(dict.get("applied"), Some(VmValue::Bool(true))));
    assert_eq!(
        dict.get("before_sha256").map(VmValue::display),
        Some(expected_hash)
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "beta");
}

#[test]
fn embed_capability_registers_documented_methods() {
    let registry = collect_into_registry(EmbedCapability::default());
    let names: Vec<_> = registry.iter().map(|b| b.name).collect();
    assert_eq!(
        names,
        vec![
            "hostlib_embed_similarity",
            "hostlib_embed_top_k",
            "hostlib_embed_vector",
            "hostlib_embed_info",
        ]
    );
    // The default backend is the always-available lexical floor and every
    // method must round-trip without a model asset present.
    let info = registry
        .find("hostlib_embed_info")
        .expect("info builtin registered");
    let out = (info.handler)(&[]).expect("info runs with no args");
    assert!(matches!(out, harn_vm::VmValue::Dict(_)));
}

#[test]
fn every_registered_builtin_has_request_and_response_schemas() {
    let registry = HostlibRegistry::new()
        .with(AstCapability)
        .with(CodeIndexCapability::new())
        .with(ScannerCapability)
        .with(EmbedCapability::default())
        .with(FsCapability)
        .with(FsSnapshotCapability)
        .with(FsWatchCapability)
        .with(ToolsCapability)
        .with(SecretStoreCapability)
        .with(HostLeaseCapability);
    #[cfg(feature = "terminal-session")]
    let registry = registry.with(TerminalSessionCapability::new());

    for entry in registry.builtins().iter() {
        assert!(
            schemas::lookup(entry.module, entry.method, schemas::SchemaKind::Request).is_some(),
            "missing request schema for {}.{}",
            entry.module,
            entry.method
        );
        assert!(
            schemas::lookup(entry.module, entry.method, schemas::SchemaKind::Response).is_some(),
            "missing response schema for {}.{}",
            entry.module,
            entry.method
        );
    }
}

#[test]
fn every_schema_parses_as_valid_json_schema_2020_12() {
    for (module, method, kind, body) in schemas::SCHEMAS {
        let value: serde_json::Value = serde_json::from_str(body).unwrap_or_else(|err| {
            panic!("schema for {module}.{method} ({kind:?}) is not valid JSON: {err}")
        });
        let dialect = value
            .get("$schema")
            .and_then(|v| v.as_str())
            .expect("every shipped schema must declare its dialect via $schema");
        assert!(
            dialect.contains("draft/2020-12"),
            "schema for {module}.{method} ({kind:?}) declares unexpected dialect: {dialect}"
        );
        // Sanity check on shape: every schema must be an object and either
        // declare a top-level `type` or be a pure `$ref`. This catches
        // accidental empty or malformed files without forcing a full
        // schema-validator dependency at scaffold stage.
        assert!(
            value.is_object(),
            "schema for {module}.{method} ({kind:?}) must be a JSON object"
        );
        let object = value.as_object().unwrap();
        assert!(
            object.contains_key("type") || object.contains_key("$ref"),
            "schema for {module}.{method} ({kind:?}) must declare `type` or `$ref`"
        );
    }
}
