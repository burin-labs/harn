//! What the compiler emits before anything runs.
//!
//! Named-pipeline parameter binding, binding params from globals, local type
//! aliases surviving as runtime schema values, and the disassembler output.

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
fn compile_named_with_param_globals_binds_params_from_globals() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let source = r#"
pipeline selected(value) {
  log("${value}")
}
"#;
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().unwrap();
                let mut parser = Parser::new(tokens);
                let program = parser.parse().unwrap();
                let chunk = Compiler::new()
                    .compile_named_with_param_globals(&program, "selected")
                    .unwrap();

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_global("value", VmValue::Int(42));
                let result = vm.execute(&chunk).await.unwrap();

                assert!(matches!(result, VmValue::Nil));
                assert_eq!(vm.output().trim_end(), "[harn] 42");
            })
            .await;
    });
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
