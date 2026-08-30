use std::fs;

use serde_json::Value as JsonValue;

use crate::test_util::process::harn_e2e_command;

fn fixture() -> (tempfile::TempDir, String) {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("widgets.harn");
    fs::write(
        &path,
        r#"
import { ToolRegistry, tool_registry_from } from "std/tools"

fn registry() -> ToolRegistry {
  return tool_registry_from([
    {
      name: "lookup_widget",
      description: "Fetch one widget.",
      parameters: {
        widget_id: {schema: {type: "integer", minimum: 1}, required: true},
        verbose: {schema: {type: "boolean"}, required: false},
      },
      returns: {
        type: "object",
        properties: {id: {type: "integer"}, verbose: {type: "boolean"}},
        required: ["id", "verbose"],
        additionalProperties: false,
      },
      annotations: {readOnlyHint: true, destructiveHint: false},
      execution_policy: {kind: "fetch", side_effect_level: "network"},
      cli: {command: ["widgets", "get"]},
      source: {
        kind: "openapi",
        id: "getWidget",
        binding: {method: "GET", path: "/widgets/{widget_id}"},
      },
      handler: {args -> {id: args.widget_id, verbose: args.verbose ?? false}},
    },
  ], {name: "widgets", version: "1.2.3", description: "Widget integration"})
}

fn main(harness: Harness) {
  harness.tools.mcp_tools(registry())
}
"#,
    )
    .expect("write fixture");
    (temp, path.display().to_string())
}

#[test]
fn tool_registry_projects_schema_help_and_execution_from_one_handler() {
    let (_temp, path) = fixture();

    let schema = harn_e2e_command()
        .args(["tool", "schema", &path])
        .output()
        .expect("schema command");
    assert!(
        schema.status.success(),
        "schema failed: {}",
        String::from_utf8_lossy(&schema.stderr)
    );
    let schema: JsonValue = serde_json::from_slice(&schema.stdout).expect("schema JSON");
    assert_eq!(schema["schema_version"], "harn-tools/1.0");
    assert_eq!(schema["info"]["name"], "widgets");
    assert_eq!(schema["tools"][0]["name"], "lookup_widget");
    assert_eq!(
        schema["tools"][0]["cli"]["command"],
        serde_json::json!(["widgets", "get"])
    );
    assert_eq!(schema["tools"][0]["policy"]["kind"], "fetch");
    assert_eq!(schema["tools"][0]["source"]["binding"]["method"], "GET");

    let help = harn_e2e_command()
        .args(["tool", "run", &path, "widgets", "get", "--help"])
        .output()
        .expect("help command");
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("--widget-id <INT>"), "{help}");
    assert!(help.contains("--harn-input"), "{help}");

    let run = harn_e2e_command()
        .args([
            "tool",
            "run",
            &path,
            "widgets",
            "get",
            "--widget-id",
            "42",
            "--verbose",
            "false",
        ])
        .output()
        .expect("run command");
    assert!(
        run.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<JsonValue>(&run.stdout).expect("run JSON"),
        serde_json::json!({"id": 42, "verbose": false})
    );
}

#[test]
fn tool_registry_cli_rejects_schema_violations_before_dispatch() {
    let (_temp, path) = fixture();
    let output = harn_e2e_command()
        .args(["tool", "run", &path, "widgets", "get", "--widget-id", "0"])
        .output()
        .expect("invalid run command");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("do not match its schema"), "{stderr}");
    assert!(stderr.contains("minimum"), "{stderr}");
}
