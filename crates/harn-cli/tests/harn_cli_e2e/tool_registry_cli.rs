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
      cli: {
        command: ["widgets", "get"],
        arguments: {
          widget_id: {position: 0, value_name: "WIDGET"},
          verbose: {long: "verbose", short: "v", aliases: ["detailed"]},
        },
      },
      source: {
        kind: "openapi",
        id: "getWidget",
        binding: {method: "GET", path: "/widgets/{widget_id}"},
      },
      handler: {args -> {id: args.widget_id, verbose: args.verbose ?? false}},
    },
    {
      name: "operator_receipt",
      description: "Read one operator receipt.",
      parameters: {},
      governance: {audiences: ["catalog", "cli"]},
      cli: {command: ["operator", "receipt"]},
      handler: {_args -> {surface: "cli"}},
    },
    {
      name: "remote_probe",
      description: "Probe one remote integration.",
      parameters: {},
      governance: {audiences: ["catalog", "mcp"]},
      cli: {command: ["remote", "probe"]},
      handler: {_args -> {surface: "mcp"}},
    },
  ], {
    info: {name: "widgets", version: "1.2.3", description: "Widget integration"},
    cli: {
      commands: [{
        command: ["widgets"],
        title: "Manage widgets",
        aliases: ["w"],
        display_order: 1,
      }],
    },
  })
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
    assert_eq!(
        schema["tools"][1]["governance"]["audiences"],
        serde_json::json!(["cli", "catalog"])
    );

    let help = harn_e2e_command()
        .args(["tool", "run", &path, "widgets", "get", "--help"])
        .output()
        .expect("help command");
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("[WIDGET]"), "{help}");
    assert!(help.contains("-v, --verbose <VERBOSE>"), "{help}");
    assert!(help.contains("--harn-input"), "{help}");
    assert!(help.contains("--json"), "{help}");

    let run = harn_e2e_command()
        .args([
            "tool", "run", &path, "w", "get", "42", "-v", "false", "--json",
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
fn tool_registry_generates_completion_from_the_same_command_tree() {
    let (_temp, path) = fixture();

    for shell in ["bash", "zsh", "fish", "power-shell"] {
        let output = harn_e2e_command()
            .args(["tool", "completions", &path, "--shell", shell])
            .output()
            .expect("completion command");
        assert!(
            output.status.success(),
            "{shell} completion failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let script = String::from_utf8_lossy(&output.stdout);
        for token in ["widgets", "get", "verbose"] {
            assert!(script.contains(token), "{shell} omitted {token}: {script}");
        }
    }
}

#[test]
fn tool_registry_cli_allows_only_cli_governed_tools() {
    let (_temp, path) = fixture();

    let allowed = harn_e2e_command()
        .args(["tool", "run", &path, "operator", "receipt", "--json"])
        .output()
        .expect("allowed CLI invocation");
    assert!(
        allowed.status.success(),
        "allowed invocation failed: {}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<JsonValue>(&allowed.stdout).unwrap(),
        serde_json::json!({"surface": "cli"})
    );

    let denied = harn_e2e_command()
        .args(["tool", "run", &path, "remote", "probe"])
        .output()
        .expect("excluded CLI invocation");
    assert!(!denied.status.success());
    assert!(denied.stdout.is_empty(), "excluded handler must not run");
    assert!(
        String::from_utf8_lossy(&denied.stderr).contains("unrecognized subcommand"),
        "{}",
        String::from_utf8_lossy(&denied.stderr)
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

#[test]
fn tool_schema_exports_is_offline_typed_and_byte_deterministic() {
    let temp = tempfile::tempdir().expect("tempdir");
    let contracts = temp.path().join("contracts.harn");
    let script = temp.path().join("server.harn");
    let execution_marker = temp.path().join("main-executed.txt");
    fs::write(
        &contracts,
        r"
pub struct Envelope<T> {
  value: T
  tags: list<string>
}
pub type Request = {query: string, options: {limit: int}}
pub type Response = Envelope<{id: string, score: float}>
",
    )
    .expect("write imported contracts");
    fs::write(
        &script,
        format!(
            r#"
import {{ Request, Response }} from "./contracts"
fn main(harness: Harness) {{
  harness.fs.write_text({}, "executed")
  panic("tool schema exports executed main")
}}
/// Search records
pub fn search(request: Request) -> Response {{
  return {{value: {{id: request.query, score: 1.0}}, tags: []}}
}}
"#,
            serde_json::to_string(&execution_marker.display().to_string()).unwrap()
        ),
    )
    .expect("write export server");

    let run = || {
        harn_e2e_command()
            .args([
                "tool",
                "schema",
                &script.display().to_string(),
                "--surface",
                "exports",
            ])
            .output()
            .expect("tool schema exports command")
    };
    let first = run();
    let second = run();
    for output in [&first, &second] {
        assert!(
            output.status.success(),
            "exports schema failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("executed main"), "{stderr}");
        assert!(!stderr.contains("capability"), "{stderr}");
    }
    assert_eq!(first.stdout, second.stdout, "catalog bytes must be stable");
    assert!(
        !execution_marker.exists(),
        "offline export discovery must not invoke main or filesystem capabilities"
    );

    let catalog: JsonValue = serde_json::from_slice(&first.stdout).expect("catalog JSON");
    assert_eq!(catalog["schema_version"], "harn-tools/1.0");
    assert_eq!(catalog["tools"][0]["name"], "search");
    assert_eq!(
        catalog["tools"][0]["inputSchema"]["properties"]["request"]["properties"]["options"]
            ["properties"]["limit"]["type"],
        "integer"
    );
    assert_eq!(
        catalog["tools"][0]["outputSchema"]["properties"]["value"]["properties"]["score"]["type"],
        "number"
    );
    assert_eq!(catalog["tools"][0]["cli"]["hidden"], false);
    assert_eq!(catalog["tools"][0]["deferLoading"], false);
}
