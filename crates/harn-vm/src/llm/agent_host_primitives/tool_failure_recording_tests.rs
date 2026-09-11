//! A Harn tool handler that returns a plain dict describing failure — never a
//! throw — must be recorded as a failed call, and the failure text must reach
//! the transcript the next turn reads.
//!
//! Before harn#7893, `carries_typed_outcome` only recognized a typed struct
//! return, so a plain `{ok: false, error: "..."}` dict was display-stringified
//! before the failure detector ever saw it, and the detector was then handed
//! unparseable prose instead of JSON. Every non-throwing failure — `{ok:
//! false}`, `{success: false}`, `{status: "error"}`, `{isError: true}` (the
//! MCP shape) — read as a success, and only an explicit `throw` recorded a
//! failure. This exercises the real dispatch entry point end to end (not just
//! the classifier unit tests in `agent_tools/handler_result.rs`), through a
//! compiled Harn handler closure exactly the way a `tool_define`d tool
//! dispatches, so a regression anywhere between classification and the
//! recorded tool-result envelope fails here.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::host_agent_dispatch_tool_call;
use crate::value::{DictMap, VmClosure, VmEnv, VmValue};

struct PipelineInputBridge;

struct ConsentBridge(Arc<AtomicUsize>);

impl crate::HostCallBridge for ConsentBridge {
    fn dispatch<'a>(
        &'a self,
        capability: &'a str,
        operation: &'a str,
        _params: &'a DictMap,
    ) -> crate::HostCallDispatchFuture<'a> {
        assert_eq!((capability, operation), ("permission", "request"));
        self.0.fetch_add(1, Ordering::SeqCst);
        crate::host_call_ready(Ok(Some(VmValue::dict([("approved", VmValue::Bool(true))]))))
    }
}

impl crate::HostCallBridge for PipelineInputBridge {
    fn dispatch<'a>(
        &'a self,
        capability: &'a str,
        operation: &'a str,
        _params: &'a DictMap,
    ) -> crate::HostCallDispatchFuture<'a> {
        crate::host_call_ready(Ok(((capability, operation)
            == ("runtime", "pipeline_input"))
            .then(|| VmValue::String("pipeline-value".into()))))
    }
}

/// Compile one Harn function into a callable closure, the same way a
/// `tool_define`d handler is compiled from pipeline source.
fn compiled_closure(name: &str, source: &str) -> Arc<VmClosure> {
    compiled_closure_with(name, source, crate::compiler::Compiler::new())
}

fn compiled_trusted_closure(name: &str, source: &str) -> Arc<VmClosure> {
    compiled_closure_with(
        name,
        source,
        crate::compiler::Compiler::new_trusted_host_dispatch(),
    )
}

fn compiled_closure_with(
    name: &str,
    source: &str,
    compiler: crate::compiler::Compiler,
) -> Arc<VmClosure> {
    let program = harn_parser::check_source_strict(source).expect("handler source parses");
    let chunk = compiler.compile(&program).expect("handler source compiles");
    let function = chunk
        .functions
        .iter()
        .find(|function| function.name.as_str() == name)
        .expect("compiled handler function")
        .clone();
    Arc::new(VmClosure {
        func: function,
        env: VmEnv::new(),
        source_dir: None,
        module_functions: None,
        module_state: None,
        retained_module_scope: None,
    })
}

/// One `tools` value declaring a single Harn-handled tool by name, the same
/// shape `find_tool_handler` reads: `{tools: [{name, handler}]}`.
fn tools_with_handler(tool_name: &str, handler: Arc<VmClosure>) -> VmValue {
    VmValue::dict([(
        "tools",
        VmValue::List(Arc::new(vec![VmValue::dict([
            ("name", VmValue::string(tool_name)),
            ("handler", VmValue::Closure(handler)),
        ])])),
    )])
}

async fn dispatch(tool_name: &str, handler_source: &str) -> serde_json::Value {
    let handler = compiled_closure("handler", handler_source);
    let tools = tools_with_handler(tool_name, handler);
    let call = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "id": "non-throwing-failure-1",
        "name": tool_name,
        "arguments": {},
    }));
    let result = host_agent_dispatch_tool_call(
        crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new()),
        call,
        Some(&tools),
        &DictMap::new(),
    )
    .await
    .expect("dispatch returns a value for a handler that returns rather than throws");
    crate::llm::helpers::vm_value_to_json(&result)
}

async fn dispatch_trusted(tool_name: &str, handler_source: &str) -> serde_json::Value {
    let handler = compiled_trusted_closure("handler", handler_source);
    let tools = tools_with_handler(tool_name, handler);
    let call = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "id": "trusted-handler-1",
        "name": tool_name,
        "arguments": {},
    }));
    let mut vm = crate::vm::Vm::new();
    crate::register_vm_stdlib(&mut vm);
    let result = host_agent_dispatch_tool_call(
        crate::vm::AsyncBuiltinCtx::for_test(vm),
        call,
        Some(&tools),
        &DictMap::new(),
    )
    .await
    .expect("dispatch returns a typed tool result");
    crate::llm::helpers::vm_value_to_json(&result)
}

/// The shape the maturity gate scored 0/3: a handler returns `{ok: false,
/// error: "..."}` without throwing. The dispatch record must say the call
/// failed, and the failure text must be what the next turn reads back.
#[tokio::test]
async fn a_non_throwing_ok_false_return_is_recorded_as_a_failed_call() {
    let result = dispatch(
        "flaky_write",
        r#"fn handler(request: dict) { return {ok: false, error: "disk full"} }"#,
    )
    .await;

    assert_eq!(
        result["ok"],
        serde_json::json!(false),
        "a handler-declared failure must not read as ok:true: {result}"
    );
    assert_eq!(
        result["status"],
        serde_json::json!("error"),
        "the recorded status must be error, not ok: {result}"
    );
    assert_eq!(
        result["error_category"],
        serde_json::json!("tool_error"),
        "the failure must be classified, not left unclassified: {result}"
    );
    let observation = result["observation"]
        .as_str()
        .expect("observation is the text the next turn reads");
    assert!(
        observation.contains("disk full"),
        "the failure text must reach the next turn's observation, got: {observation}"
    );
}

/// The MCP-shaped sibling (`isError: true` rather than `ok`/`success`), so the
/// fix is proven across the documented failure vocabulary, not one key.
#[tokio::test]
async fn an_is_error_true_return_is_recorded_as_a_failed_call() {
    let result = dispatch(
        "flaky_mcp_call",
        r#"fn handler(request: dict) { return {isError: true, message: "server unavailable"} }"#,
    )
    .await;

    assert_eq!(result["ok"], serde_json::json!(false), "{result}");
    assert_eq!(result["status"], serde_json::json!("error"), "{result}");
    let observation = result["observation"].as_str().expect("observation");
    assert!(
        observation.contains("server unavailable"),
        "got: {observation}"
    );
}

/// The negative control: an ordinary successful plain-dict return must not be
/// swept into failure by a classifier that got too eager.
#[tokio::test]
async fn a_non_throwing_ok_true_return_is_recorded_as_a_successful_call() {
    let result = dispatch(
        "reliable_write",
        r#"fn handler(request: dict) { return {ok: true, message: "wrote 12 bytes"} }"#,
    )
    .await;

    assert_eq!(result["ok"], serde_json::json!(true), "{result}");
    assert_eq!(result["status"], serde_json::json!("ok"), "{result}");
    assert_eq!(
        result["error_category"],
        serde_json::Value::Null,
        "{result}"
    );
}

/// A privileged wire is owned by the host-selected pipeline boundary, not by
/// a model-invoked handler. The same bridge remains serviceable outside the
/// handler, which keeps this from passing because the bridge was disconnected.
#[tokio::test(flavor = "current_thread")]
async fn host_call_inside_tool_handler_fails_loudly_while_pipeline_call_remains_serviceable() {
    let _bridge = crate::install_host_call_bridge(Arc::new(PipelineInputBridge));
    let direct = crate::dispatch_host_operation("runtime", "pipeline_input", &DictMap::new())
        .await
        .expect("the host-selected pipeline boundary reaches the host bridge");
    assert_eq!(direct.display(), "pipeline-value");

    let result = dispatch_trusted(
        "read_pipeline_input",
        r#"fn handler(request: dict) { return host_call("runtime.pipeline_input", {}) }"#,
    )
    .await;

    assert_eq!(result["ok"], serde_json::json!(false), "{result}");
    assert!(
        result["observation"]
            .as_str()
            .is_some_and(|text| text.contains("unavailable inside a tool handler")),
        "the refusal must distinguish an unserviceable host call from an empty host answer: {result}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn command_hook_consent_reaches_the_host_without_opening_other_handler_calls() {
    let calls = Arc::new(AtomicUsize::new(0));
    let _bridge = crate::install_host_call_bridge(Arc::new(ConsentBridge(calls.clone())));
    let source = r#"fn handler(request: dict) {
        const response = host_call("permission.request", {})
        return {ok: response.approved}
    }"#;

    let direct = dispatch_trusted("direct_consent", source).await;
    assert_eq!(
        direct["ok"], false,
        "a tool cannot ask on the policy's behalf: {direct}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    {
        let _hook = crate::orchestration::enter_command_policy_hook();
        let consent = dispatch_trusted("policy_consent", source).await;
        assert_eq!(
            consent["ok"], true,
            "the policy's consent call must reach the host: {consent}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let unrelated = dispatch_trusted(
            "pipeline_input",
            r#"fn handler(request: dict) { return host_call("runtime.pipeline_input", {}) }"#,
        )
        .await;
        assert_eq!(unrelated["ok"], false, "{unrelated}");
        assert!(
            unrelated["observation"]
                .as_str()
                .is_some_and(|text| text.contains("unavailable inside a tool handler")),
            "{unrelated}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    let after = dispatch_trusted("direct_consent_after_hook", source).await;
    assert_eq!(
        after["ok"], false,
        "the hook's consent access must unwind: {after}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
