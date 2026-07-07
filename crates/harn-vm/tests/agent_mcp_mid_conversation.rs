#![recursion_limit = "256"]
//! Coverage for mid-conversation MCP mounting (`agent_mcp_mount_additional`).
//!
//! When a skill activates mid-conversation and declares MCP servers, the agent
//! loop mounts ONLY the servers not already active (the delta) so their
//! `server__tool` entries become visible in the catalog AND callable via the
//! execution-policy tool ceiling — without re-connecting a live server or
//! duplicating tools/ceiling entries.
//!
//! These tests drive the real `std/agent/mcp` primitives with a spec-aware
//! stub for the runtime-only `__host_mcp_bootstrap` host builtin: the stub
//! echoes one `<name>__tool` per requested spec and one server_info per spec,
//! so the harn side can observe exactly which servers were (re)bootstrapped.

use std::collections::BTreeMap;
use std::sync::Arc;

use harn_vm::value::{VmError, VmValue};

/// Run a harn program with a spec-aware `__host_mcp_bootstrap` stub. For every
/// spec passed, the stub returns a flat tool `{name: "<server>__tool"}` and a
/// `server_info` entry `{name: "<server>"}`. Records the server names each
/// bootstrap call received into `[harn] bootstrapped=<names>` so the test can
/// prove the delta (and only the delta) was mounted.
fn run_with_spec_aware_stub(source: &str) -> Result<Vec<String>, String> {
    harn_vm::reset_thread_local_state();
    let chunk = harn_vm::compile_source(source)?;
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
                vm.register_builtin("__host_mcp_bootstrap", move |args, out| {
                    let specs = match args.get(1) {
                        Some(VmValue::List(list)) => list.clone(),
                        _ => Arc::new(Vec::new()),
                    };
                    let mut names: Vec<String> = Vec::new();
                    let mut tools: Vec<VmValue> = Vec::new();
                    let mut server_info: Vec<VmValue> = Vec::new();
                    for spec in specs.iter() {
                        let name = spec
                            .as_dict()
                            .and_then(|d| d.get("name"))
                            .map(VmValue::display)
                            .unwrap_or_default();
                        if name.is_empty() {
                            continue;
                        }
                        names.push(name.clone());
                        let mut tool: BTreeMap<String, VmValue> = BTreeMap::new();
                        tool.insert("name".to_string(), VmValue::string(format!("{name}__tool")));
                        tools.push(VmValue::dict(tool));
                        let mut info: BTreeMap<String, VmValue> = BTreeMap::new();
                        info.insert("name".to_string(), VmValue::string(&name));
                        server_info.push(VmValue::dict(info));
                    }
                    // Surface which servers this call bootstrapped so the test
                    // can assert delta-only behavior across mounts.
                    out.push_str(&format!("[harn] bootstrapped={}\n", names.join(",")));
                    let mut result: BTreeMap<String, VmValue> = BTreeMap::new();
                    result.insert("tools_added".to_string(), VmValue::List(Arc::new(tools)));
                    result.insert(
                        "server_info".to_string(),
                        VmValue::List(Arc::new(server_info)),
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

fn field<'a>(lines: &'a [String], key: &str) -> &'a str {
    lines
        .iter()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .unwrap_or("<missing>")
}

fn all_fields<'a>(lines: &'a [String], key: &str) -> Vec<&'a str> {
    lines
        .iter()
        .filter_map(|l| l.strip_prefix(&format!("{key}=")))
        .collect()
}

/// Simulate the loop: bootstrap the initially-configured server, then a skill
/// activates mid-conversation declaring servers `[alpha, beta]`. Only `beta`
/// is new, so only it is bootstrapped, and both servers' tools end up visible
/// and callable.
#[test]
fn mid_conversation_mount_bootstraps_only_the_delta() {
    let source = r#"
import { agent_mcp_bootstrap_if_needed, agent_mcp_mount_additional } from "std/agent/mcp"
pipeline main(task) {
  const session = {session_id: "sess-midconv"}
  // Initial loop-entry bootstrap of the pre-configured server `alpha`.
  const opts = {
    mcp_servers: [{name: "alpha", command: "true"}],
    tools: {_type: "tool_registry", tools: [{name: "look"}]},
    policy: {tools: ["look"]},
  }
  const booted = agent_mcp_bootstrap_if_needed(session, opts)

  // A skill activates mid-conversation declaring `alpha` (already mounted)
  // and `beta` (new). Mount only the delta.
  const skill_specs = [{name: "alpha", command: "true"}, {name: "beta", command: "true"}]
  const out = agent_mcp_mount_additional(session, booted, skill_specs)

  const catalog = (out?.tools?.tools ?? []).map({ t -> to_string(t?.name ?? "") })
  const ceiling = out?.policy?.tools ?? []
  log("catalog=" + join(catalog, ","))
  log("ceiling=" + join(ceiling, ","))
  log("delta_nonnil=" + to_string(out?._mcp_delta_bootstrap != nil))
  log("server_ids=" + join((out?._mcp_server_info ?? []).map({ i -> to_string(i?.name ?? "") }), ","))
}
"#;
    let lines = run_with_spec_aware_stub(source).expect("snippet runs");

    // Two bootstrap calls: the initial `alpha`, then the delta `beta` only.
    let boots = all_fields(&lines, "bootstrapped");
    assert_eq!(
        boots,
        vec!["alpha", "beta"],
        "initial bootstrap mounts alpha; mid-conversation mount adds ONLY the delta beta"
    );

    // Both servers' tools are in the catalog (model can see them), no dup alpha.
    let catalog = field(&lines, "catalog");
    assert_eq!(
        catalog, "look,alpha__tool,beta__tool",
        "catalog holds host tool + both MCP tools once each: {catalog}"
    );

    // Both are admitted into the non-empty ceiling (model can call them).
    let ceiling = field(&lines, "ceiling");
    assert_eq!(
        ceiling, "look,alpha__tool,beta__tool",
        "ceiling extended with both MCP tools, host tool preserved: {ceiling}"
    );

    assert_eq!(field(&lines, "delta_nonnil"), "true");
    assert_eq!(field(&lines, "server_ids"), "alpha,beta");
}

/// Re-mounting a server that is already active is a no-op: no re-bootstrap, no
/// duplicate tools/ceiling entries, and `_mcp_delta_bootstrap` is nil.
#[test]
fn mounting_already_active_server_is_a_noop() {
    let source = r#"
import { agent_mcp_bootstrap_if_needed, agent_mcp_mount_additional } from "std/agent/mcp"
pipeline main(task) {
  const session = {session_id: "sess-noop"}
  const opts = {
    mcp_servers: [{name: "alpha", command: "true"}],
    tools: {_type: "tool_registry", tools: [{name: "look"}]},
    policy: {tools: ["look"]},
  }
  const booted = agent_mcp_bootstrap_if_needed(session, opts)
  const out = agent_mcp_mount_additional(session, booted, [{name: "alpha", command: "true"}])

  const catalog = (out?.tools?.tools ?? []).map({ t -> to_string(t?.name ?? "") })
  const ceiling = out?.policy?.tools ?? []
  log("catalog=" + join(catalog, ","))
  log("ceiling=" + join(ceiling, ","))
  log("delta_nonnil=" + to_string(out?._mcp_delta_bootstrap != nil))
}
"#;
    let lines = run_with_spec_aware_stub(source).expect("snippet runs");

    // Only the initial bootstrap ran — the second mount found nothing new.
    let boots = all_fields(&lines, "bootstrapped");
    assert_eq!(
        boots,
        vec!["alpha"],
        "already-active server must not be re-bootstrapped"
    );
    assert_eq!(
        field(&lines, "catalog"),
        "look,alpha__tool",
        "no duplicate alpha__tool"
    );
    assert_eq!(
        field(&lines, "ceiling"),
        "look,alpha__tool",
        "no duplicate ceiling entry"
    );
    assert_eq!(field(&lines, "delta_nonnil"), "false");
}

/// An empty (open) ceiling stays open even when mid-conversation mounting adds
/// tools — mirrors the initial-bootstrap invariant.
#[test]
fn mid_conversation_mount_keeps_open_ceiling_open() {
    let source = r#"
import { agent_mcp_mount_additional } from "std/agent/mcp"
pipeline main(task) {
  const session = {session_id: "sess-open"}
  const opts = {tools: {_type: "tool_registry", tools: [{name: "look"}]}, policy: {tools: []}}
  const out = agent_mcp_mount_additional(session, opts, [{name: "beta", command: "true"}])
  log("catalog=" + join((out?.tools?.tools ?? []).map({ t -> to_string(t?.name ?? "") }), ","))
  log("ceiling_len=" + to_string(len(out?.policy?.tools ?? [])))
}
"#;
    let lines = run_with_spec_aware_stub(source).expect("snippet runs");
    assert_eq!(
        field(&lines, "catalog"),
        "look,beta__tool",
        "tool still enters the catalog under an open policy"
    );
    assert_eq!(
        field(&lines, "ceiling_len"),
        "0",
        "open (empty) ceiling must stay open"
    );
}
