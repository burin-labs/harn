//! Program runners for runtime tests. Setup runs after stdlib registration.

use crate::compiler::{Compiler, CompilerOptions};
use crate::stdlib::register_vm_stdlib;
use crate::vm::*;
use crate::{Chunk, InlineCacheEntry, VmError, VmValue};
use harn_clock::test_support::run_local;
use harn_lexer::Lexer;
use harn_parser::Parser;
use std::collections::HashSet;
use std::path::Path;

fn compile(source: &str, compiler: Compiler) -> Result<Chunk, String> {
    let tokens = Lexer::new(source)
        .tokenize()
        .map_err(|error| error.to_string())?;
    let program = Parser::new(tokens)
        .parse()
        .map_err(|error| error.to_string())?;
    compiler
        .compile(&program)
        .map_err(|error| error.to_string())
}

fn test_vm() -> Vm {
    let mut vm = crate::new_vm();
    register_vm_stdlib(&mut vm);
    vm.set_harness(crate::Harness::real());
    vm
}

async fn execute(chunk: Chunk, setup: impl FnOnce(&mut Vm)) -> Result<(String, VmValue), VmError> {
    let mut vm = test_vm();
    setup(&mut vm);
    let result = vm.execute(&chunk).await?;
    Ok((vm.output().to_string(), result))
}

pub(in crate::vm_tests) fn run_harn(source: &str) -> (String, VmValue) {
    run_harn_with_options(source, CompilerOptions::optimized())
}

pub(super) fn run_harn_with_options(source: &str, options: CompilerOptions) -> (String, VmValue) {
    run_local(async {
        execute(
            compile(source, Compiler::with_options(options)).unwrap(),
            |_| {},
        )
        .await
        .unwrap()
    })
}

pub(super) fn run_harn_with_legacy_ambient_capabilities(source: &str) -> (String, VmValue) {
    run_local(async {
        let options = CompilerOptions::optimized().with_legacy_ambient_capabilities();
        let chunk = compile(source, Compiler::with_options(options)).unwrap();
        let mut vm = crate::new_vm();
        vm.set_harness(crate::Harness::real());
        let root = vm.root_harness_value().expect("test installs root Harness");
        vm.set_global("harness", root);
        register_vm_stdlib(&mut vm);
        let result = vm.execute(&chunk).await.unwrap();
        (vm.output().to_string(), result)
    })
}

pub(in crate::vm_tests) fn run_harn_result_display_with_options(
    source: &str,
    options: CompilerOptions,
) -> Result<(String, String), String> {
    run_local(async {
        let chunk = compile(source, Compiler::with_options(options))?;
        execute(chunk, |_| {})
            .await
            .map(|(output, value)| (output, value.display()))
            .map_err(|error| error.to_string())
    })
}

pub(super) fn run_harn_with_inline_cache_entries(
    source: &str,
) -> (Vec<InlineCacheEntry>, String, VmValue) {
    run_local(async {
        let chunk = compile(source, Compiler::new()).unwrap();
        let mut vm = test_vm();
        let result = vm.execute(&chunk).await.unwrap();
        let entries = vm.inline_cache_entries().cloned().collect();
        (entries, vm.output().to_string(), result)
    })
}

pub(super) fn run_output(source: &str) -> String {
    run_harn(source).0.trim_end().to_string()
}

pub(in crate::vm_tests) fn run_harn_at(
    path: &Path,
    source: &str,
) -> Result<(String, VmValue), VmError> {
    run_harn_with_setup(source, |vm| {
        vm.set_source_info(&path.display().to_string(), source);
        if let Some(parent) = path.parent() {
            vm.set_source_dir(parent);
        }
    })
}

pub(super) fn run_harn_result(source: &str) -> Result<(String, VmValue), VmError> {
    run_local(run_harn_result_async(source))
}

pub(in crate::vm_tests) async fn run_harn_result_async(
    source: &str,
) -> Result<(String, VmValue), VmError> {
    execute(compile(source, Compiler::new()).unwrap(), |_| {}).await
}

pub(super) fn run_harn_with_setup(
    source: &str,
    setup: impl FnOnce(&mut Vm),
) -> Result<(String, VmValue), VmError> {
    run_local(async { execute(compile(source, Compiler::new()).unwrap(), setup).await })
}

pub(super) fn run_harn_with_policy(
    source: &str,
    policy: crate::orchestration::CapabilityPolicy,
) -> Result<(String, VmValue), VmError> {
    crate::orchestration::push_execution_policy(policy);
    let result = run_harn_result(source);
    crate::orchestration::pop_execution_policy();
    result
}

pub(super) fn run_vm(source: &str) -> String {
    run_harn_result(source).unwrap().0
}

pub(super) fn run_vm_err(source: &str) -> String {
    match run_harn_result(source) {
        Err(error) => error.to_string(),
        Ok(_) => panic!("Expected error"),
    }
}

pub(super) fn run_harn_with_denied(
    source: &str,
    denied: HashSet<String>,
) -> Result<(String, VmValue), VmError> {
    run_harn_with_setup(source, |vm| vm.set_denied_builtins(denied))
}
