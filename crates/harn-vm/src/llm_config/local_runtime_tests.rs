use super::*;

#[test]
fn parse_config_rejects_unknown_local_runtime_lifecycle_values() {
    for (field, value) in [("kind", "daemon_shell"), ("stop", "kill_all")] {
        let kind = if field == "kind" {
            value
        } else {
            "managed_process"
        };
        let stop = if field == "stop" { value } else { "pid" };
        let source = format!(
            r#"
[providers.demo.local_runtime]
kind = "{kind}"
stop = "{stop}"
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
