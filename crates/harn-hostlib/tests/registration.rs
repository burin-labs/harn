//! Integration tests asserting that every module's registration surface
//! compiles, that unimplemented methods route through `HostlibError` rather
//! than panicking, and that every registered builtin has a matching schema.
//!
//! These tests are the contract implementation work must keep green:
//! when a module moves beyond scaffolding, the only change here should be
//! that a routed `Unimplemented` becomes a real return value — never a
//! removed builtin.

use harn_hostlib::{
    ast::AstCapability, code_index::CodeIndexCapability, fs_watch::FsWatchCapability,
    scanner::ScannerCapability, schemas, tools::ToolsCapability, BuiltinRegistry,
    HostlibCapability, HostlibError, HostlibRegistry,
};

fn collect_into_registry<C: HostlibCapability>(cap: C) -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    cap.register_builtins(&mut registry);
    registry
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
        ]
    );
    // Each AST builtin must reject an empty payload with `MissingParameter`
    // (never `Unimplemented`). The required field differs per method:
    // file-based builtins want `path`; the analysis builtins added by #773
    // accept either `content` or `path` and surface that as the
    // `content_or_path` synthetic parameter.
    let expected_param = |name: &str| -> &'static str {
        match name {
            "hostlib_ast_parse_errors" | "hostlib_ast_undefined_names" => "content_or_path",
            _ => "path",
        }
    };
    for entry in registry.iter() {
        let err = (entry.handler)(&[]).expect_err("handler must error on empty args");
        match err {
            HostlibError::MissingParameter { builtin, param } => {
                assert_eq!(builtin, entry.name);
                assert_eq!(param, expected_param(entry.name));
            }
            other => panic!(
                "expected MissingParameter for {}, got {other:?}",
                entry.name
            ),
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
            // File table accessors (#776).
            "hostlib_code_index_path_to_id",
            "hostlib_code_index_id_to_path",
            "hostlib_code_index_file_ids",
            "hostlib_code_index_file_meta",
            "hostlib_code_index_file_hash",
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
            "hostlib_tools_run_test",
            "hostlib_tools_run_build_command",
            "hostlib_tools_inspect_test_results",
            "hostlib_tools_manage_packages",
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
        "hostlib_tools_run_test",
        "hostlib_tools_run_build_command",
        "hostlib_tools_inspect_test_results",
        "hostlib_tools_manage_packages",
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
fn install_default_wires_every_module_into_a_vm() {
    let mut vm = harn_vm::Vm::new();
    let registry = harn_hostlib::install_default(&mut vm);

    assert_eq!(
        registry.modules(),
        &["ast", "code_index", "scanner", "fs_watch", "tools"]
    );
    // Builtin count: 5 ast + 27 code_index + 2 scanner + 2 fs_watch + 12
    // tools + 1 hostlib_enable = 49.
    assert!(registry.builtins().len() >= 49);
}

#[test]
fn every_registered_builtin_has_request_and_response_schemas() {
    let registry = HostlibRegistry::new()
        .with(AstCapability)
        .with(CodeIndexCapability::new())
        .with(ScannerCapability)
        .with(FsWatchCapability)
        .with(ToolsCapability);

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
