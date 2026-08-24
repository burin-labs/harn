use super::*;

#[test]
fn test_loaded_sources_reports_entry_program() {
    let mut dbg = Debugger::new();
    let (_dir, file) = write_temp_program(
        "loaded.harn",
        "pipeline t(harness: Harness, task: unknown) { harness.stdio.log(\"hi\") }",
    );
    dbg.handle_message(make_request(1, "initialize", None));
    dbg.handle_message(make_request(
        2,
        "launch",
        Some(json!({"program": file.to_string_lossy()})),
    ));
    let responses = dbg.handle_message(make_request(3, "loadedSources", None));
    let body = responses[0].body.as_ref().unwrap();
    let sources = body["sources"].as_array().unwrap();
    assert!(
        sources
            .iter()
            .any(|s| s.get("path").and_then(|p| p.as_str()).is_some()),
        "loadedSources must include at least the entry script's path"
    );
}

#[test]
fn test_modules_reports_entry_module() {
    let mut dbg = Debugger::new();
    let (_dir, file) = write_temp_program(
        "mods.harn",
        "pipeline t(harness: Harness, task: unknown) { harness.stdio.log(\"hi\") }",
    );
    dbg.handle_message(make_request(1, "initialize", None));
    dbg.handle_message(make_request(
        2,
        "launch",
        Some(json!({"program": file.to_string_lossy()})),
    ));
    let responses = dbg.handle_message(make_request(3, "modules", None));
    let body = responses[0].body.as_ref().unwrap();
    let modules = body["modules"].as_array().unwrap();
    assert!(!modules.is_empty(), "modules list must not be empty");
    let total = body["totalModules"].as_u64().unwrap_or(0);
    assert!(total >= 1);
    let first = &modules[0];
    assert!(first.get("id").is_some());
    assert!(first.get("name").is_some());
    assert!(first.get("path").is_some());
}

#[test]
fn test_modules_with_explicit_zero_module_count_returns_all() {
    // Per DAP, `moduleCount: 0` disables paging and means "all remaining".
    let mut dbg = Debugger::new();
    let (_dir, file) = write_temp_program(
        "zerocount.harn",
        "pipeline t(harness: Harness, task: unknown) { harness.stdio.log(\"hi\") }",
    );
    dbg.handle_message(make_request(1, "initialize", None));
    dbg.handle_message(make_request(
        2,
        "launch",
        Some(json!({"program": file.to_string_lossy()})),
    ));
    let responses = dbg.handle_message(make_request(
        3,
        "modules",
        Some(json!({"startModule": 0, "moduleCount": 0})),
    ));
    let body = responses[0].body.as_ref().unwrap();
    let modules = body["modules"].as_array().unwrap();
    let total = body["totalModules"].as_u64().unwrap_or(0);
    assert_eq!(
        modules.len() as u64,
        total,
        "moduleCount: 0 must return every module (paging disabled)"
    );
}
