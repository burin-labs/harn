use super::*;
use crate::codegen::compile_catalog;
use crate::tool_registry::{ToolCatalog, ToolInvocationOutcome};
use serde_json::json;
use std::collections::BTreeMap;

fn contract() -> ValidatedCodegenIr {
    let catalog: ToolCatalog = serde_json::from_value(json!({
        "schema_version":"harn-tools/2.0","info":{"name":"native-pair"},
        "tools":[{"name":"echo","inputSchema":{"type":"object","properties":{"value":{"type":"integer"}},"required":["value"],"additionalProperties":false},
            "outputSchema":{"type":"integer"},
            "errorSchema":{"type":"object","properties":{"kind":{"const":"rejected"},"code":{"type":"integer"}},"required":["kind","code"],"additionalProperties":false},
            "governance":{"audiences":["cli","mcp"]},"cli":{"command":["echo"],"aliases":[],"hidden":false,"arguments":{}},"deferLoading":false}]
    })).unwrap();
    compile_catalog(&PreparedToolCatalog::prepare(catalog).unwrap(), &[]).unwrap()
}

const WORKER: &str = r#"
import json, os, sys
profile=json.loads(os.environ['NATIVE_PROFILE'])
tools=json.loads(os.environ['NATIVE_TOOLS'])
calls=0
lists=0
held=None
held_second=None
cancelled=None
cancelled_ids=[]
def reply(request, result):
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
def complete(request, value, meta):
    reply(request, {'resultType':'complete','content':[], 'structuredContent':{'result':value}, '_meta':meta})
for line in sys.stdin:
    request=json.loads(line)
    method=request['method']
    if method=='server/discover':
        reply(request, {'resultType':'complete','supportedVersions':['2026-07-28'],'capabilities':{'tools':{}},'ttlMs':0,'cacheScope':'private',
            '_meta':{'io.modelcontextprotocol/serverInfo':{'name':'native-pair','version':'1'},'com.harnlang/nativeBinding':profile}})
    elif method=='tools/list':
        lists+=1
        reply(request, {'tools':tools,'_meta':{'calls':calls,'lists':lists}})
    elif method=='notifications/cancelled':
        request_id=request['params']['requestId']
        if held is not None and request_id==held['id']:
            cancelled=held['id']
            held=None
        else:
            assert held_second is not None and request_id==held_second['id']
            held_second=None
        cancelled_ids.append(request_id)
    elif method=='tools/call':
        assert request['params']['name']=='echo'
        calls+=1
        value=request['params']['arguments']['value']
        if value==1:
            assert held is None
            held=request
        elif value==2:
            assert held is not None, 'second call must overlap the held first call'
            complete(request,9223372036854775807,{'calls':calls,'first_id':held['id'],'second_id':request['id'],'full_width':9223372036854775807})
        elif value==3:
            assert cancelled is not None and held is None
            complete(request,3,{'cancelled_id':cancelled,'calls':calls,'lists':lists})
        elif value==4:
            print(json.dumps({'jsonrpc':'2.0','id':request['id']+100,'result':{'content':[],'structuredContent':{'result':4}}}),flush=True)
            break
        elif value==5:
            reply(request,{'resultType':'complete','content':[],'isError':True,'_meta':{'com.harnlang/toolContract':{'applicationError':{'tool':'echo','data':{'kind':'rejected','code':9223372036854775807}}}}})
        elif value==6:
            reply(request,{'resultType':'input_required','requestState':'opaque','inputRequests':{'confirmation':{'method':'elicitation/create','params':{'mode':'form','message':'confirm','requestedSchema':{'type':'object','properties':{}}}}}})
        elif value==8:
            reply(request,{'resultType':'task','taskId':'forbidden-native-task','status':'working','createdAt':'2026-01-01T00:00:00Z','lastUpdatedAt':'2026-01-01T00:00:00Z','ttlMs':1000})
        elif value==9:
            print(json.dumps({'jsonrpc':'2.0','id':request['id'],'error':{'code':-32603,'message':'synthetic RPC fault','data':{'value':9223372036854775807}}}),flush=True)
        elif value==10:
            assert held is not None and held_second is None
            held_second=request
        elif value==11:
            assert cancelled is not None and held is None and held_second is not None, 'third call crossed the exact concurrency bound before cancellation'
            complete(request,11,{'calls':calls,'active':1})
        elif value==12:
            assert held is None and held_second is None and len(cancelled_ids)==2
            complete(request,12,{'calls':calls,'active':0})
        elif value==13:
            complete(request,'invalid integer output',{'calls':calls})
        elif value==14:
            reply(request,{'resultType':'complete','content':[],'isError':True,'_meta':{'com.harnlang/toolContract':{'applicationError':{'tool':'echo','data':{'kind':'rejected','code':5.0}}}}})
        else:
            complete(request,value,{'calls':calls})
"#;

async fn worker(ir: &ValidatedCodegenIr, alter: impl FnOnce(&mut Value)) -> VmMcpClientHandle {
    let mut profile = serde_json::to_value(NativeMcpBindingProfile::for_ir(
        ir,
        NonZeroU32::new(2).unwrap(),
    ))
    .unwrap();
    alter(&mut profile);
    let env = BTreeMap::from([
        (
            "NATIVE_PROFILE".into(),
            serde_json::to_string(&profile).unwrap(),
        ),
        (
            "NATIVE_TOOLS".into(),
            serde_json::to_string(&ir.ir().mcp_tools).unwrap(),
        ),
    ]);
    super::super::connect::mcp_connect_stdio_impl(
        "python3",
        &["-u".into(), "-c".into(), WORKER.into()],
        &env,
        None,
        crate::mcp_protocol::PROTOCOL_VERSION.into(),
    )
    .await
    .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn native_declared_errors_validate_wire_tokens_before_publication() {
    let ir = contract();
    let native = worker(&ir, |_| {})
        .await
        .bind_native(ir, NonZeroU32::new(2).unwrap())
        .await
        .unwrap();
    let valid = native
        .call(ToolAudience::Mcp, "echo", &json!({"value":5}))
        .await
        .unwrap();
    let ToolInvocationOutcome::ApplicationError(error) = valid.outcome else {
        panic!("known declared application error must reach the public call")
    };
    assert_eq!(error.data["code"], json!(i64::MAX));
    assert_eq!(
        native
            .call(ToolAudience::Mcp, "echo", &json!({"value":14}))
            .await
            .err(),
        Some(NativeMcpFailure::Handler),
        "a floating-point token cannot inhabit an integer error field"
    );
    let after = native
        .call(ToolAudience::Mcp, "echo", &json!({"value":7}))
        .await
        .unwrap();
    assert_eq!(after.metadata.unwrap()["calls"], json!(3));
    native.disconnect().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn verified_unary_calls_overlap_pair_by_id_and_cancel_only_the_abandoned_call() {
    let ir = contract();
    let handle = worker(&ir, |_| {}).await;
    let native = handle
        .bind_native(ir, NonZeroU32::new(2).unwrap())
        .await
        .unwrap();
    assert_eq!(
        native
            .call(ToolAudience::Mcp, "echo", &json!({"value":"bad"}))
            .await
            .err(),
        Some(NativeMcpFailure::Input)
    );
    assert_eq!(
        native
            .call(ToolAudience::Mcp, "forged", &json!({"value":1}))
            .await
            .err(),
        Some(NativeMcpFailure::Operation)
    );
    let first_input = json!({"value":1});
    let second_input = json!({"value":2});
    let mut first = Box::pin(native.call(ToolAudience::Mcp, "echo", &first_input));
    let mut second = Box::pin(native.call(ToolAudience::Mcp, "echo", &second_input));
    let response = tokio::select! {
        biased;
        _=&mut first => panic!("held first call completed before cancellation"),
        response=&mut second => response.unwrap(),
    };
    let ToolInvocationOutcome::Success { json, value, .. } = response.outcome else {
        panic!("successful second call")
    };
    assert_eq!(json, json!(i64::MAX));
    assert!(matches!(value, crate::VmValue::Int(i64::MAX)));
    let metadata = response.metadata.unwrap();
    assert_eq!(metadata["calls"], 2);
    assert_eq!(metadata["full_width"], json!(i64::MAX));
    assert_ne!(metadata["first_id"], metadata["second_id"]);
    drop(first);
    assert!(
        handle.call("tools/list", json!({})).await.is_err(),
        "raw compatibility cannot share verified response accounting"
    );
    let after = native
        .call(ToolAudience::Mcp, "echo", &json!({"value":3}))
        .await
        .unwrap();
    let meta = after.metadata.unwrap();
    assert_eq!(meta["cancelled_id"], metadata["first_id"]);
    assert_eq!(meta["calls"], 3, "invalid inputs never reached a handler");
    assert_eq!(
        meta["lists"], 1,
        "raw call was refused before wire dispatch"
    );
    native.disconnect().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn native_profile_refusal_leaves_the_worker_handler_census_at_zero() {
    let ir = contract();
    for field in ["catalog_digest", "binding_digest", "ir_digest"] {
        let handle = worker(&ir, |profile| {
            profile["identity"][field] = json!("sha256:wrong")
        })
        .await;
        assert_eq!(
            handle
                .bind_native(ir.clone(), NonZeroU32::new(2).unwrap())
                .await
                .err()
                .unwrap(),
            NativeMcpFailure::Profile
        );
        let evidence = handle.call("tools/list", json!({})).await.unwrap();
        assert_eq!(evidence["_meta"]["calls"], 0);
        handle.disconnect().await.unwrap();
    }
}

#[tokio::test]
async fn selected_contract_does_not_publish_or_invoke_unselected_operations() {
    let ir = contract();
    let mut catalog: ToolCatalog = serde_json::from_str(&ir.ir().catalog_canonical_json).unwrap();
    let mut unselected = catalog.tools[0].clone();
    unselected.name = "unselected".into();
    unselected.cli.command = vec!["unselected".into()];
    catalog.tools.push(unselected);
    let ir = compile_catalog(
        &PreparedToolCatalog::prepare(catalog).unwrap(),
        &["echo".into()],
    )
    .unwrap();
    assert_eq!(ir.ir().operations.len(), 1);
    let client = worker(&ir, |_| {}).await;
    let binding = client
        .bind_native(ir, NonZeroU32::new(2).unwrap())
        .await
        .unwrap();
    assert_eq!(
        binding
            .catalog()
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec!["echo"]
    );
    assert!(matches!(
        binding
            .call(ToolAudience::Mcp, "unselected", &json!({"value":7}))
            .await,
        Err(NativeMcpFailure::Operation)
    ));
    let reached = binding
        .call(ToolAudience::Mcp, "echo", &json!({"value":7}))
        .await
        .unwrap();
    assert_eq!(reached.metadata.unwrap()["calls"], json!(1));
    binding.disconnect().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn native_errors_and_incomplete_results_cannot_become_success_or_replay() {
    let ir = contract();
    let handle = worker(&ir, |_| {}).await;
    let native = handle
        .bind_native(ir, NonZeroU32::new(2).unwrap())
        .await
        .unwrap();
    let error = native
        .call(ToolAudience::Mcp, "echo", &json!({"value":5}))
        .await
        .unwrap();
    let ToolInvocationOutcome::ApplicationError(error) = error.outcome else {
        panic!("declared error")
    };
    assert_eq!(error.data["code"], json!(i64::MAX));
    assert_eq!(
        native
            .call(ToolAudience::Mcp, "echo", &json!({"value":6}))
            .await
            .err(),
        Some(NativeMcpFailure::Incomplete)
    );
    assert_eq!(
        native
            .call(ToolAudience::Mcp, "echo", &json!({"value":9}))
            .await
            .err(),
        Some(NativeMcpFailure::Rpc)
    );
    assert_eq!(
        native
            .call(ToolAudience::Mcp, "echo", &json!({"value":8}))
            .await
            .err(),
        Some(NativeMcpFailure::Incomplete)
    );
    let observed = native
        .call(ToolAudience::Mcp, "echo", &json!({"value":7}))
        .await
        .unwrap();
    assert_eq!(
        observed.metadata.unwrap()["calls"],
        5,
        "input-required never caused a replay"
    );
    assert_eq!(
        native
            .call(ToolAudience::Mcp, "echo", &json!({"value":4}))
            .await
            .err(),
        Some(NativeMcpFailure::Transport)
    );
    native.disconnect().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn native_exact_concurrency_bound_holds_until_a_request_permit_is_retired() {
    let ir = contract();
    let handle = worker(&ir, |_| {}).await;
    let native = handle
        .bind_native(ir, NonZeroU32::new(2).unwrap())
        .await
        .unwrap();
    let first_input = json!({"value":1});
    let second_input = json!({"value":10});
    let third_input = json!({"value":11});
    let mut first = Box::pin(native.call(ToolAudience::Mcp, "echo", &first_input));
    let mut second = Box::pin(native.call(ToolAudience::Mcp, "echo", &second_input));
    let mut third = Box::pin(native.call(ToolAudience::Mcp, "echo", &third_input));
    assert!(futures::poll!(first.as_mut()).is_pending());
    assert!(futures::poll!(second.as_mut()).is_pending());
    assert!(futures::poll!(third.as_mut()).is_pending());
    drop(first);
    let third = third.await.unwrap();
    assert_eq!(third.metadata.unwrap()["active"], 1);
    drop(second);
    let terminal = native
        .call(ToolAudience::Mcp, "echo", &json!({"value":12}))
        .await
        .unwrap();
    assert_eq!(terminal.metadata.as_ref().unwrap()["calls"], 4);
    assert_eq!(terminal.metadata.unwrap()["active"], 0);
    native.disconnect().await.unwrap();
}

#[tokio::test]
async fn canonical_agent_retry_budget_never_replays_a_native_handler_after_rpc_failure() {
    for failure in [9, 5, 13] {
        let ir = contract();
        let mut catalog: ToolCatalog =
            serde_json::from_str(&ir.ir().catalog_canonical_json).unwrap();
        catalog.tools[0]
            .governance
            .audiences
            .push(ToolAudience::Agent);
        let ir =
            compile_catalog(&PreparedToolCatalog::prepare(catalog.clone()).unwrap(), &[]).unwrap();
        let client = worker(&ir, |_| {}).await;
        let binding = client
            .bind_native(ir, NonZeroU32::new(2).unwrap())
            .await
            .unwrap();
        let mut vm = crate::new_vm();
        crate::register_vm_stdlib(&mut vm);
        let invoked = binding.clone();
        vm.register_async_builtin("native_effect", move |_ctx, args| {
            let invoked = invoked.clone();
            async move { invoked.invoke_vm("echo", &args[0]).await }
        });
        let chunk = crate::compile_source(
            "fn proxy(input: dict) { return native_effect(input) }\nreturn proxy",
        )
        .unwrap();
        let crate::VmValue::Closure(handler) = vm.execute(&chunk).await.unwrap() else {
            panic!("proxy closure was not reached")
        };
        let registry = crate::tool_registry::bind_catalog_handlers(
            catalog,
            BTreeMap::from([("echo".into(), handler.as_ref().clone())]),
        )
        .unwrap();
        let _failed = vm
        .call_named_builtin(
            "__host_agent_dispatch_tool_call",
            vec![
                crate::schema::json_to_vm_value(
                    &json!({"id":"native-mutation","name":"echo","arguments":{"value":failure}}),
                ),
                registry,
                crate::schema::json_to_vm_value(&json!({"tool_retries":2,"tool_backoff_ms":0})),
            ],
        )
        .await;
        let observed = binding
            .call(ToolAudience::Agent, "echo", &json!({"value":7}))
            .await
            .unwrap();
        assert_eq!(
            observed.metadata.unwrap()["calls"],
            json!(2),
            "one failed mutation and one reached observer call; native failure {failure} must not replay the mutation"
        );
        binding.disconnect().await.unwrap();
    }
}

#[tokio::test]
async fn ordinary_generic_handler_retries_keep_the_existing_explicit_budget() {
    let ir = contract();
    let mut catalog: ToolCatalog = serde_json::from_str(&ir.ir().catalog_canonical_json).unwrap();
    catalog.tools[0]
        .governance
        .audiences
        .push(ToolAudience::Agent);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    let mut vm = crate::new_vm();
    crate::register_vm_stdlib(&mut vm);
    vm.register_async_builtin("legacy_effect", move |_ctx, _args| {
        let observed = observed.clone();
        async move {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(crate::VmError::Runtime("ordinary fixture failure".into()))
        }
    });
    let chunk = crate::compile_source(
        "fn proxy(input: dict) { return legacy_effect(input) }\nreturn proxy",
    )
    .unwrap();
    let crate::VmValue::Closure(handler) = vm.execute(&chunk).await.unwrap() else {
        panic!("legacy fixture not reached")
    };
    let registry = crate::tool_registry::bind_catalog_handlers(
        catalog,
        BTreeMap::from([("echo".into(), handler.as_ref().clone())]),
    )
    .unwrap();
    let _result = vm
        .call_named_builtin(
            "__host_agent_dispatch_tool_call",
            vec![
                crate::schema::json_to_vm_value(
                    &json!({"id":"legacy-retry","name":"echo","arguments":{"value":9}}),
                ),
                registry,
                crate::schema::json_to_vm_value(&json!({"tool_retries":2,"tool_backoff_ms":0})),
            ],
        )
        .await;
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
}

#[tokio::test]
async fn native_deadline_cancels_only_the_reached_request_without_replay() {
    let ir = contract();
    let client = worker(&ir, |_| {}).await;
    let binding = client
        .bind_native(ir, NonZeroU32::new(2).unwrap())
        .await
        .unwrap();
    let first = json!({"value":1});
    let second = json!({"value":2});
    let mut held = Box::pin(binding.call(ToolAudience::Mcp, "echo", &first));
    let overlap = binding.call(ToolAudience::Mcp, "echo", &second);
    tokio::pin!(overlap);
    let positive = tokio::select! { biased;
        _ = &mut held => panic!("first request must remain held"),
        result = &mut overlap => result.unwrap(),
    };
    assert_eq!(positive.metadata.unwrap()["calls"], json!(2));
    tokio::time::pause();
    tokio::time::advance(super::super::MCP_TIMEOUT).await;
    assert!(matches!(held.await, Err(NativeMcpFailure::Deadline)));
    tokio::time::resume();
    let after = binding
        .call(ToolAudience::Mcp, "echo", &json!({"value":3}))
        .await
        .unwrap();
    assert_eq!(after.metadata.unwrap()["calls"], json!(3));
    binding.disconnect().await.unwrap();
}
