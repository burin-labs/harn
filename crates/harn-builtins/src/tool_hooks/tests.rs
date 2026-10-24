use super::*;
use harn_vm::stdlib::register_vm_stdlib;
use harn_vm_core::value::VmDictExt;

fn vm_with_stdlib() -> Vm {
    let mut vm = harn_vm::new_vm();
    register_vm_stdlib(&mut vm);
    vm
}

fn call_sync(vm: &Vm, name: &str, args: &[VmValue]) -> Result<VmValue, VmError> {
    let builtin = vm
        .builtin_handler_for_test(name)
        .cloned()
        .expect("builtin registered");
    let mut out = String::new();
    builtin(args, &mut out)
}

fn sample_rule_config() -> VmValue {
    let mut config: harn_vm_core::value::DictMap = harn_vm_core::value::DictMap::new();
    config.put_str("id", "rust.cargo.target_dir_conflict");
    config.put_str("pattern", r"^cargo (build|test)\b");
    config.put("applies_to", VmValue::strings(["rust"]));
    config.put_str("severity", "warning");
    config.put_str(
        "explanation",
        "Concurrent cargo runs without --target-dir thrash the lockfile",
    );
    VmValue::dict(config)
}

fn sample_catalogue_config(rule: VmValue) -> VmValue {
    let mut config: harn_vm_core::value::DictMap = harn_vm_core::value::DictMap::new();
    config.put_str("id", "harn-canon/rust");
    config.put_str("stack", "rust");
    config.put_str("version", "0.1.0");
    config.put_str("source", "harn-canon");
    config.put("rules", VmValue::list(vec![rule]));
    VmValue::dict(config)
}

fn dict_string(dict: &harn_vm_core::value::DictMap, key: &str) -> String {
    dict.get(key).map(|v| v.display()).unwrap_or_default()
}

#[test]
fn tool_rule_constructor_tags_dict() {
    let vm = vm_with_stdlib();
    let result = call_sync(&vm, "tool_rule", &[sample_rule_config()]).expect("tool_rule ok");
    let dict = result.as_dict().expect("dict");
    assert_eq!(dict_string(dict, "_type"), TOOL_RULE_TYPE);
    assert!(dict.get("priority").is_some(), "priority defaulted");
    assert!(dict.get("references").is_some(), "references defaulted");
}

#[test]
fn tool_rule_rejects_bad_severity() {
    let vm = vm_with_stdlib();
    let mut config: harn_vm_core::value::DictMap = harn_vm_core::value::DictMap::new();
    config.put_str("id", "r");
    config.put_str("pattern", ".");
    config.put_str("severity", "catastrophic");
    let result = call_sync(&vm, "tool_rule", &[VmValue::dict(config)]);
    assert!(matches!(result, Err(VmError::Thrown(_))));
}

#[test]
fn tool_rule_rejects_invalid_regex() {
    let vm = vm_with_stdlib();
    let mut config: harn_vm_core::value::DictMap = harn_vm_core::value::DictMap::new();
    config.put_str("id", "r");
    config.put_str("pattern", "[invalid");
    let result = call_sync(&vm, "tool_rule", &[VmValue::dict(config)]);
    let Err(VmError::Thrown(VmValue::String(message))) = result else {
        panic!("expected thrown string error, got {result:?}");
    };
    assert!(message.contains("invalid regex"));
}

#[test]
fn catalogue_round_trips_via_json() {
    let vm = vm_with_stdlib();
    let rule = call_sync(&vm, "tool_rule", &[sample_rule_config()]).expect("rule");
    let cat = call_sync(&vm, "catalogue", &[sample_catalogue_config(rule)]).expect("cat");
    let json = call_sync(&vm, "json_stringify", std::slice::from_ref(&cat)).expect("encode");
    let decoded = call_sync(&vm, "json_parse", &[json]).expect("decode");
    let original = cat.as_dict().expect("dict");
    let decoded_dict = decoded.as_dict().expect("dict");
    assert_eq!(dict_string(decoded_dict, "id"), dict_string(original, "id"));
    assert_eq!(
        dict_string(decoded_dict, "stack"),
        dict_string(original, "stack")
    );
    assert_eq!(
        dict_string(decoded_dict, "version"),
        dict_string(original, "version")
    );
    assert_eq!(
        dict_string(decoded_dict, "_type"),
        dict_string(original, "_type")
    );
}

#[test]
fn registry_register_replaces_by_id() {
    let vm = vm_with_stdlib();
    let registry = call_sync(&vm, "tool_hooks_registry", &[]).expect("registry");
    let rule = call_sync(&vm, "tool_rule", &[sample_rule_config()]).expect("rule");
    let cat = call_sync(&vm, "catalogue", &[sample_catalogue_config(rule)]).expect("cat");
    let r1 = call_sync(&vm, "tool_hooks_register", &[registry, cat.clone()]).expect("register");
    let r2 = call_sync(&vm, "tool_hooks_register", &[r1, cat]).expect("re-register replaces");
    let list = call_sync(&vm, "tool_hooks_list", &[r2]).expect("list");
    let items = match list {
        VmValue::List(items) => items,
        _ => panic!("expected list"),
    };
    assert_eq!(items.len(), 1, "duplicate id replaces, not appends");
}

#[test]
fn registry_unregister_removes_catalogue() {
    let vm = vm_with_stdlib();
    let registry = call_sync(&vm, "tool_hooks_registry", &[]).expect("registry");
    let rule = call_sync(&vm, "tool_rule", &[sample_rule_config()]).expect("rule");
    let cat = call_sync(&vm, "catalogue", &[sample_catalogue_config(rule)]).expect("cat");
    let registered = call_sync(&vm, "tool_hooks_register", &[registry, cat]).expect("ok");
    let pruned = call_sync(
        &vm,
        "tool_hooks_unregister",
        &[registered, VmValue::string("harn-canon/rust")],
    )
    .expect("unregister");
    let list = call_sync(&vm, "tool_hooks_list", &[pruned]).expect("list");
    let items = match list {
        VmValue::List(items) => items,
        _ => panic!("expected list"),
    };
    assert!(items.is_empty(), "list empty after unregister");
}

#[test]
fn context_stacks_normalizes_shapes() {
    let dict_form = VmValue::dict([("stacks", VmValue::strings(["rust"]))]);
    assert_eq!(context_stacks(Some(&dict_form)), vec!["rust".to_string()]);

    let dict_string = VmValue::dict([("stacks", VmValue::string("python"))]);
    assert_eq!(
        context_stacks(Some(&dict_string)),
        vec!["python".to_string()]
    );

    let list_form = VmValue::strings(["typescript", "rust"]);
    assert_eq!(
        context_stacks(Some(&list_form)),
        vec!["typescript".to_string(), "rust".to_string()]
    );

    let raw_string = VmValue::string("swift");
    assert_eq!(context_stacks(Some(&raw_string)), vec!["swift".to_string()]);

    assert!(context_stacks(None).is_empty());
    assert!(context_stacks(Some(&VmValue::Nil)).is_empty());
}

#[test]
fn registry_filter_keeps_matching_and_stackless_catalogues() {
    let vm = vm_with_stdlib();
    let rule = call_sync(&vm, "tool_rule", &[sample_rule_config()]).expect("rule");
    let rust_cat =
        call_sync(&vm, "catalogue", &[sample_catalogue_config(rule.clone())]).expect("cat");
    let mut shell_cfg: harn_vm_core::value::DictMap = harn_vm_core::value::DictMap::new();
    shell_cfg.put_str("id", "harn-canon/shell");
    // Stackless catalogue: matches every requested stack — universal rules.
    shell_cfg.put("rules", VmValue::list(vec![rule.clone()]));
    let shell_cat = call_sync(&vm, "catalogue", &[VmValue::dict(shell_cfg)]).expect("cat");
    let mut python_cfg: harn_vm_core::value::DictMap = harn_vm_core::value::DictMap::new();
    python_cfg.put_str("id", "harn-canon/python");
    python_cfg.put_str("stack", "python");
    python_cfg.put("rules", VmValue::list(vec![rule]));
    let py_cat = call_sync(&vm, "catalogue", &[VmValue::dict(python_cfg)]).expect("cat");

    let registry = call_sync(&vm, "tool_hooks_registry", &[]).expect("registry");
    let r1 = call_sync(&vm, "tool_hooks_register", &[registry, rust_cat]).expect("r1");
    let r2 = call_sync(&vm, "tool_hooks_register", &[r1, shell_cat]).expect("r2");
    let r3 = call_sync(&vm, "tool_hooks_register", &[r2, py_cat]).expect("r3");

    // Empty stacks filter is a no-op — every catalogue survives.
    let unfiltered = call_sync(
        &vm,
        "tool_hooks_filter",
        &[r3.clone(), VmValue::list(Vec::new())],
    )
    .expect("unfiltered");
    assert_eq!(
        match call_sync(&vm, "tool_hooks_list", &[unfiltered]).expect("list") {
            VmValue::List(items) => items.len(),
            _ => panic!("list"),
        },
        3,
    );

    let only_rust =
        call_sync(&vm, "tool_hooks_filter", &[r3, VmValue::strings(["rust"])]).expect("filtered");
    let listed = call_sync(&vm, "tool_hooks_list", &[only_rust]).expect("list");
    let items = match listed {
        VmValue::List(items) => items,
        _ => panic!("expected list"),
    };
    // rust catalogue + stackless shell catalogue remain; python drops.
    assert_eq!(items.len(), 2);
    let ids: Vec<String> = items
        .iter()
        .filter_map(|v| match v {
            VmValue::Dict(d) => d.get("id").map(|id| id.display()),
            _ => None,
        })
        .collect();
    assert!(ids.iter().any(|id| id == "harn-canon/rust"));
    assert!(ids.iter().any(|id| id == "harn-canon/shell"));
}

#[test]
fn applies_to_matching_respects_empty_lists() {
    assert!(applies_to_matches(&[], &[]));
    assert!(applies_to_matches(
        &[],
        &["python".to_string(), "rust".to_string()]
    ));
    assert!(!applies_to_matches(&["rust".to_string()], &[]));
    assert!(applies_to_matches(
        &["rust".to_string()],
        &["rust".to_string()]
    ));
    assert!(!applies_to_matches(
        &["rust".to_string()],
        &["python".to_string()]
    ));
}

// TH-03: emit_audit + inject_reminder side-effect helpers.

fn audit_payload(rule_id: &str, command: &str) -> VmValue {
    let mut payload: harn_vm_core::value::DictMap = harn_vm_core::value::DictMap::new();
    payload.put_str("rule_id", rule_id);
    payload.put_str("command", command);
    VmValue::dict(payload)
}

fn reminder_options(body: &str, tag: &str, ttl: i64) -> VmValue {
    let mut options: harn_vm_core::value::DictMap = harn_vm_core::value::DictMap::new();
    options.put_str("body", body);
    options.put("tags", VmValue::list(vec![VmValue::string(tag)]));
    options.put("ttl_turns", VmValue::Int(ttl));
    VmValue::dict(options)
}

#[test]
fn tool_hooks_emit_audit_records_lifecycle_entry() {
    let vm = vm_with_stdlib();
    // Drain anything in the log so other tests don't bleed in.
    let _ = harn_vm::orchestration::take_lifecycle_audit_log();
    let entry = call_sync(
        &vm,
        "tool_hooks_emit_audit",
        &[
            VmValue::string("tool_rewrite"),
            audit_payload("rust.cargo.target_dir", "cargo build"),
        ],
    )
    .expect("emit_audit ok");
    let dict = entry.as_dict().expect("entry dict");
    assert_eq!(dict_string(dict, "kind"), "tool_rewrite");
    let drained = harn_vm::orchestration::take_lifecycle_audit_log();
    assert_eq!(drained.len(), 1, "exactly one audit entry recorded");
    assert_eq!(drained[0].kind, "tool_rewrite");
    assert_eq!(
        drained[0].payload.get("rule_id").and_then(|v| v.as_str()),
        Some("rust.cargo.target_dir")
    );
}

#[test]
fn tool_hooks_emit_audit_rejects_empty_kind() {
    let vm = vm_with_stdlib();
    let result = call_sync(
        &vm,
        "tool_hooks_emit_audit",
        &[VmValue::string(""), VmValue::Nil],
    );
    let Err(VmError::Thrown(VmValue::String(message))) = result else {
        panic!("expected thrown error, got {result:?}");
    };
    assert!(message.contains("non-empty"));
}

#[test]
fn tool_hooks_inject_reminder_records_audit_when_no_session() {
    let vm = vm_with_stdlib();
    let _ = harn_vm::orchestration::take_lifecycle_audit_log();
    let report = call_sync(
        &vm,
        "tool_hooks_inject_reminder",
        &[reminder_options("heads up", "tool_rewritten", 1)],
    )
    .expect("inject_reminder ok");
    let dict = report.as_dict().expect("report dict");
    // No active session in this unit test, so session_attached is false
    // but the audit entry still records the side-effect.
    assert!(
        matches!(dict.get("session_attached"), Some(VmValue::Bool(false))),
        "no live session in unit test"
    );
    assert!(
        matches!(dict.get("reminder_id"), Some(VmValue::String(s)) if !s.is_empty()),
        "reminder_id populated"
    );
    let drained = harn_vm::orchestration::take_lifecycle_audit_log();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].kind, "tool_hooks.reminder_injected");
    assert_eq!(
        drained[0].payload.get("body").and_then(|v| v.as_str()),
        Some("heads up")
    );
    assert_eq!(
        drained[0].payload.get("ttl_turns").and_then(|v| v.as_i64()),
        Some(1)
    );
}

#[test]
fn tool_hooks_inject_reminder_requires_body() {
    let vm = vm_with_stdlib();
    let mut options: harn_vm_core::value::DictMap = harn_vm_core::value::DictMap::new();
    options.put("tags", VmValue::strings(["x"]));
    let result = call_sync(&vm, "tool_hooks_inject_reminder", &[VmValue::dict(options)]);
    let Err(VmError::Thrown(VmValue::String(message))) = result else {
        panic!("expected thrown error, got {result:?}");
    };
    assert!(message.contains("body"));
}

// TH-05: classifier cache helpers.

fn verdict_value(kind: &str) -> VmValue {
    let mut dict: harn_vm_core::value::DictMap = harn_vm_core::value::DictMap::new();
    dict.put_str("kind", kind);
    dict.put("confidence", VmValue::Float(0.9));
    VmValue::dict(dict)
}

#[test]
fn classifier_cache_roundtrips_value() {
    let vm = vm_with_stdlib();
    // Clean slate so prior tests don't bleed in via the thread-local map.
    call_sync(&vm, "__tool_hooks_classifier_cache_clear", &[]).expect("clear");
    let put = call_sync(
        &vm,
        "__tool_hooks_classifier_cache_put",
        &[
            VmValue::string("scope:hashA"),
            verdict_value("rewrite"),
            VmValue::Int(0),
            VmValue::Nil,
        ],
    )
    .expect("put");
    assert!(matches!(put, VmValue::Nil));
    let got = call_sync(
        &vm,
        "__tool_hooks_classifier_cache_get",
        &[VmValue::string("scope:hashA"), VmValue::Int(0)],
    )
    .expect("get");
    let dict = got.as_dict().expect("verdict dict");
    assert_eq!(dict_string(dict, "kind"), "rewrite");
}

#[test]
fn classifier_cache_expires_entries_after_ttl() {
    let vm = vm_with_stdlib();
    call_sync(&vm, "__tool_hooks_classifier_cache_clear", &[]).expect("clear");
    call_sync(
        &vm,
        "__tool_hooks_classifier_cache_put",
        &[
            VmValue::string("scope:expiring"),
            verdict_value("deny"),
            VmValue::Int(1_000),
            VmValue::Int(500), // 500ms TTL → expires at 1_500.
        ],
    )
    .expect("put");
    // Within window → returns the verdict.
    let fresh = call_sync(
        &vm,
        "__tool_hooks_classifier_cache_get",
        &[VmValue::string("scope:expiring"), VmValue::Int(1_200)],
    )
    .expect("get fresh");
    assert!(matches!(fresh, VmValue::Dict(_)));
    // At/after expiry → nil (and the entry is evicted on read).
    let expired = call_sync(
        &vm,
        "__tool_hooks_classifier_cache_get",
        &[VmValue::string("scope:expiring"), VmValue::Int(1_600)],
    )
    .expect("get expired");
    assert!(matches!(expired, VmValue::Nil));
}

#[test]
fn classifier_cache_rejects_empty_key() {
    let vm = vm_with_stdlib();
    let result = call_sync(
        &vm,
        "__tool_hooks_classifier_cache_get",
        &[VmValue::string(""), VmValue::Int(0)],
    );
    let Err(VmError::Thrown(VmValue::String(message))) = result else {
        panic!("expected thrown error, got {result:?}");
    };
    assert!(message.contains("non-empty"));
}
