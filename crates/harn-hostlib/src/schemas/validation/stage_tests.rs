use super::*;
use crate::registry::{BuiltinRegistry, HostlibCapability};

#[test]
fn run_and_wait_stage_metadata_use_the_compiled_boundary_schema() {
    let mut registry = BuiltinRegistry::new();
    crate::tools::ToolsCapability.register_builtins(&mut registry);
    let run = registry
        .find("hostlib_tools_run_command")
        .unwrap()
        .handler
        .clone();
    let wait = registry
        .find("hostlib_tools_wait_command")
        .unwrap()
        .handler
        .clone();
    for background in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let request = harn_vm::json_to_vm_value(&serde_json::json!({
            "mode": "shell", "shell_id": "sh",
            "command": "printf setup && test -f missing && /usr/bin/printf final",
            "cwd": root.path(), "env_mode": "replace", "env": {"PATH": "/usr/bin:/bin"},
            "background": background, "capture": {"final_stage": true},
            "timeout_ms": harn_clock::test_support::HANG_CEILING.as_millis() as u64,
        }));
        let started = run(&[request]).unwrap();
        let (method, response) = if background {
            let started = crate::json::vm_value_to_json(&started);
            let handle = started["handle_id"]
                .as_str()
                .expect("actual background handle");
            let request = harn_vm::json_to_vm_value(&serde_json::json!({
                "handle_id": handle,
                "timeout_ms": harn_clock::test_support::HANG_CEILING.as_millis() as u64,
            }));
            ("wait_command", wait(&[request]).unwrap())
        } else {
            ("run_command", started)
        };
        let observed = crate::json::vm_value_to_json(&response);
        assert_eq!(observed["exit_code"], 1, "{observed}");
        assert_eq!(observed["stdout"], "setup");
        assert_eq!(
            observed["stage_output"]["failed_clause"],
            serde_json::json!({
                "index": 2, "total": 3, "source": "test -f missing",
            })
        );
        validate_response("stage_schema_control", "tools", method, response)
            .expect("actual run/wait responses must satisfy the same compiled public schema");
        for invalid in [
            serde_json::json!({"index": 0, "total": 3, "source": "test -f missing"}),
            serde_json::json!({"index": "2", "total": 3, "source": "test -f missing"}),
            serde_json::json!({"index": 2, "source": "test -f missing"}),
            serde_json::json!({"index": 2, "total": 3, "source": "test -f missing", "extra": true}),
        ] {
            let mut forged = observed.clone();
            forged["stage_output"]["failed_clause"] = invalid;
            assert!(
                validate_response(
                    "stage_schema_control",
                    "tools",
                    method,
                    harn_vm::json_to_vm_value(&forged)
                )
                .is_err(),
                "{forged}"
            );
        }
    }
}
