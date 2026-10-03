use super::HostRequestTrace;
use crate::{Vm, VmValue};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct RecordingBridge(Mutex<Vec<HostRequestTrace>>);

impl crate::HostCallBridge for RecordingBridge {
    fn dispatch<'a>(
        &'a self,
        _capability: &'a str,
        _operation: &'a str,
        _params: &'a crate::value::DictMap,
    ) -> crate::HostCallDispatchFuture<'a> {
        panic!("VM host calls must carry attribution")
    }

    fn dispatch_traced<'a>(
        &'a self,
        capability: &'a str,
        operation: &'a str,
        _params: &'a crate::value::DictMap,
        trace: &'a HostRequestTrace,
    ) -> crate::HostCallDispatchFuture<'a> {
        assert_eq!((capability, operation), ("env", "scan"));
        self.0.lock().unwrap().push(trace.clone());
        crate::host_call_ready(Ok(Some(VmValue::Nil)))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn imported_callers_and_repeated_shapes_survive_async_dispatch_without_values() {
    let temp = tempfile::tempdir().unwrap();
    let helper = temp.path().join("helper.harn");
    let entry = temp.path().join("entry.harn");
    std::fs::write(
        &helper,
        r#"pub fn repeated() {
            for item in [1, 2] {
                host_call("env.scan", {path: "private-path-sentinel", argv: ["secret-command", "secret-argument"]})
            }
        }
        pub fn distinct() {
            host_call("env.scan", {path: "another-private-path", argv: []})
        }"#,
    ).unwrap();
    std::fs::write(
        &entry,
        r#"import { repeated, distinct } from "./helper"
        pub fn route() { repeated(); distinct() }"#,
    )
    .unwrap();
    let bridge = Arc::new(RecordingBridge::default());
    let _guard = crate::install_host_call_bridge(bridge.clone());
    let mut vm = Vm::new();
    crate::stdlib::register_vm_stdlib(&mut vm);
    vm.enable_trusted_host_dispatch().unwrap();
    let exports = vm.load_module_exports(&entry).await.unwrap();
    vm.call_closure_pub(exports.get("route").unwrap(), &[])
        .await
        .unwrap();

    let traces = bridge.0.lock().unwrap();
    assert_eq!(traces.len(), 3);
    assert_eq!(traces[0], traces[1]);
    for (trace, function, length) in [(&traces[0], "repeated", 2), (&traces[2], "distinct", 0)] {
        let caller = trace.caller.as_ref().expect("measured caller");
        assert_eq!(caller.function, function);
        assert_eq!(
            std::path::Path::new(caller.module.as_deref().unwrap())
                .canonicalize()
                .unwrap(),
            helper.canonicalize().unwrap(),
        );
        assert_eq!(trace.arguments.len(), 2);
        assert_eq!(trace.arguments["path"].value_type, "string");
        assert_eq!(trace.arguments["path"].list_length, None);
        assert_eq!(trace.arguments["argv"].value_type, "list");
        assert_eq!(trace.arguments["argv"].list_length, Some(length));
    }
    let serialized = serde_json::to_string(&*traces).unwrap();
    for secret in [
        "private-path-sentinel",
        "secret-command",
        "secret-argument",
        "another-private-path",
    ] {
        assert!(
            !serialized.contains(secret),
            "argument value leaked: {serialized}"
        );
    }
}
