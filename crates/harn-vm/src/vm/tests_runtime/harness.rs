//! Shared program runners for the runtime tests.
//!
//! Every runner compiles a Harn source string and executes it, differing only
//! in what it hands back: printed output, the returned value, the raw
//! `VmError`, recorded inline-cache entries, or a rendered error string. The
//! `*_with_*` variants take the compiler options, a setup hook, a capability
//! policy, or a denied-builtin set.

use crate::compiler::{Compiler, CompilerOptions};
use crate::stdlib::register_vm_stdlib;
use crate::{InlineCacheEntry, VmError, VmValue};
use harn_lexer::Lexer;
use harn_parser::Parser;
use std::collections::HashSet;
use std::path::Path;

use crate::vm::*;
pub(in crate::vm) fn run_harn(source: &str) -> (String, VmValue) {
    run_harn_with_options(source, CompilerOptions::optimized())
}

pub(super) fn run_harn_with_options(source: &str, options: CompilerOptions) -> (String, VmValue) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().unwrap();
                let mut parser = Parser::new(tokens);
                let program = parser.parse().unwrap();
                let chunk = Compiler::with_options(options).compile(&program).unwrap();

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_harness(crate::Harness::real());
                let result = vm.execute(&chunk).await.unwrap();
                (vm.output().to_string(), result)
            })
            .await
    })
}

pub(super) fn run_harn_with_legacy_ambient_capabilities(source: &str) -> (String, VmValue) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().unwrap();
                let mut parser = Parser::new(tokens);
                let program = parser.parse().unwrap();
                let chunk = Compiler::with_options(
                    CompilerOptions::optimized().with_legacy_ambient_capabilities(),
                )
                .compile(&program)
                .unwrap();

                let mut vm = Vm::new();
                vm.set_harness(crate::Harness::real());
                let root = vm.root_harness_value().expect("test installs root Harness");
                vm.set_global("harness", root);
                register_vm_stdlib(&mut vm);
                let result = vm.execute(&chunk).await.unwrap();
                (vm.output().to_string(), result)
            })
            .await
    })
}

pub(in crate::vm) fn run_harn_result_display_with_options(
    source: &str,
    options: CompilerOptions,
) -> Result<(String, String), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().map_err(|error| error.to_string())?;
                let mut parser = Parser::new(tokens);
                let program = parser.parse().map_err(|error| error.to_string())?;
                let chunk = Compiler::with_options(options)
                    .compile(&program)
                    .map_err(|error| error.to_string())?;

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_harness(crate::Harness::real());
                let result = vm
                    .execute(&chunk)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok((vm.output().to_string(), result.display()))
            })
            .await
    })
}

pub(super) fn run_harn_with_inline_cache_entries(
    source: &str,
) -> (Vec<InlineCacheEntry>, String, VmValue) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().unwrap();
                let mut parser = Parser::new(tokens);
                let program = parser.parse().unwrap();
                let chunk = Compiler::new().compile(&program).unwrap();

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_harness(crate::Harness::real());
                let result = vm.execute(&chunk).await.unwrap();
                let inline_cache_entries = vm
                    .inline_cache_sets
                    .iter()
                    .flat_map(|entries| entries.iter().cloned())
                    .collect();
                (inline_cache_entries, vm.output().to_string(), result)
            })
            .await
    })
}

pub(super) fn run_output(source: &str) -> String {
    run_harn(source).0.trim_end().to_string()
}

pub(in crate::vm) fn run_harn_at(path: &Path, source: &str) -> Result<(String, VmValue), VmError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().unwrap();
                let mut parser = Parser::new(tokens);
                let program = parser.parse().unwrap();
                let chunk = Compiler::new().compile(&program).unwrap();

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_harness(crate::Harness::real());
                vm.set_source_info(&path.display().to_string(), source);
                if let Some(parent) = path.parent() {
                    vm.set_source_dir(parent);
                }
                let result = vm.execute(&chunk).await?;
                Ok((vm.output().to_string(), result))
            })
            .await
    })
}

pub(super) fn run_harn_result(source: &str) -> Result<(String, VmValue), VmError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().unwrap();
                let mut parser = Parser::new(tokens);
                let program = parser.parse().unwrap();
                let chunk = Compiler::new().compile(&program).unwrap();

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_harness(crate::Harness::real());
                let result = vm.execute(&chunk).await?;
                Ok((vm.output().to_string(), result))
            })
            .await
    })
}

pub(in crate::vm) async fn run_harn_result_async(
    source: &str,
) -> Result<(String, VmValue), VmError> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let chunk = Compiler::new().compile(&program).unwrap();

    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_harness(crate::Harness::real());
    let result = vm.execute(&chunk).await?;
    Ok((vm.output().to_string(), result))
}

pub(super) fn run_harn_with_setup<F>(source: &str, setup: F) -> Result<(String, VmValue), VmError>
where
    F: FnOnce(&mut Vm),
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().unwrap();
                let mut parser = Parser::new(tokens);
                let program = parser.parse().unwrap();
                let chunk = Compiler::new().compile(&program).unwrap();

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_harness(crate::Harness::real());
                setup(&mut vm);
                let result = vm.execute(&chunk).await?;
                Ok((vm.output().to_string(), result))
            })
            .await
    })
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
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().unwrap();
                let mut parser = Parser::new(tokens);
                let program = parser.parse().unwrap();
                let chunk = Compiler::new().compile(&program).unwrap();
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_harness(crate::Harness::real());
                vm.execute(&chunk).await.unwrap();
                vm.output().to_string()
            })
            .await
    })
}

pub(super) fn run_vm_err(source: &str) -> String {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().unwrap();
                let mut parser = Parser::new(tokens);
                let program = parser.parse().unwrap();
                let chunk = Compiler::new().compile(&program).unwrap();
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_harness(crate::Harness::real());
                match vm.execute(&chunk).await {
                    Err(e) => format!("{e}"),
                    Ok(_) => panic!("Expected error"),
                }
            })
            .await
    })
}

/// Helper that runs Harn source with a set of denied builtins.
pub(super) fn run_harn_with_denied(
    source: &str,
    denied: HashSet<String>,
) -> Result<(String, VmValue), VmError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().unwrap();
                let mut parser = Parser::new(tokens);
                let program = parser.parse().unwrap();
                let chunk = Compiler::new().compile(&program).unwrap();

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_harness(crate::Harness::real());
                vm.set_denied_builtins(denied);
                let result = vm.execute(&chunk).await?;
                Ok((vm.output().to_string(), result))
            })
            .await
    })
}
