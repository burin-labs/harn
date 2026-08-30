use super::process_exec::{process_exec_stdin, resolve_process_exec_cwd};
use super::{
    build_sandboxed_command, capability_manifest_with_mocks, clear_host_call_bridge,
    dispatch_host_operation, dispatch_host_tool_call, dispatch_host_tool_list,
    dispatch_mock_host_call, dispatch_mock_hostlib_call, host_call_ready, host_has_builtin,
    host_mock_clear_builtin, parse_host_mock, push_host_mock, register_mockable_host_operation,
    register_scoped_mockable_host_operation, reset_host_state, reset_scoped_host_state,
    set_host_call_bridge, validate_host_mock_registration, HostCallBridge, HostCallDispatchFuture,
    HostMock,
};
use crate::value::VmDictExt;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use crate::value::{VmError, VmValue};

/// Collect a built command's env mutations as `(name, Option<value>)`,
/// where `None` marks a variable the command removes from the inherited
/// environment.
fn command_env(
    cmd: &tokio::process::Command,
) -> std::collections::BTreeMap<String, Option<String>> {
    cmd.as_std()
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect()
}

#[test]
fn build_sandboxed_command_forces_deterministic_message_locale() {
    // A verify command spawned by a non-Anglosphere user whose *shell*
    // exports LC_ALL (inherited via the parent env, NOT pinned by the
    // caller's `env` dict) must still emit English diagnostics, or the
    // downstream English-keyed matchers (syntax repair, error grounding,
    // pass/fail classification) misfire. In merge mode the child inherits
    // the parent env implicitly, so the builder must issue an explicit
    // LC_ALL removal — observable here as a `(key, None)` mutation — and
    // pin LC_MESSAGES=C + DOTNET_CLI_UI_LANGUAGE=en. The caller pins no
    // locale key here, so the overlay engages.
    let mut params = crate::value::DictMap::new();
    params.put_str("mode", "argv");
    params.put(
        "argv",
        VmValue::List(Arc::new(vec![VmValue::string("/bin/true")])),
    );
    params.put_str("env_mode", "merge");
    let mut caller_env = crate::value::DictMap::new();
    // An innocuous caller env key that must NOT suppress the locale overlay.
    caller_env.put_str("CARGO_TARGET_DIR", "/tmp/target");
    params.put("env", VmValue::dict_map(caller_env));

    let cmd = build_sandboxed_command(&params, "process.exec").expect("build command");
    let env = command_env(&cmd);

    assert_eq!(
        env.get("LC_ALL"),
        Some(&None),
        "the builder must remove LC_ALL from the child so an inherited shell \
             value cannot override the forced LC_MESSAGES"
    );
    assert_eq!(
        env.get("LC_MESSAGES"),
        Some(&Some("C".to_string())),
        "LC_MESSAGES must be pinned to C for untranslated (English) tool output"
    );
    assert_eq!(
        env.get("DOTNET_CLI_UI_LANGUAGE"),
        Some(&Some("en".to_string())),
        ".NET ignores LC_* and needs its own UI-language override"
    );
}

#[test]
fn build_sandboxed_command_respects_a_caller_pinned_locale() {
    // A caller that explicitly pins the locale keys (or LC_ALL) wins over
    // the deterministic overlay — same caller-wins rule as TMPDIR.
    let mut params = crate::value::DictMap::new();
    params.put_str("mode", "argv");
    params.put(
        "argv",
        VmValue::List(Arc::new(vec![VmValue::string("/bin/true")])),
    );
    params.put_str("env_mode", "merge");
    let mut caller_env = crate::value::DictMap::new();
    caller_env.put_str("LC_ALL", "fr_FR.UTF-8");
    caller_env.put_str("LC_MESSAGES", "fr_FR.UTF-8");
    params.put("env", VmValue::dict_map(caller_env));

    let cmd = build_sandboxed_command(&params, "process.exec").expect("build command");
    let env = command_env(&cmd);

    assert_eq!(
        env.get("LC_ALL"),
        Some(&Some("fr_FR.UTF-8".to_string())),
        "a caller that pins LC_ALL keeps it — the overlay must not strip an explicit value"
    );
    assert_eq!(
        env.get("LC_MESSAGES"),
        Some(&Some("fr_FR.UTF-8".to_string())),
        "a caller-pinned LC_MESSAGES wins over the C overlay"
    );
}

#[test]
fn process_exec_relative_cwd_resolves_against_execution_root() {
    let dir = tempfile::tempdir().expect("tempdir");
    crate::stdlib::process::set_thread_execution_context(Some(
        crate::orchestration::RunExecutionRecord {
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            source_dir: Some(dir.path().join("src").to_string_lossy().into_owned()),
            ..Default::default()
        },
    ));

    assert_eq!(
        resolve_process_exec_cwd("subdir"),
        dir.path().join("subdir")
    );

    crate::stdlib::process::set_thread_execution_context(None);
}

#[test]
fn workspace_project_root_fallback_prefers_execution_context_project_root() {
    run_host_async_test(|| async {
        let project = tempfile::tempdir().expect("project root");
        let cwd = tempfile::tempdir().expect("cwd");
        crate::stdlib::process::set_thread_execution_context(Some(
            crate::orchestration::RunExecutionRecord {
                cwd: Some(cwd.path().to_string_lossy().into_owned()),
                project_root: Some(project.path().to_string_lossy().into_owned()),
                ..Default::default()
            },
        ));

        let result =
            dispatch_host_operation("workspace", "project_root", &crate::value::DictMap::new())
                .await
                .expect("workspace.project_root result");

        crate::stdlib::process::set_thread_execution_context(None);
        assert_eq!(result.display(), project.path().display().to_string());
    });
}

#[test]
fn manifest_includes_operation_metadata() {
    let manifest = capability_manifest_with_mocks();
    let process = manifest
        .as_dict()
        .and_then(|d| d.get("process"))
        .and_then(|v| v.as_dict())
        .expect("process capability");
    assert!(process.get("description").is_some());
    let operations = process
        .get("operations")
        .and_then(|v| v.as_dict())
        .expect("operations dict");
    assert!(operations.get("exec").is_some());
}

#[test]
fn mocked_capabilities_appear_in_manifest() {
    reset_host_state();
    push_host_mock(HostMock {
        capability: "project".to_string(),
        operation: "metadata_get".to_string(),
        params: None,
        result: Some(VmValue::dict(crate::value::DictMap::new())),
        error: None,
        unregistered_ok: false,
    });
    let manifest = capability_manifest_with_mocks();
    let project = manifest
        .as_dict()
        .and_then(|d| d.get("project"))
        .and_then(|v| v.as_dict())
        .expect("project capability");
    let operations = project
        .get("operations")
        .and_then(|v| v.as_dict())
        .expect("operations dict");
    assert!(operations.get("metadata_get").is_some());
    reset_host_state();
}

#[test]
fn mock_host_call_matches_partial_params_and_overrides_order() {
    reset_host_state();
    let mut exact_params = crate::value::DictMap::new();
    exact_params.put_str("namespace", "facts");
    push_host_mock(HostMock {
        capability: "project".to_string(),
        operation: "metadata_get".to_string(),
        params: None,
        result: Some(VmValue::String(arcstr::ArcStr::from("fallback"))),
        error: None,
        unregistered_ok: false,
    });
    push_host_mock(HostMock {
        capability: "project".to_string(),
        operation: "metadata_get".to_string(),
        params: Some(exact_params),
        result: Some(VmValue::String(arcstr::ArcStr::from("facts"))),
        error: None,
        unregistered_ok: false,
    });

    let mut call_params = crate::value::DictMap::new();
    call_params.put_str("dir", "pkg");
    call_params.put_str("namespace", "facts");
    let exact = dispatch_mock_host_call("project", "metadata_get", &call_params)
        .expect("expected exact mock")
        .expect("exact mock should succeed");
    assert_eq!(exact.display(), "facts");

    call_params.put_str("namespace", "classification");
    let fallback = dispatch_mock_host_call("project", "metadata_get", &call_params)
        .expect("expected fallback mock")
        .expect("fallback mock should succeed");
    assert_eq!(fallback.display(), "fallback");
    reset_host_state();
}

#[test]
fn mock_host_call_can_throw_errors() {
    reset_host_state();
    push_host_mock(HostMock {
        capability: "project".to_string(),
        operation: "metadata_get".to_string(),
        params: None,
        result: None,
        error: Some("boom".to_string()),
        unregistered_ok: false,
    });
    let params = crate::value::DictMap::new();
    let result =
        dispatch_mock_host_call("project", "metadata_get", &params).expect("expected mock result");
    match result {
        Err(VmError::Thrown(VmValue::String(message))) => assert_eq!(message.as_str(), "boom"),
        other => panic!("unexpected result: {other:?}"),
    }
    reset_host_state();
}

#[test]
fn host_mock_registration_rejects_unknown_operations_by_default() {
    let host_mock = HostMock {
        capability: "runtime".to_string(),
        operation: "tas".to_string(),
        params: None,
        result: Some(VmValue::Nil),
        error: None,
        unregistered_ok: false,
    };
    let error = validate_host_mock_registration(&host_mock)
        .expect_err("unknown host operation should fail at registration");
    match error {
        VmError::Thrown(VmValue::String(message)) => {
            assert!(message.contains("runtime.tas"));
            assert!(message.contains("unregistered_ok"));
            assert!(message.contains("runtime.task"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn host_mock_registration_allows_explicit_test_local_operations() {
    let host_mock = HostMock {
        capability: "synthetic".to_string(),
        operation: "op".to_string(),
        params: None,
        result: Some(VmValue::Nil),
        error: None,
        unregistered_ok: true,
    };
    validate_host_mock_registration(&host_mock)
        .expect("explicit unregistered_ok should permit synthetic mocks");
}

#[test]
fn host_mock_registration_accepts_runtime_registered_operations() {
    register_mockable_host_operation(
        "code_index",
        "stats",
        "Hostlib schema-backed operation registered at runtime.",
    );
    let host_mock = HostMock {
        capability: "code_index".to_string(),
        operation: "stats".to_string(),
        params: None,
        result: Some(VmValue::Nil),
        error: None,
        unregistered_ok: false,
    };
    validate_host_mock_registration(&host_mock)
        .expect("registered hostlib operations should be mockable");
}

#[test]
fn clearing_live_mocks_preserves_scoped_manifest_declarations() {
    reset_scoped_host_state();
    register_scoped_mockable_host_operation(
        "scoped_clear_fixture",
        "answer",
        "Test-scoped manifest declaration.",
    );
    let host_mock = HostMock {
        capability: "scoped_clear_fixture".to_string(),
        operation: "answer".to_string(),
        params: None,
        result: Some(VmValue::Nil),
        error: None,
        unregistered_ok: false,
    };

    validate_host_mock_registration(&host_mock).expect("scoped declaration is registered");
    host_mock_clear_builtin(&[], &mut String::new()).expect("clear live mocks");
    validate_host_mock_registration(&host_mock)
        .expect("clearing live mocks must preserve manifest declarations");
    reset_scoped_host_state();
}

#[tokio::test]
async fn declared_mockable_operation_is_not_reported_as_callable() {
    std::thread::spawn(|| {
        register_mockable_host_operation(
            "async_host_registration",
            "cross_thread",
            "Embedding operation registered before async worker migration.",
        );
    })
    .join()
    .expect("registration worker should finish");

    std::thread::spawn(|| {
        let host_mock = HostMock {
            capability: "async_host_registration".to_string(),
            operation: "cross_thread".to_string(),
            params: None,
            result: Some(VmValue::Nil),
            error: None,
            unregistered_ok: false,
        };
        validate_host_mock_registration(&host_mock)
            .expect("process host registration should be visible after worker migration");

        let typo = HostMock {
            operation: "cross_tread".to_string(),
            ..host_mock
        };
        validate_host_mock_registration(&typo)
            .expect_err("an undeclared operation should still fail closed");
    })
    .join()
    .expect("validation worker should finish");

    assert!(matches!(
        host_has_builtin(
            &[
                VmValue::string("async_host_registration"),
                VmValue::string("cross_thread"),
            ],
            &mut String::new(),
        )
        .expect("host_has should succeed"),
        VmValue::Bool(false)
    ));
    dispatch_host_operation(
        "async_host_registration",
        "cross_thread",
        &crate::value::DictMap::new(),
    )
    .await
    .expect_err("an unmocked declaration must remain unsupported at dispatch");
}

#[test]
fn host_mock_parse_preserves_unregistered_ok_config() {
    let config = VmValue::dict(crate::value::DictMap::from_iter([
        (crate::value::intern_key("result"), VmValue::string("ok")),
        (
            crate::value::intern_key("unregistered_ok"),
            VmValue::Bool(true),
        ),
    ]));
    let host_mock = parse_host_mock(&[VmValue::string("synthetic"), VmValue::string("op"), config])
        .expect("parse host mock config");
    assert!(host_mock.unregistered_ok);
}

#[test]
fn hostlib_mock_dispatch_matches_module_method_and_params() {
    reset_host_state();
    let mut mock_params = crate::value::DictMap::new();
    mock_params.put(
        "argv",
        VmValue::List(Arc::new(vec![VmValue::string("echo")])),
    );
    push_host_mock(HostMock {
        capability: "tools".to_string(),
        operation: "run_command".to_string(),
        params: Some(mock_params),
        result: Some(VmValue::String(arcstr::ArcStr::from("direct"))),
        error: None,
        unregistered_ok: false,
    });

    let mut call_params = crate::value::DictMap::new();
    call_params.put(
        "argv",
        VmValue::List(Arc::new(vec![VmValue::string("echo")])),
    );
    call_params.put_str("cwd", "/tmp/not-used");
    let value = dispatch_mock_hostlib_call("tools", "run_command", &call_params)
        .expect("expected hostlib mock")
        .expect("hostlib mock should succeed");
    assert_eq!(value.display(), "direct");
    reset_host_state();
}

#[test]
fn hostlib_run_command_falls_back_to_process_exec_mocks() {
    reset_host_state();
    let mut mock_params = crate::value::DictMap::new();
    mock_params.put(
        "argv",
        VmValue::List(Arc::new(vec![
            VmValue::string("cargo"),
            VmValue::string("test"),
        ])),
    );
    push_host_mock(HostMock {
        capability: "process".to_string(),
        operation: "exec".to_string(),
        params: Some(mock_params),
        result: Some(VmValue::String(arcstr::ArcStr::from("legacy"))),
        error: None,
        unregistered_ok: false,
    });

    let mut call_params = crate::value::DictMap::new();
    call_params.put(
        "argv",
        VmValue::List(Arc::new(vec![
            VmValue::string("cargo"),
            VmValue::string("test"),
        ])),
    );
    call_params.put_str("cwd", "/tmp/not-used");
    let value = dispatch_mock_hostlib_call("tools", "run_command", &call_params)
        .expect("expected legacy process.exec mock")
        .expect("legacy mock should succeed");
    assert_eq!(value.display(), "legacy");
    reset_host_state();
}

#[test]
fn hostlib_run_command_prefers_exact_mock_over_process_exec_alias() {
    reset_host_state();
    let mut params = crate::value::DictMap::new();
    params.put(
        "argv",
        VmValue::List(Arc::new(vec![
            VmValue::string("npm"),
            VmValue::string("test"),
        ])),
    );
    push_host_mock(HostMock {
        capability: "process".to_string(),
        operation: "exec".to_string(),
        params: Some(params.clone()),
        result: Some(VmValue::String(arcstr::ArcStr::from("legacy"))),
        error: None,
        unregistered_ok: false,
    });
    push_host_mock(HostMock {
        capability: "tools".to_string(),
        operation: "run_command".to_string(),
        params: Some(params.clone()),
        result: Some(VmValue::String(arcstr::ArcStr::from("direct"))),
        error: None,
        unregistered_ok: false,
    });

    let value = dispatch_mock_hostlib_call("tools", "run_command", &params)
        .expect("expected exact hostlib mock")
        .expect("exact mock should succeed");
    assert_eq!(value.display(), "direct");
    reset_host_state();
}

#[derive(Default)]
struct TestHostToolBridge;

impl HostCallBridge for TestHostToolBridge {
    fn dispatch<'a>(
        &'a self,
        _capability: &'a str,
        _operation: &'a str,
        _params: &'a crate::value::DictMap,
    ) -> HostCallDispatchFuture<'a> {
        host_call_ready(Ok(None))
    }

    fn list_tools(&self) -> Result<Option<VmValue>, VmError> {
        let tool = VmValue::dict(crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("name"),
                VmValue::String(arcstr::ArcStr::from("Read".to_string())),
            ),
            (
                crate::value::intern_key("description"),
                VmValue::String(arcstr::ArcStr::from(
                    "Read a file from the host".to_string(),
                )),
            ),
            (
                crate::value::intern_key("schema"),
                VmValue::dict(crate::value::DictMap::from_iter([(
                    crate::value::intern_key("type"),
                    VmValue::String(arcstr::ArcStr::from("object".to_string())),
                )])),
            ),
            (crate::value::intern_key("deprecated"), VmValue::Bool(false)),
        ]));
        Ok(Some(VmValue::List(std::sync::Arc::new(vec![tool]))))
    }

    fn call_tool(&self, name: &str, args: &VmValue) -> Result<Option<VmValue>, VmError> {
        if name != "Read" {
            return Ok(None);
        }
        let path = args
            .as_dict()
            .and_then(|dict| dict.get("path"))
            .map(|value| value.display())
            .unwrap_or_default();
        Ok(Some(VmValue::String(arcstr::ArcStr::from(format!(
            "read:{path}"
        )))))
    }
}

struct CountingProcessExecBridge {
    calls: Arc<AtomicUsize>,
}

impl HostCallBridge for CountingProcessExecBridge {
    fn dispatch<'a>(
        &'a self,
        capability: &'a str,
        operation: &'a str,
        _params: &'a crate::value::DictMap,
    ) -> HostCallDispatchFuture<'a> {
        if (capability, operation) != ("process", "exec") {
            return host_call_ready(Ok(None));
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        host_call_ready(Ok(Some(VmValue::dict(crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("status"),
                VmValue::String(arcstr::ArcStr::from("completed".to_string())),
            ),
            (crate::value::intern_key("exit_code"), VmValue::Int(0)),
            (crate::value::intern_key("success"), VmValue::Bool(true)),
        ])))))
    }
}

fn run_host_async_test<F, Fut>(test: F)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    // Several of these install or clear a host-call bridge, which opens a
    // new turn and so bumps the process-global epoch. Hold the shared lock
    // so that cannot invalidate a `turn_cache` test's entry mid-assertion.
    let _guard = super::turn_cache::epoch_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local.run_until(test()).await;
    });
}

#[test]
fn host_tool_list_uses_installed_host_call_bridge() {
    run_host_async_test(|| async {
        reset_host_state();
        set_host_call_bridge(Arc::new(TestHostToolBridge));
        let tools = dispatch_host_tool_list().await.expect("tool list");
        clear_host_call_bridge();

        let VmValue::List(items) = tools else {
            panic!("expected tool list");
        };
        assert_eq!(items.len(), 1);
        let tool = items[0].as_dict().expect("tool dict");
        assert_eq!(tool.get("name").unwrap().display(), "Read");
        assert_eq!(tool.get("deprecated").unwrap().display(), "false");
    });
}

#[test]
fn host_tool_call_uses_installed_host_call_bridge() {
    run_host_async_test(|| async {
        set_host_call_bridge(Arc::new(TestHostToolBridge));
        let args = VmValue::dict(crate::value::DictMap::from_iter([(
            crate::value::intern_key("path"),
            VmValue::String(arcstr::ArcStr::from("README.md".to_string())),
        )]));
        let value = dispatch_host_tool_call("Read", &args)
            .await
            .expect("tool call");
        clear_host_call_bridge();
        assert_eq!(value.display(), "read:README.md");
    });
}

#[test]
fn process_exec_bridge_is_gated_by_command_policy() {
    run_host_async_test(|| async {
        crate::orchestration::clear_command_policies();
        let calls = Arc::new(AtomicUsize::new(0));
        set_host_call_bridge(Arc::new(CountingProcessExecBridge {
            calls: calls.clone(),
        }));
        crate::orchestration::push_command_policy(crate::orchestration::CommandPolicy {
            tools: vec!["run".to_string()],
            workspace_roots: Vec::new(),
            default_shell_mode: "shell".to_string(),
            deny_patterns: vec!["cat *".to_string()],
            ..Default::default()
        });

        let result = dispatch_host_operation(
            "process",
            "exec",
            &crate::value::DictMap::from_iter([
                (
                    crate::value::intern_key("mode"),
                    VmValue::String(arcstr::ArcStr::from("shell")),
                ),
                (
                    crate::value::intern_key("command"),
                    VmValue::String(arcstr::ArcStr::from("cat Cargo.toml")),
                ),
            ]),
        )
        .await
        .expect("process.exec result");

        crate::orchestration::clear_command_policies();
        clear_host_call_bridge();

        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "blocked command must not reach host bridge"
        );
        let result = result.as_dict().expect("blocked result dict");
        assert_eq!(result.get("status").unwrap().display(), "blocked");
        assert!(
            result
                .get("reason")
                .map(VmValue::display)
                .unwrap_or_default()
                .contains("cat *"),
            "blocked result should name the matched policy pattern"
        );
    });
}

#[cfg(unix)]
async fn process_exec_env_probe(env: VmValue, env_mode: Option<&str>) -> (String, String) {
    // Run `sh -c 'printf "%s|%s" "$PARENT_VAR" "$CHILD_VAR"'` so we can
    // observe whether an inherited parent var survives alongside the
    // explicitly-provided child var. The parent var is set on this
    // process's environment immediately before the spawn.
    std::env::set_var("PARENT_VAR", "inherited");
    let mut params = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("mode"),
            VmValue::String(arcstr::ArcStr::from("argv")),
        ),
        (
            crate::value::intern_key("argv"),
            VmValue::List(std::sync::Arc::new(vec![
                // Absolute path so the spawn does not depend on PATH,
                // which the `replace` case intentionally clears.
                VmValue::String(arcstr::ArcStr::from("/bin/sh")),
                VmValue::String(arcstr::ArcStr::from("-c")),
                VmValue::String(arcstr::ArcStr::from(
                    "printf '%s|%s' \"$PARENT_VAR\" \"$CHILD_VAR\"",
                )),
            ])),
        ),
        (crate::value::intern_key("env"), env),
    ]);
    if let Some(mode) = env_mode {
        params.put_str("env_mode", mode);
    }
    let result = super::dispatch_process_exec(&params, serde_json::Value::Null)
        .await
        .expect("process.exec result");
    let dict = result.as_dict().expect("result dict");
    let stdout = dict.get("stdout").map(VmValue::display).unwrap_or_default();
    std::env::remove_var("PARENT_VAR");
    let (parent, child) = stdout.split_once('|').unwrap_or((&stdout, ""));
    (parent.to_string(), child.to_string())
}

#[cfg(unix)]
#[test]
fn process_exec_env_default_merges_with_parent() {
    run_host_async_test(|| async {
        // No `env_mode`: the provided key must be added WITHOUT clearing
        // the inherited parent environment (the env-clear footgun fix).
        let child_env = VmValue::dict(crate::value::DictMap::from_iter([(
            crate::value::intern_key("CHILD_VAR"),
            VmValue::String(arcstr::ArcStr::from("provided")),
        )]));
        let (parent, child) = process_exec_env_probe(child_env, None).await;
        assert_eq!(
            parent, "inherited",
            "default env_mode must inherit parent env"
        );
        assert_eq!(
            child, "provided",
            "default env_mode must apply provided keys"
        );
    });
}

#[cfg(unix)]
#[test]
fn process_exec_env_mode_replace_clears_parent() {
    run_host_async_test(|| async {
        // Explicit `replace`: the inherited parent var must be gone and
        // only the provided key survives. This preserves the ability to
        // fully replace the environment when intentionally requested.
        let child_env = VmValue::dict(crate::value::DictMap::from_iter([(
            crate::value::intern_key("CHILD_VAR"),
            VmValue::String(arcstr::ArcStr::from("provided")),
        )]));
        let (parent, child) = process_exec_env_probe(child_env, Some("replace")).await;
        assert_eq!(parent, "", "explicit replace must clear parent env");
        assert_eq!(
            child, "provided",
            "explicit replace must keep provided keys"
        );
    });
}

#[cfg(unix)]
#[test]
fn process_exec_env_mode_unknown_is_rejected() {
    run_host_async_test(|| async {
        let params = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("mode"),
                VmValue::String(arcstr::ArcStr::from("argv")),
            ),
            (
                crate::value::intern_key("argv"),
                VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                    arcstr::ArcStr::from("true"),
                )])),
            ),
            (
                crate::value::intern_key("env"),
                VmValue::dict(crate::value::DictMap::from_iter([(
                    crate::value::intern_key("CHILD_VAR"),
                    VmValue::String(arcstr::ArcStr::from("x")),
                )])),
            ),
            (
                crate::value::intern_key("env_mode"),
                VmValue::String(arcstr::ArcStr::from("bogus")),
            ),
        ]);
        let err = super::dispatch_process_exec(&params, serde_json::Value::Null)
            .await
            .expect_err("unknown env_mode must error");
        assert!(
            format!("{err:?}").contains("env_mode"),
            "error should name env_mode, got {err:?}"
        );
    });
}

#[test]
fn process_exec_stdin_preserves_absent_nil_and_explicit_empty() {
    let missing = crate::value::DictMap::new();
    assert_eq!(
        process_exec_stdin(&missing, "process.exec").expect("missing stdin"),
        crate::stdlib::sandbox::ProcessStdin::Null
    );

    let nil = crate::value::DictMap::from_iter([(crate::value::intern_key("stdin"), VmValue::Nil)]);
    assert_eq!(
        process_exec_stdin(&nil, "process.exec").expect("nil stdin"),
        crate::stdlib::sandbox::ProcessStdin::Null
    );

    let empty = crate::value::DictMap::from_iter([(
        crate::value::intern_key("stdin"),
        VmValue::string(""),
    )]);
    assert_eq!(
        process_exec_stdin(&empty, "process.exec").expect("empty stdin"),
        crate::stdlib::sandbox::ProcessStdin::Bytes(Vec::new()),
        "an explicit empty stream must not collapse into missing input"
    );

    let invalid =
        crate::value::DictMap::from_iter([(crate::value::intern_key("stdin"), VmValue::Int(0))]);
    let error = process_exec_stdin(&invalid, "process.exec")
        .expect_err("a non-string stdin must fail at the host seam");
    assert!(error.to_string().contains("stdin must be a string or nil"));
}

#[test]
fn process_exec_stdin_child_echo() {
    if std::env::var_os("HARN_PROCESS_STDIN_ECHO_CHILD").is_none() {
        return;
    }
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).expect("child reads stdin");
    print!("HARN_STDIN_ECHO_START{input}HARN_STDIN_ECHO_END");
}

#[test]
fn process_exec_delivers_stdin_to_a_real_child() {
    run_host_async_test(|| async {
        let current_exe = std::env::current_exe().expect("current test executable");
        let input = "line one\nline two: λ\n";
        let env = VmValue::dict(crate::value::DictMap::from_iter([(
            crate::value::intern_key("HARN_PROCESS_STDIN_ECHO_CHILD"),
            VmValue::string("1"),
        )]));
        let params = crate::value::DictMap::from_iter([
            (crate::value::intern_key("mode"), VmValue::string("argv")),
            (
                crate::value::intern_key("argv"),
                VmValue::List(std::sync::Arc::new(vec![
                    VmValue::string(current_exe.to_string_lossy()),
                    VmValue::string("process_exec_stdin_child_echo"),
                    VmValue::string("--nocapture"),
                ])),
            ),
            (crate::value::intern_key("env"), env),
            (crate::value::intern_key("stdin"), VmValue::string(input)),
            (crate::value::intern_key("timeout_ms"), VmValue::Int(10_000)),
        ]);

        let result = super::dispatch_process_exec(&params, serde_json::Value::Null)
            .await
            .expect("process.exec result");
        let receipt = result.as_dict().expect("process receipt");
        assert!(
            matches!(receipt.get("success"), Some(VmValue::Bool(true))),
            "the child process must complete successfully"
        );
        let stdout = receipt
            .get("stdout")
            .map(VmValue::display)
            .unwrap_or_default();
        assert!(
            stdout.contains(&format!("HARN_STDIN_ECHO_START{input}HARN_STDIN_ECHO_END")),
            "the real child must echo the exact multiline Unicode input; stdout={stdout:?}"
        );
    });
}

// Drive the real `host_call("process","exec")` builder under a restricted
// policy and read back the `$TMPDIR` the child actually saw. This is the
// agent-facing path; the assertion is OS-independent (it observes the
// injected env, not OS-sandbox enforcement), so it pins the mechanism on
// every CI host while the live OS-level link proof runs on a Linux host
// with Landlock available.
#[cfg(unix)]
async fn process_exec_tmpdir_probe(
    workspace: &std::path::Path,
    caller_env: Option<VmValue>,
) -> String {
    let mut env_pairs = vec![(
        crate::value::intern_key("mode"),
        VmValue::String(arcstr::ArcStr::from("argv")),
    )];
    env_pairs.push((
        crate::value::intern_key("argv"),
        VmValue::List(std::sync::Arc::new(vec![
            VmValue::String(arcstr::ArcStr::from("/bin/sh")),
            VmValue::String(arcstr::ArcStr::from("-c")),
            VmValue::String(arcstr::ArcStr::from("printf '%s' \"$TMPDIR\"")),
        ])),
    ));
    if let Some(env) = caller_env {
        env_pairs.push((crate::value::intern_key("env"), env));
    }
    let params = crate::value::DictMap::from_iter(env_pairs);

    crate::orchestration::push_execution_policy(crate::orchestration::CapabilityPolicy {
        sandbox_profile: crate::orchestration::SandboxProfile::Worktree,
        workspace_roots: vec![workspace.to_string_lossy().into_owned()],
        // Keep OS confinement out of this unit assertion regardless of host
        // Landlock/seatbelt availability; we are pinning the env injection,
        // not OS enforcement (which the Linux Landlock run proves
        // end-to-end).
        ..crate::orchestration::CapabilityPolicy::default()
    });
    std::env::set_var("HARN_HANDLER_SANDBOX", "off");
    let result = super::dispatch_process_exec(&params, serde_json::Value::Null)
        .await
        .expect("process.exec result");
    std::env::remove_var("HARN_HANDLER_SANDBOX");
    crate::orchestration::pop_execution_policy();
    result
        .as_dict()
        .and_then(|d| d.get("stdout"))
        .map(VmValue::display)
        .unwrap_or_default()
}

#[cfg(unix)]
#[test]
fn process_exec_injects_workspace_local_tmpdir() {
    run_host_async_test(|| async {
        let workspace = tempfile::tempdir().expect("workspace");
        let tmpdir = process_exec_tmpdir_probe(workspace.path(), None).await;

        assert!(
            !tmpdir.is_empty(),
            "sandboxed child must receive a non-empty TMPDIR"
        );
        let tmpdir_path = std::path::PathBuf::from(&tmpdir);
        let canonical_tmpdir = std::fs::canonicalize(&tmpdir_path)
            .expect("workspace-local TMPDIR should canonicalize");
        let canonical_workspace =
            std::fs::canonicalize(workspace.path()).expect("workspace should canonicalize");
        assert!(
            canonical_tmpdir.starts_with(&canonical_workspace),
            "child TMPDIR {tmpdir:?} must live inside the workspace {:?}",
            workspace.path()
        );
        assert!(
            tmpdir_path.ends_with(".harn-tmp"),
            "child TMPDIR {tmpdir:?} must be the workspace-local .harn-tmp dir"
        );
        assert!(
            tmpdir_path.is_dir(),
            "the workspace-local TMPDIR must have been created on disk"
        );
    });
}

#[cfg(unix)]
#[test]
fn process_exec_respects_caller_pinned_tmpdir() {
    run_host_async_test(|| async {
        let workspace = tempfile::tempdir().expect("workspace");
        let caller_tmp = workspace.path().join("caller-chosen");
        std::fs::create_dir_all(&caller_tmp).unwrap();
        let caller_env = VmValue::dict(crate::value::DictMap::from_iter([(
            crate::value::intern_key("TMPDIR"),
            VmValue::String(arcstr::ArcStr::from(
                caller_tmp.to_string_lossy().into_owned(),
            )),
        )]));

        let tmpdir = process_exec_tmpdir_probe(workspace.path(), Some(caller_env)).await;

        assert_eq!(
            std::path::PathBuf::from(&tmpdir),
            caller_tmp,
            "an explicit caller TMPDIR must override the workspace-local default"
        );
    });
}

#[test]
fn host_tool_list_is_empty_without_bridge() {
    run_host_async_test(|| async {
        clear_host_call_bridge();
        let tools = dispatch_host_tool_list().await.expect("tool list");
        let VmValue::List(items) = tools else {
            panic!("expected tool list");
        };
        assert!(items.is_empty());
    });
}
