//! Mechanism-fitness coverage for `agent_mcp_bootstrap_if_needed`'s tool-
//! ceiling admission (the "Burin agent can USE an MCP server's tools" fix).
//!
//! Before this fix, bootstrapping an MCP server merged its `server__tool`
//! entries into the agent's tool CATALOG (`opts.tools`) — so the model could
//! SEE them — but never into the execution-policy tool CEILING
//! (`opts.policy.tools`). Every dispatch was then denied with
//! "exceeds tool ceiling". These tests drive `agent_mcp_bootstrap_if_needed`
//! with a stubbed `__host_mcp_bootstrap` and assert that:
//!   (a) a non-empty ceiling is EXTENDED with the bootstrapped tool names
//!       (so the tool becomes callable), and is idempotent;
//!   (b) an EMPTY ceiling (== "no ceiling, allow any tool") is left empty —
//!       we must not flip an open policy closed and forbid the host's own
//!       tools;
//!   (c) when no MCP servers are configured the opts are returned unchanged.

use std::collections::BTreeMap;
use std::sync::Arc;

use harn_vm::value::{VmError, VmValue};

/// Run a harn program with a stubbed `__host_mcp_bootstrap` that returns the
/// given `tools_added` names (each as a flat `{name: ...}` tool descriptor,
/// the shape `tag_mcp_tool` emits). Returns the `[harn] `-prefixed log lines.
fn run_with_stub(source: &str, tools_added: &[&str]) -> Result<Vec<String>, String> {
    harn_vm::reset_thread_local_state();
    let chunk = harn_vm::compile_source(source)?;
    let tools: Vec<VmValue> = tools_added
        .iter()
        .map(|name| {
            let mut dict: BTreeMap<String, VmValue> = BTreeMap::new();
            dict.insert("name".to_string(), VmValue::string(*name));
            VmValue::dict(dict)
        })
        .collect();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                // Override the runtime-only host builtin with a deterministic
                // stub so the test never opens a real MCP connection.
                let stub_tools = Arc::new(tools);
                vm.register_builtin("__host_mcp_bootstrap", move |_args, _out| {
                    let mut result: BTreeMap<String, VmValue> = BTreeMap::new();
                    result.insert("tools_added".to_string(), VmValue::List(stub_tools.clone()));
                    result.insert(
                        "server_info".to_string(),
                        VmValue::List(Arc::new(Vec::new())),
                    );
                    result.insert("errors".to_string(), VmValue::List(Arc::new(Vec::new())));
                    Ok(VmValue::dict(result))
                });
                vm.execute(&chunk)
                    .await
                    .map_err(|e: VmError| format!("{e:?}"))?;
                Ok(vm.output().to_string())
            })
            .await
    })
    .map(|raw| {
        raw.lines()
            .filter_map(|l| l.strip_prefix("[harn] ").map(str::to_string))
            .collect()
    })
}

/// Harn snippet: bootstrap one MCP server and echo the resulting catalog tool
/// names and the policy ceiling, so the Rust side can assert on both.
fn bootstrap_snippet(mcp_servers: &str, policy: &str) -> String {
    format!(
        r#"
import {{ agent_mcp_bootstrap_if_needed }} from "std/agent/mcp"
pipeline main(task) {{
  const session = {{session_id: "sess-mcp-ceiling"}}
  const opts = {{
    mcp_servers: {mcp_servers},
    tools: {{_type: "tool_registry", tools: [{{name: "look"}}]}},
    policy: {policy},
  }}
  const out = agent_mcp_bootstrap_if_needed(session, opts)
  const catalog = (out?.tools?.tools ?? []).map({{ t -> to_string(t?.name ?? "") }})
  const ceiling = out?.policy?.tools ?? []
  log("catalog=" + join(catalog, ","))
  log("ceiling=" + join(ceiling, ","))
  log("ceiling_type=" + type_of(out?.policy?.tools))
}}
"#
    )
}

fn field<'a>(lines: &'a [String], key: &str) -> &'a str {
    lines
        .iter()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .unwrap_or("<missing>")
}

#[test]
fn nonempty_ceiling_admits_bootstrapped_mcp_tools() {
    let lines = run_with_stub(
        &bootstrap_snippet(
            r#"[{name: "burin-harness-debugger"}]"#,
            r#"{tools: ["look", "search"]}"#,
        ),
        &[
            "burin-harness-debugger__list_runs",
            "burin-harness-debugger__trial_failure_cause",
        ],
    )
    .expect("snippet runs");

    // Catalog gained the MCP tools (model can see them).
    let catalog = field(&lines, "catalog");
    assert!(
        catalog.contains("burin-harness-debugger__list_runs"),
        "MCP tool must enter the catalog: {catalog}"
    );

    // CEILING gained them too (model can now call them — the fix).
    let ceiling = field(&lines, "ceiling");
    assert!(
        ceiling.contains("burin-harness-debugger__list_runs"),
        "MCP tool must be admitted into the tool ceiling: {ceiling}"
    );
    assert!(
        ceiling.contains("burin-harness-debugger__trial_failure_cause"),
        "every bootstrapped MCP tool must be admitted: {ceiling}"
    );
    // Host's own tools are preserved, not replaced.
    assert!(ceiling.contains("look"), "host tools preserved: {ceiling}");
    assert!(
        ceiling.contains("search"),
        "host tools preserved: {ceiling}"
    );
}

#[test]
fn empty_ceiling_stays_open() {
    // An empty `policy.tools` means "no ceiling". Adding names would flip it
    // closed and silently forbid the host's own tools — so it must stay empty.
    let lines = run_with_stub(
        &bootstrap_snippet(r#"[{name: "burin-harness-debugger"}]"#, r"{tools: []}"),
        &["burin-harness-debugger__list_runs"],
    )
    .expect("snippet runs");

    assert_eq!(
        field(&lines, "ceiling"),
        "",
        "open (empty) ceiling must stay open"
    );
    // Catalog still gains the tool regardless, so it stays callable under the
    // open policy.
    assert!(field(&lines, "catalog").contains("burin-harness-debugger__list_runs"));
}

#[test]
fn no_mcp_servers_is_a_noop() {
    let lines = run_with_stub(
        &bootstrap_snippet("[]", r#"{tools: ["look", "search"]}"#),
        &["burin-harness-debugger__list_runs"],
    )
    .expect("snippet runs");

    // No servers configured -> bootstrap never ran -> ceiling untouched.
    assert_eq!(field(&lines, "ceiling"), "look,search");
    assert_eq!(field(&lines, "catalog"), "look");
}
