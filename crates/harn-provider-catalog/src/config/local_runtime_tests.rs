use super::*;

#[test]
fn parse_config_rejects_unknown_local_runtime_lifecycle_values() {
    for (field, value) in [
        ("kind", "daemon_shell"),
        ("stop", "kill_all"),
        ("wire_protocol", "custom_http"),
    ] {
        let kind = if field == "kind" {
            value
        } else {
            "managed_process"
        };
        let stop = if field == "stop" { value } else { "pid" };
        let wire_protocol = if field == "wire_protocol" {
            value
        } else {
            "open_ai_compatible"
        };
        let source = format!(
            r#"
[providers.demo.local_runtime]
kind = "{kind}"
stop = "{stop}"
wire_protocol = "{wire_protocol}"
"#,
        );
        let error = parse_config_toml(&source)
            .expect_err("unknown local runtime lifecycle value must fail at the config boundary");
        let message = error.to_string();
        assert!(
            message.contains(value),
            "expected invalid {field} value in parse error, got {message:?}"
        );
    }
}

#[test]
fn local_runtime_lifecycle_rejects_incoherent_ownership_and_stop() {
    let runtime = LocalRuntimeDef {
        kind: Some(LocalRuntimeKind::ManagedProcess),
        stop: Some(LocalRuntimeStop::External),
        wire_protocol: Some(LocalRuntimeWireProtocol::OpenAiCompatible),
        ..LocalRuntimeDef::default()
    };

    assert!(matches!(
        runtime.lifecycle(),
        Err(LocalRuntimeLifecycleError::Incoherent { .. })
    ));
}
