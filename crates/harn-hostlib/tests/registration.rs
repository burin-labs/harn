//! Integration tests asserting that every module's registration surface
//! compiles, that unimplemented methods route through `HostlibError` rather
//! than panicking, and that every registered builtin has a matching schema.
//!
//! These tests are the contract that follow-up implementation issues
//! (B2/B3/B4/C1/C2/C3) must keep green: when an issue lands, the only
//! change here should be that a routed `Unimplemented` becomes a real
//! return value — never a removed builtin.

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

fn assert_all_unimplemented(registry: &BuiltinRegistry) {
    for entry in registry.iter() {
        let err = (entry.handler)(&[]).expect_err("scaffold methods must be unimplemented");
        match err {
            HostlibError::Unimplemented { builtin } => {
                assert_eq!(
                    builtin, entry.name,
                    "builtin name mismatch: registry says {} but handler says {builtin}",
                    entry.name
                );
            }
            other => panic!("expected Unimplemented, got {other:?}"),
        }
    }
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
        ]
    );
    assert_all_unimplemented(&registry);
}

#[test]
fn code_index_capability_registers_documented_methods() {
    let registry = collect_into_registry(CodeIndexCapability);
    let names: Vec<_> = registry.iter().map(|b| b.name).collect();
    assert_eq!(
        names,
        vec![
            "hostlib_code_index_query",
            "hostlib_code_index_rebuild",
            "hostlib_code_index_stats",
            "hostlib_code_index_imports_for",
            "hostlib_code_index_importers_of",
        ]
    );
    assert_all_unimplemented(&registry);
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
    assert_all_unimplemented(&registry);
}

#[test]
fn fs_watch_capability_registers_documented_methods() {
    let registry = collect_into_registry(FsWatchCapability);
    let names: Vec<_> = registry.iter().map(|b| b.name).collect();
    assert_eq!(
        names,
        vec!["hostlib_fs_watch_subscribe", "hostlib_fs_watch_unsubscribe"]
    );
    assert_all_unimplemented(&registry);
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
            // Process tools — slated for issue C2; still routed through
            // `HostlibError::Unimplemented` until then.
            "hostlib_tools_run_command",
            "hostlib_tools_run_test",
            "hostlib_tools_run_build_command",
            "hostlib_tools_inspect_test_results",
            "hostlib_tools_manage_packages",
            // Per-session opt-in builtin from issue #567.
            "hostlib_enable",
        ]
    );

    // Process tools must still route through `Unimplemented` — they are
    // slated for issue C2.
    let process_methods = [
        "hostlib_tools_run_command",
        "hostlib_tools_run_test",
        "hostlib_tools_run_build_command",
        "hostlib_tools_inspect_test_results",
        "hostlib_tools_manage_packages",
    ];
    for name in process_methods {
        let entry = registry.find(name).expect("registered");
        let err = (entry.handler)(&[]).expect_err("must be unimplemented");
        assert!(
            matches!(err, HostlibError::Unimplemented { builtin } if builtin == name),
            "expected Unimplemented for {name}, got {err:?}"
        );
    }

    // Deterministic tools must refuse to run before
    // `hostlib_enable("tools:deterministic")`. We check one representative
    // builtin to confirm the gating handler is wired.
    harn_hostlib::tools::permissions::reset();
    let search = registry.find("hostlib_tools_search").expect("registered");
    let err = (search.handler)(&[]).expect_err("disabled by default");
    match err {
        HostlibError::Backend { builtin, message } => {
            assert_eq!(builtin, "hostlib_tools_search");
            assert!(
                message.contains("hostlib_enable"),
                "gating error must point users at hostlib_enable: {message}"
            );
        }
        other => panic!("expected Backend gate error, got {other:?}"),
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
    // Builtin count: 3 ast + 5 code_index + 2 scanner + 2 fs_watch + 12
    // tools + 1 hostlib_enable = 25.
    assert!(registry.builtins().len() >= 25);
}

#[test]
fn every_registered_builtin_has_request_and_response_schemas() {
    let registry = HostlibRegistry::new()
        .with(AstCapability)
        .with(CodeIndexCapability)
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
