use std::fs;
use std::path::Path;
use std::process::Output;

use harn_vm::codegen::{CodegenIr, CodegenOutput};
use harn_vm::wire::PreparedWireGraph;
use serde_json::{json, Value};

use crate::test_util::process::harn_e2e_command;
use crate::test_util::stdio_jsonrpc::StdioJsonRpcClient;

const PRIVATE_OUTPUT: &str = "PRIVATE-WRONG-OUTPUT-CONTENT";

fn run(root: &Path, args: &[&str], expected: i32) -> Output {
    let output = harn_e2e_command()
        .current_dir(root)
        .args(args)
        .output()
        .expect("run actual Harn CLI");
    assert_eq!(output.status.code(), Some(expected), "{args:?}: {output:?}");
    output
}

fn registry_source(wrong_output: bool) -> String {
    let result = if wrong_output {
        format!(
            "return {{wrong: {}}}",
            serde_json::to_string(PRIVATE_OUTPUT).unwrap()
        )
    } else {
        "return echo(args.input)".into()
    };
    format!(
        r#"
import {{ echo, mutate, reject, scheduled }} from "./source/server"
import {{ ToolRegistry }} from "std/tools"

fn main(harness: Harness) {{
  const ir = json_parse(harness.fs.read_text("contract.json"))
  const catalog = json_parse(ir.catalog_canonical_json)
  const handlers = {{
    echo: {{args ->
      const previous = harness.fs.read_text("calls.txt")
      harness.fs.write_text("calls.txt", "${{previous}}echo\n")
      {result}
    }},
    mutate: {{args -> return mutate(args.input)}},
    reject: {{args -> return reject(args.input)}},
    scheduled: {{args -> return scheduled(args.input)}},
  }}
  const registry: ToolRegistry = tool_registry_bind(catalog, handlers)
  harness.tools.mcp_tools(registry)
}}
"#
    )
}

fn request(id: i64, method: &str, mut params: Value) -> Value {
    use harn_vm::mcp_protocol as protocol;
    params["_meta"] = json!({
        (protocol::MCP_META_KEY_PROTOCOL_VERSION): protocol::PROTOCOL_VERSION,
        (protocol::MCP_META_KEY_CLIENT_INFO): {"name":"portable-wire-e2e","version":"1"},
        (protocol::MCP_META_KEY_CLIENT_CAPABILITIES): {},
    });
    harn_vm::jsonrpc::request(id, method, params)
}

fn peer(root: &Path, file: &str, surface: &str) -> StdioJsonRpcClient {
    let mut command = harn_e2e_command();
    command.current_dir(root).args([
        "serve",
        "mcp",
        "--surface",
        surface,
        "--transport",
        "stdio",
        "--obs",
        "off",
        file,
    ]);
    let mut client = StdioJsonRpcClient::spawn("portable wire MCP", command);
    let discovery = client.request(request(1, "server/discover", json!({})));
    assert!(discovery.get("error").is_none(), "{discovery}");
    let listed = client.request(request(2, "tools/list", json!({})));
    assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 4);
    client
}

fn malformed_inputs(payload: &Value) -> Vec<(&'static str, Value)> {
    let mut cases = Vec::new();
    let mut bad = payload.clone();
    bad["raw"] = json!({"$bytes_b64":"!"});
    cases.push(("base64", bad));
    let mut bad = payload.clone();
    bad["amount"] = json!("not-a-decimal");
    cases.push(("decimal", bad));
    let mut bad = payload.clone();
    bad["names"] = json!(["duplicate", "duplicate"]);
    cases.push(("set", bad));
    let mut bad = payload.clone();
    bad["choice"]["variant"] = json!("AbsentVariant");
    cases.push(("enum", bad));
    let mut bad = payload.clone();
    bad["unexpected"] = json!(true);
    cases.push(("extra", bad));
    let mut tree = json!({"value":1,"next":null});
    for _ in 0..80 {
        tree = json!({"value":1,"next":tree});
    }
    let mut bad = payload.clone();
    bad["tree"] = tree;
    cases.push(("depth", bad));
    cases
}

fn assert_payload(graph: &PreparedWireGraph, ir: &CodegenIr, expected: &Value, actual: &Value) {
    let operation = ir
        .operations
        .iter()
        .find(|operation| operation.name == "echo")
        .unwrap();
    let CodegenOutput::Value { type_id, .. } = &operation.output else {
        panic!("declared echo output")
    };
    assert_eq!(
        graph.decode(type_id, actual).expect("actual wire output"),
        graph.decode(type_id, expected).expect("known golden input")
    );
    assert_eq!(actual["sequence"].as_i64(), Some(i64::MAX));
    assert_eq!(actual["pair"][1].as_i64(), Some(9_007_199_254_740_993));
    assert_eq!(actual["amount"], expected["amount"]);
    assert_eq!(actual["raw"], expected["raw"]);
    assert!(actual["floating"].is_f64());
    assert!(actual.get("optional").is_none());
    assert_eq!(actual.get("optional_nullable"), Some(&Value::Null));
}

#[test]
fn original_harn_wire_contract_holds_across_stdio_and_generated_cli() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir(root.join("source")).unwrap();
    for (name, source) in [
        (
            "server.harn",
            include_str!("../../../harn-serve/tests/fixtures/codegen/pathological/server.harn"),
        ),
        (
            "types.harn",
            include_str!("../../../harn-serve/tests/fixtures/codegen/pathological/types.harn"),
        ),
        (
            "left.harn",
            include_str!("../../../harn-serve/tests/fixtures/codegen/pathological/left.harn"),
        ),
        (
            "right.harn",
            include_str!("../../../harn-serve/tests/fixtures/codegen/pathological/right.harn"),
        ),
    ] {
        fs::write(root.join("source").join(name), source).unwrap();
    }
    let payload: Value = serde_json::from_str(include_str!(
        "../../../harn-serve/tests/fixtures/codegen/pathological/value.json"
    ))
    .unwrap();
    let compiled = run(root, &["codegen", "ir", "--file", "source/server.harn"], 0);
    let ir: CodegenIr = serde_json::from_slice(&compiled.stdout).unwrap();
    assert_eq!(ir.operations.len(), 4);
    let graph = PreparedWireGraph::prepare(ir.types.clone()).unwrap();
    fs::write(root.join("contract.json"), &compiled.stdout).unwrap();
    fs::write(root.join("registry.harn"), registry_source(false)).unwrap();
    fs::write(root.join("wrong-output.harn"), registry_source(true)).unwrap();
    fs::write(root.join("calls.txt"), "").unwrap();
    fs::write(
        root.join("arguments.json"),
        serde_json::to_vec(&json!({"input":payload})).unwrap(),
    )
    .unwrap();
    let tool_args = |file: &'static str| {
        [
            "tool",
            "run",
            file,
            "echo",
            "--harn-input",
            "@arguments.json",
            "--harn-output",
            "json",
        ]
    };
    let positive = run(root, &tool_args("registry.harn"), 0);
    assert_payload(
        &graph,
        &ir,
        &payload,
        &serde_json::from_slice(&positive.stdout).unwrap(),
    );
    assert_eq!(
        fs::read_to_string(root.join("calls.txt")).unwrap(),
        "echo\n"
    );
    let invalid = malformed_inputs(&payload);
    for (name, input) in &invalid {
        fs::write(
            root.join("bad.json"),
            serde_json::to_vec(&json!({"input":input})).unwrap(),
        )
        .unwrap();
        let output = run(
            root,
            &[
                "tool",
                "run",
                "registry.harn",
                "echo",
                "--harn-input",
                "@bad.json",
                "--harn-output",
                "json",
            ],
            1,
        );
        assert!(output.stdout.is_empty(), "{name}");
        assert_eq!(
            fs::read_to_string(root.join("calls.txt")).unwrap(),
            "echo\n",
            "{name} entered handler"
        );
    }
    let wrong = run(root, &tool_args("wrong-output.harn"), 1);
    assert!(wrong.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&wrong.stderr).contains(PRIVATE_OUTPUT));
    assert_eq!(
        fs::read_to_string(root.join("calls.txt")).unwrap(),
        "echo\necho\n"
    );

    for (file, surface, counted) in [
        ("source/server.harn", "exports", false),
        ("registry.harn", "script", true),
    ] {
        fs::write(root.join("calls.txt"), "").unwrap();
        let mut client = peer(root, file, surface);
        let positive = client.request(request(
            3,
            "tools/call",
            json!({"name":"echo","arguments":{"input":payload}}),
        ));
        assert_payload(
            &graph,
            &ir,
            &payload,
            &positive["result"]["structuredContent"],
        );
        for (index, (name, input)) in invalid.iter().enumerate() {
            let refused = client.request(request(
                index as i64 + 4,
                "tools/call",
                json!({"name":"echo","arguments":{"input":input}}),
            ));
            assert!(
                refused.get("error").is_some() || refused["result"]["isError"] == true,
                "{name}: {refused}"
            );
            assert!(
                refused["result"].get("structuredContent").is_none(),
                "{name}"
            );
            if counted {
                assert_eq!(
                    fs::read_to_string(root.join("calls.txt")).unwrap(),
                    "echo\n",
                    "{name} entered handler"
                );
            }
        }
        let final_positive = client.request(request(
            20,
            "tools/call",
            json!({"name":"echo","arguments":{"input":payload}}),
        ));
        assert_payload(
            &graph,
            &ir,
            &payload,
            &final_positive["result"]["structuredContent"],
        );
        if counted {
            assert_eq!(
                fs::read_to_string(root.join("calls.txt")).unwrap(),
                "echo\necho\n"
            );
        }
        client.shutdown_expect_success();
    }
    fs::write(root.join("calls.txt"), "").unwrap();
    let mut client = peer(root, "wrong-output.harn", "script");
    let wrong = client.request(request(
        3,
        "tools/call",
        json!({"name":"echo","arguments":{"input":payload}}),
    ));
    assert_eq!(wrong["result"]["isError"], true);
    assert!(wrong["result"].get("structuredContent").is_none());
    assert!(!wrong.to_string().contains(PRIVATE_OUTPUT));
    assert_eq!(
        fs::read_to_string(root.join("calls.txt")).unwrap(),
        "echo\n"
    );
    client.shutdown_expect_success();
}
