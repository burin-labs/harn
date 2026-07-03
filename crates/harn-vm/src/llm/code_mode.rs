//! Code mode: run a short script that composes tools as a typed API instead
//! of emitting N separate tool calls (the CodeAct / Cloudflare Code-Mode
//! pattern). The win is composition — chain connector/MCP tools, keep the
//! intermediate data OUT of the model's context, and return only the composed
//! result.
//!
//! Security model (all four properties are enforced structurally, not by
//! convention):
//!
//! 1. **Restricted capability set.** The script runs in a *fresh* sandbox VM
//!    that has ONLY the pure composition stdlib registered
//!    ([`crate::register_core_stdlib`]: strings/json/lists/math/regex/etc.).
//!    None of the I/O builtins (`fs`, `net`, `process`, `host_call`, `llm`,
//!    `secret_store`, hostlib) are installed, so the script literally cannot
//!    name a builtin that touches the filesystem, the network, or a
//!    subprocess. Its only channel to the outside world is the single
//!    `call_tool` binding below. A name-based denylist backstops any future
//!    core addition that might leak an I/O surface.
//!
//! 2. **Tool calls stay gated.** `call_tool` routes every invocation through
//!    the unchanged
//!    [`super::agent_host_primitives::host_agent_dispatch_tool_call`] — the
//!    same choke-point the model's own tool calls use. That applies the
//!    execution-policy ceilings, dynamic permissions, the approval floor
//!    (`session/request_permission`), and the lethal-trifecta/taint upgrade
//!    before anything executes. A code-mode script therefore runs with a
//!    capability that is provably ≤ the model's own: it inherits the parent
//!    turn's ambient policy + approval floor and cannot self-grant.
//!
//! 3. **Credentials via bindings only.** MCP tool calls are dispatched by name
//!    through the gate, which resolves the live authorized client from the
//!    session registry (keyed by `session_id`). The auth token lives inside
//!    the Rust `HttpMcpClientInner` and is never materialized as a Harn value,
//!    so it can never enter the script text or the script's reachable scope.
//!    The script holds a capability *handle* (the tool name), not the key.
//!
//! 4. **Intermediate values stay out of context.** Every `let` in the script
//!    is a value in the sandbox VM's scope. Only the value the script
//!    `return`s crosses back out as the code-mode tool result; the raw
//!    tool outputs consumed mid-script never reach the model transcript.
//!    Untrusted results are still subject to taint because they flow through
//!    the same gated dispatch.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::stdlib::macros::harn_builtin;
use crate::value::{DictMap, VmError, VmValue};
use crate::vm::{AsyncBuiltinCtx, VmBuiltinMetadata};

/// Builtin names in the pure composition stdlib that we defensively deny in
/// the sandbox VM. `register_core_stdlib` is I/O-free today, so this is a
/// backstop against a future core addition accidentally exposing a network /
/// process / host / secret surface to untrusted code-mode scripts. Matched as
/// substrings against every registered builtin name.
const SANDBOX_DENY_SUBSTRINGS: &[&str] = &[
    "host_call",
    "process",
    "spawn",
    "exec",
    "shell",
    "socket",
    "fetch",
    "secret",
    "keychain",
    "hostlib_",
    "llm_call",
    "__host_",
];

/// Run a code-mode script.
///
/// `config`:
/// - `code` (string, required): the Harn script body. Statements plus a final
///   `return <value>`; that returned value becomes the tool result.
/// - `tools` (dict, optional): the tool registry the script may dispatch
///   against. Defaults to the current session registry.
/// - `options` (dict, optional): agent options (carries `session_id` etc.) so
///   dispatched calls resolve the right session policy + MCP clients.
/// - `allow_tools` (list<string>, optional): if present, restricts which tool
///   names the script may call to this allowlist (the declared connector set).
#[harn_builtin(
    sig = "__host_code_mode_run(config: dict) -> dict",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_code_mode_run_impl(
    ctx: AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let config = match args.into_iter().next() {
        Some(VmValue::Dict(config)) => config,
        _ => {
            return Err(VmError::Runtime(
                "__host_code_mode_run(config): config must be a dict".to_string(),
            ))
        }
    };

    let code = match config.get("code") {
        Some(VmValue::String(code)) => code.to_string(),
        _ => {
            return Err(VmError::Runtime(
                "__host_code_mode_run: config.code (string) is required".to_string(),
            ))
        }
    };

    let tools = match config.get("tools") {
        Some(value @ VmValue::Dict(_)) => Some(value.clone()),
        _ => crate::stdlib::tools::current_tool_registry(),
    };

    let options: DictMap = match config.get("options") {
        Some(VmValue::Dict(options)) => options.as_ref().clone(),
        _ => DictMap::new(),
    };

    let allow_tools: Option<BTreeSet<String>> = match config.get("allow_tools") {
        Some(VmValue::List(names)) => Some(names.iter().map(|value| value.display()).collect()),
        _ => None,
    };

    run_code_mode(ctx, code, tools, options, allow_tools).await
}

async fn run_code_mode(
    parent_ctx: AsyncBuiltinCtx,
    code: String,
    tools: Option<VmValue>,
    options: DictMap,
    allow_tools: Option<BTreeSet<String>>,
) -> Result<VmValue, VmError> {
    let mut sandbox = crate::Vm::new();
    crate::register_core_stdlib(&mut sandbox);

    // Defense in depth: deny any pure-stdlib builtin whose name pattern-matches
    // an I/O family, so a future core addition cannot silently widen the
    // sandbox surface. `call_tool` (registered below) is the only sanctioned
    // egress.
    let denied: std::collections::HashSet<String> = sandbox
        .builtin_names()
        .into_iter()
        .filter(|name| {
            SANDBOX_DENY_SUBSTRINGS
                .iter()
                .any(|needle| name.contains(needle))
        })
        .collect();
    sandbox.set_denied_builtins(denied);

    let tool_calls = Arc::new(AtomicUsize::new(0));

    register_call_tool(
        &mut sandbox,
        parent_ctx,
        tools,
        options,
        allow_tools,
        Arc::clone(&tool_calls),
    );

    // Wrap the script body in an entry pipeline so `execute` returns the value
    // the script `return`s. The body runs with the sandbox's restricted
    // builtin set; there is no path to the host except `call_tool`.
    let source = format!("pipeline __code_mode_entry() {{\n{code}\n}}\n");
    let chunk = crate::compile_source(&source)
        .map_err(|error| VmError::Runtime(format!("code-mode script did not compile: {error}")))?;

    let value = sandbox.execute(&chunk).await?;

    Ok(VmValue::dict([
        ("ok", VmValue::Bool(true)),
        ("value", value),
        (
            "tool_calls",
            VmValue::Int(tool_calls.load(Ordering::Relaxed) as i64),
        ),
    ]))
}

/// Register the single `call_tool(name, args)` binding on the sandbox VM. It
/// is the sandbox's only channel to the host: it routes through the same gated
/// dispatch the model's own tool calls use, so policy + approval + MCP
/// credential resolution all apply, and returns the tool's raw structured
/// result for further composition (or throws on denial/failure).
fn register_call_tool(
    sandbox: &mut crate::Vm,
    parent_ctx: AsyncBuiltinCtx,
    tools: Option<VmValue>,
    options: DictMap,
    allow_tools: Option<BTreeSet<String>>,
    tool_calls: Arc<AtomicUsize>,
) {
    let tools = Arc::new(tools);
    let options = Arc::new(options);
    let allow_tools = Arc::new(allow_tools);

    let metadata = VmBuiltinMetadata::async_builtin("call_tool");
    sandbox.register_async_builtin_with_metadata(metadata, move |_sandbox_ctx, args| {
        // Route against the PARENT ctx, never the sandbox ctx: any Harn-side
        // tool handler closure is owned by the parent VM and must execute
        // there, and the ambient bridge/session/policy scopes belong to the
        // parent turn.
        let parent_ctx = parent_ctx.clone();
        let tools = Arc::clone(&tools);
        let options = Arc::clone(&options);
        let allow_tools = Arc::clone(&allow_tools);
        let tool_calls = Arc::clone(&tool_calls);
        async move {
            let mut args = args.into_iter();
            let name = match args.next() {
                Some(VmValue::String(name)) => name.to_string(),
                Some(other) => {
                    return Err(VmError::Runtime(format!(
                        "call_tool(name, args): name must be a string; got {}",
                        other.type_name()
                    )))
                }
                None => {
                    return Err(VmError::Runtime(
                        "call_tool(name, args): missing tool name".to_string(),
                    ))
                }
            };
            let tool_args = args.next().unwrap_or(VmValue::Nil);

            if let Some(allow) = allow_tools.as_ref() {
                if !allow.contains(&name) {
                    return Err(VmError::Runtime(format!(
                        "call_tool: '{name}' is not in this code-mode script's allowed tool set"
                    )));
                }
            }

            tool_calls.fetch_add(1, Ordering::Relaxed);

            let call = VmValue::dict([
                ("name", VmValue::String(arcstr::ArcStr::from(name.as_str()))),
                ("arguments", tool_args),
            ]);

            let envelope = super::agent_host_primitives::host_agent_dispatch_tool_call(
                parent_ctx,
                call,
                tools.as_ref().as_ref(),
                options.as_ref(),
            )
            .await?;

            unwrap_tool_envelope(&name, envelope)
        }
    });
}

/// Extract the raw structured result from a dispatch envelope, or turn a
/// denied/failed dispatch into a thrown error the script can `try`/`catch`.
fn unwrap_tool_envelope(tool_name: &str, envelope: VmValue) -> Result<VmValue, VmError> {
    let dict = match &envelope {
        VmValue::Dict(dict) => dict,
        // A non-dict envelope is unexpected; hand it back verbatim rather than
        // masking it.
        _ => return Ok(envelope),
    };

    let ok = matches!(dict.get("ok"), Some(VmValue::Bool(true)));
    if ok {
        return Ok(dict.get("result").cloned().unwrap_or(VmValue::Nil));
    }

    let reason = match dict.get("error") {
        Some(VmValue::String(error)) if !error.is_empty() => error.to_string(),
        _ => dict
            .get("rendered_result")
            .map(|value| value.display())
            .unwrap_or_else(|| "tool call was denied".to_string()),
    };
    Err(VmError::Runtime(format!(
        "call_tool('{tool_name}') failed: {reason}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::VmValue;
    use std::future::Future;

    /// Drive an async body on a current-thread runtime inside a `LocalSet`, the
    /// harness the VM's `execute` path expects (it may `spawn_local`).
    fn block_on_local<F: Future>(future: F) -> F::Output {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime");
        let local = tokio::task::LocalSet::new();
        rt.block_on(local.run_until(future))
    }

    async fn secret_tool_registry() -> VmValue {
        // A fake connector tool whose Harn-closure handler closes over a
        // secret credential and returns only public data. Mirrors the shape of
        // a bound MCP client: the credential is held by the host, and the
        // script sees a capability handle (the tool name), never the key.
        // The handler returns its payload as JSON text — the shape a real MCP
        // text-content connector returns — so the sandbox script parses it with
        // `json_parse` and composes over the structured result. The credential
        // is closed over by the host-side handler and never leaves it.
        let source = r#"
pipeline main() {
  let handler = fn(args) {
    let _credential = "SECRET-TOKEN-do-not-leak"
    return json_stringify({ records: [{ id: 1, title: args.q + "-alpha" }, { id: 2, title: args.q + "-beta" }] })
  }
  return { tools: [{ name: "connector_read", handler: handler }] }
}
"#;
        let chunk = crate::compile_source(source).expect("compile registry source");
        let mut vm = crate::Vm::new();
        crate::register_vm_stdlib(&mut vm);
        vm.execute(&chunk).await.expect("run registry source")
    }

    #[test]
    fn composes_tool_results_and_returns_only_the_summary() {
        let out = block_on_local(async {
            let tools = secret_tool_registry().await;
            // Call the connector twice, compose across both results, return only
            // a small summary — the full record payloads stay in the sandbox.
            let code = r#"
let first = json_parse(call_tool("connector_read", { q: "one" }))
let second = json_parse(call_tool("connector_read", { q: "two" }))
let total = len(first.records) + len(second.records)
return { total: total, first_title: first.records[0].title }
"#;
            let mut vm = crate::Vm::new();
            crate::register_vm_stdlib(&mut vm);
            let ctx = AsyncBuiltinCtx::for_test(vm);
            run_code_mode(ctx, code.to_string(), Some(tools), DictMap::new(), None)
                .await
                .expect("code-mode run")
        });

        let VmValue::Dict(out) = out else {
            panic!("expected dict output");
        };

        // (a) Composition is correct: 2 + 2 records, title carries the arg.
        let value = out.get("value").expect("value");
        let VmValue::Dict(value) = value else {
            panic!("expected value dict");
        };
        assert_eq!(value.get("total").and_then(|v| v.as_int()), Some(4));
        assert_eq!(
            value.get("first_title").map(|v| v.display()),
            Some("one-alpha".to_string())
        );
        assert_eq!(out.get("tool_calls").and_then(|v| v.as_int()), Some(2));

        // (b) Intermediate values did not leak: the returned surface names
        // only the summary keys, and the raw per-record payloads (`beta`, the
        // `records` list) are absent from what re-enters the model context.
        let rendered = format!("{:?}", out.get("value"));
        assert!(
            !rendered.contains("beta") && !rendered.contains("records"),
            "intermediate record payloads must not appear in the returned value: {rendered}"
        );

        // (d) The credential never became a Harn value reachable by the
        // script, so it cannot appear in the returned output.
        assert!(
            !rendered.contains("SECRET-TOKEN"),
            "credential must never be visible to the script: {rendered}"
        );
    }

    #[test]
    fn script_tool_calls_route_through_the_policy_gate() {
        use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};

        let err = block_on_local(async {
            // A tool ceiling that excludes `connector_read` — the same execution
            // policy that gates the model's own tool calls. A code-mode script
            // must not slip past it, proving script calls hit the identical gate.
            push_execution_policy(CapabilityPolicy {
                tools: vec!["some_other_tool".to_string()],
                ..Default::default()
            });
            let tools = secret_tool_registry().await;
            let mut vm = crate::Vm::new();
            crate::register_vm_stdlib(&mut vm);
            let ctx = AsyncBuiltinCtx::for_test(vm);
            let result = run_code_mode(
                ctx,
                r#"return call_tool("connector_read", { q: "x" })"#.to_string(),
                Some(tools),
                DictMap::new(),
                None,
            )
            .await;
            pop_execution_policy();
            result.expect_err("call outside the tool ceiling must be denied by the gate")
        });

        let rendered = format!("{err:?}").to_lowercase();
        assert!(
            rendered.contains("ceiling")
                || rendered.contains("not permitted")
                || rendered.contains("failed"),
            "expected a policy-gate denial, got: {err:?}"
        );
    }

    #[test]
    fn sandbox_core_stdlib_has_no_io_builtins() {
        // The sandbox VM must not expose filesystem / network / process /
        // secret / host builtins — the script's only egress is `call_tool`.
        let mut sandbox = crate::Vm::new();
        crate::register_core_stdlib(&mut sandbox);
        let names = sandbox.builtin_names();
        for banned in [
            "read_file",
            "host_call",
            "run_command",
            "hostlib_secret_store_get",
        ] {
            assert!(
                !names.iter().any(|name| name == banned),
                "sandbox core stdlib unexpectedly exposes I/O builtin {banned}"
            );
        }
    }

    #[test]
    fn allowlist_blocks_tools_outside_the_declared_connector_set() {
        let err = block_on_local(async {
            let tools = secret_tool_registry().await;
            let allow: BTreeSet<String> = ["something_else".to_string()].into_iter().collect();
            let mut vm = crate::Vm::new();
            crate::register_vm_stdlib(&mut vm);
            let ctx = AsyncBuiltinCtx::for_test(vm);
            run_code_mode(
                ctx,
                r#"return call_tool("connector_read", { q: "x" })"#.to_string(),
                Some(tools),
                DictMap::new(),
                Some(allow),
            )
            .await
            .expect_err("call outside allowlist must fail")
        });
        assert!(
            format!("{err:?}").contains("allowed tool set"),
            "expected allowlist denial, got: {err:?}"
        );
    }
}
