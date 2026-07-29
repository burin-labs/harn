//! What the compiler emits before anything runs.
//!
//! Named-pipeline invocation, local type aliases surviving as runtime schema
//! values, and the disassembler output.

use crate::compiler::{Compiler, CompilerOptions};
use crate::stdlib::register_vm_stdlib;
use crate::VmValue;
use harn_lexer::Lexer;
use harn_parser::Parser;

use super::harness::*;
use crate::vm::*;
#[test]
fn compile_named_pipeline_ignores_unbound_params() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let source = r#"
pipeline selected(task) {
  log("ok")
}
"#;
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().unwrap();
                let mut parser = Parser::new(tokens);
                let program = parser.parse().unwrap();
                let chunk = Compiler::new().compile_named(&program, "selected").unwrap();

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                let result = vm.execute(&chunk).await.unwrap();

                assert!(matches!(result, VmValue::Nil));
                assert_eq!(vm.output().trim_end(), "[harn] ok");
            })
            .await;
    });
}

#[test]
fn callable_entry_invokes_pipeline_with_explicit_typed_values() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        tokio::task::LocalSet::new()
            .run_until(async {
                let source = "pipeline selected(value: int) { return value + 1 }";
                let program = parse(source);
                let entry = Compiler::new()
                    .compile_named_pipeline_entry(&program, "selected", None)
                    .unwrap();

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                assert!(matches!(
                    vm.execute_callable_entry_with_timeout(
                        &entry,
                        &[VmValue::Int(41)],
                        std::time::Duration::from_secs(1),
                    )
                    .await,
                    Ok(VmValue::Int(42))
                ));

                let mut wrong_type = Vm::new();
                register_vm_stdlib(&mut wrong_type);
                let error = wrong_type
                    .execute_callable_entry_with_timeout(
                        &entry,
                        &[VmValue::string("forty-one")],
                        std::time::Duration::from_secs(1),
                    )
                    .await
                    .unwrap_err();
                assert!(
                    error.to_string().contains("expected int"),
                    "unexpected type error: {error}"
                );
            })
            .await;
    });
}

#[test]
fn callable_entry_runs_fixture_and_pipeline_in_one_initialized_vm() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        tokio::task::LocalSet::new()
            .run_until(async {
                let source = r"
let fixture_calls = 0
fn fixture() -> {calls: int} {
  fixture_calls = fixture_calls + 1
  return {calls: fixture_calls}
}
pipeline selected(fx: {calls: int}, value: int) {
  return fx.calls * 40 + value
}
";
                let program = parse(source);
                let entry = Compiler::new()
                    .compile_named_pipeline_entry(&program, "selected", Some("fixture"))
                    .unwrap();

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                assert!(matches!(
                    vm.execute_callable_entry_with_timeout(
                        &entry,
                        &[VmValue::Int(2)],
                        std::time::Duration::from_secs(1),
                    )
                    .await,
                    Ok(VmValue::Int(42))
                ));
            })
            .await;
    });
}

fn parse(source: &str) -> Vec<harn_parser::SNode> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    Parser::new(tokens).parse().unwrap()
}

#[test]
fn local_type_alias_is_runtime_schema_value_for_user_wrappers() {
    let (out, _) = run_harn(
        r#"
fn accepts_schema(schema) {
  return schema_report({name: "Ada"}, schema).ok
}

fn uses_later_alias() {
  return accepts_schema(UserShape)
}

type UserShape = {name: string}

pipeline t(task) {
  log(accepts_schema(UserShape))
  log(uses_later_alias())
}
"#,
    );

    assert_eq!(out, "[harn] true\n[harn] true\n");
}

#[test]
fn test_disassembly() {
    let mut lexer = Lexer::new("pipeline t(task) { log(2 + 3) }");
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let chunk = Compiler::with_options(CompilerOptions::without_optimizations())
        .compile(&program)
        .unwrap();
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("CONSTANT"));
    assert!(disasm.contains("ADD"));
    assert!(disasm.contains("CALL_BUILTIN"));
}
