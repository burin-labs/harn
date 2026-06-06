//! # harn-codegen
//!
//! An experimental ahead-of-time / JIT native compiler for the *scalar-compute
//! subset* of Harn.
//!
//! Harn is an orchestration language: most real programs are dominated by LLM
//! calls, tool dispatch, and other I/O, where native code generation buys
//! nothing. The honest, useful niche this crate targets is the opposite end —
//! small, hot, side-effect-free numeric kernels (scoring, parsing helpers,
//! bit-twiddling, geometry, …) that run in tight loops. For those, lowering
//! the bytecode to machine code removes interpreter dispatch overhead entirely.
//!
//! ## Pipeline
//!
//! ```text
//! Harn source ──(harn-vm front-end)──▶ Chunk/bytecode
//!            │
//!            ├─ decode  (bytecode.rs)  reachable scalar instructions
//!            ├─ verify  (verify.rs)    typed control-flow graph (ScalarFunction)
//!            ├─ lower   (lower.rs)     Cranelift IR
//!            └─ backend
//!                 ├─ jit.rs   in-process machine code  ▶ NativeFunction
//!                 └─ aot.rs   relocatable object file  ▶ ObjectArtifact
//! ```
//!
//! [`eval`] provides a pure-Rust reference interpreter over `ScalarFunction`
//! that mirrors the VM's semantics; it is the differential-test oracle and a
//! dependency-free fallback.
//!
//! ## Supported subset
//!
//! `int` (`i64`), `float` (`f64`), and `bool`; arithmetic (`+ - * / %`, integer
//! `%` and `/` trap-checked, no float `%`), comparisons, logical `!`, `if`/
//! `else`, `while` loops, ternaries, short-circuit `&&`/`||`, and `let`/`var`
//! locals. Anything else — strings, lists, dicts, `nil`, closures,
//! host/`harness` calls, `await`, `**` — is reported as
//! [`CodegenError::Unsupported`], which callers treat as "stay on the
//! interpreter".
//!
//! ## Not part of the distributed binary
//!
//! This crate links Cranelift and is intentionally absent from the dependency
//! graph of `harn-cli`/`harn-vm`. It is `publish = false` and never ships in
//! the crates.io binary. See `docs/src/dev/native-codegen.md`.

mod aot;
mod bytecode;
mod error;
mod eval;
mod jit;
mod lower;
mod source;
mod value;
mod verify;

pub use aot::{emit_object, ObjectArtifact};
pub use bytecode::{BinOp, CmpOp, Instr};
pub use error::{CodegenError, NativeTrap};
pub use eval::{evaluate, EvalError};
pub use jit::{compile as jit_compile, NativeFunction};
pub use source::{analyze_function, analyze_named};
pub use value::{ScalarType, ScalarValue};
pub use verify::{verify, Block, ScalarFunction, Terminator};

/// Compile a named top-level Harn function from `source` to in-process native
/// code in one step.
///
/// # Errors
///
/// Propagates any [`CodegenError`] from front-end compilation, verification,
/// or the JIT backend.
pub fn compile_named(source: &str, function: &str) -> Result<NativeFunction, CodegenError> {
    let scalar = analyze_named(source, function)?;
    jit_compile(&scalar)
}

/// Compile a named top-level Harn function from `source` to a host-target
/// object file in one step.
///
/// # Errors
///
/// Propagates any [`CodegenError`] from front-end compilation, verification,
/// or the object backend.
pub fn compile_named_object(source: &str, function: &str) -> Result<ObjectArtifact, CodegenError> {
    let scalar = analyze_named(source, function)?;
    emit_object(&scalar)
}

/// Derive a stable, linker-safe C symbol from a Harn function name.
///
/// Non-identifier characters become `_`; the `harn_scalar_` prefix avoids
/// leading-digit and reserved-name collisions and namespaces the export.
pub(crate) fn symbol_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 12);
    out.push_str("harn_scalar_");
    let mut any = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
            any = true;
        } else {
            out.push('_');
        }
    }
    if !any {
        out.push_str("anon");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::symbol_name;

    #[test]
    fn symbol_names_are_sanitised() {
        assert_eq!(symbol_name("add"), "harn_scalar_add");
        assert_eq!(symbol_name("score_v2"), "harn_scalar_score_v2");
        assert_eq!(symbol_name("foo.bar-baz"), "harn_scalar_foo_bar_baz");
        assert_eq!(symbol_name(""), "harn_scalar_anon");
        // No identifier characters at all -> substitutes plus the `anon` tag.
        assert_eq!(symbol_name("!!!"), "harn_scalar____anon");
    }
}
