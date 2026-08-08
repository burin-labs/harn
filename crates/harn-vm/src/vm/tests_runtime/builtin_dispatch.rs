//! Choosing the direct builtin call path over the host bridge.
//!
//! Registered sync and async builtins get their direct id, callbacks get a
//! builtin-ref id, user functions and local closures still shadow a builtin of
//! the same name, and an unregistered name falls back to the bridge.

use crate::compiler::Compiler;
use crate::stdlib::register_vm_stdlib;
use crate::VmValue;
use harn_lexer::Lexer;
use harn_parser::Parser;
use std::sync::Arc;

use super::harness::*;
use crate::vm::*;
#[test]
fn test_direct_builtin_call_uses_registered_sync_id() {
    let (out, _) = run_harn_with_setup(
        r#"pipeline t(harness: Harness, task) { test_sync("ok") }"#,
        |vm| {
            vm.register_builtin("test_sync", |args, out| {
                out.push_str("sync:");
                out.push_str(&args[0].display());
                Ok(VmValue::Nil)
            });
        },
    )
    .unwrap();
    assert_eq!(out, "sync:ok");
}

#[test]
fn test_direct_builtin_call_uses_registered_async_id() {
    let (out, _) = run_harn_with_setup(
        r#"pipeline t(harness: Harness, task) {
const value = test_async("ok")
harness.stdio.log(value)
}"#,
        |vm| {
            vm.register_async_builtin("test_async", |_ctx, args| async move {
                Ok(VmValue::String(arcstr::ArcStr::from(format!(
                    "async:{}",
                    args[0].display()
                ))))
            });
        },
    )
    .unwrap();
    assert_eq!(out.trim(), "[harn] async:ok");
}

#[test]
fn test_direct_builtin_callback_uses_builtin_ref_id() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task) {
const converted = ["first_name"].map(snake_to_camel)
harness.stdio.log(converted[0])
}"#,
    );
    assert_eq!(out, "[harn] firstName");
}

#[test]
fn test_direct_builtin_call_preserves_function_shadowing() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task) {
fn push(xs, x) {
  harness.stdio.log("shadow")
}
push([1], 2)
}"#,
    );
    assert_eq!(out, "[harn] shadow");
}

#[test]
fn test_direct_builtin_call_preserves_local_closure_shadowing() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task) {
const push = { xs, x -> harness.stdio.log("local") }
push([1], 2)
}"#,
    );
    assert_eq!(out, "[harn] local");
}

#[test]
fn test_direct_builtin_call_falls_back_to_bridge() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let out = rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let tmp = tempfile::tempdir().unwrap();
                let host_path = tmp.path().join("host.harn");
                std::fs::write(
                    &host_path,
                    r#"pub fn bridge_echo(value) { return "bridge:" + value }"#,
                )
                .unwrap();

                let mut host_vm = Vm::new();
                register_vm_stdlib(&mut host_vm);
                let bridge = crate::bridge::HostBridge::from_harn_module(host_vm, &host_path)
                    .await
                    .unwrap();

                let source = r#"pipeline t(harness: Harness, task) { harness.stdio.log(bridge_echo("ok")) }"#;
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().unwrap();
                let mut parser = Parser::new(tokens);
                let program = parser.parse().unwrap();
                let chunk = Compiler::new().compile(&program).unwrap();

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_bridge(Arc::new(bridge));
                vm.execute(&chunk).await.unwrap();
                vm.output().trim().to_string()
            })
            .await
    });
    assert_eq!(out, "[harn] bridge:ok");
}
