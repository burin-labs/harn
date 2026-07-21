//! Implementation of the type-inference walk on the AST.
//!
//! `TypeChecker` itself, its constructors, and the public `check*` entry
//! points live in `super` (`typechecker::mod.rs`). The actual inference
//! work — node-kind dispatch, refinement extraction, generic call binding,
//! variance enforcement — is split across this submodule by node-kind
//! family for readability.
//!
//! Each file declares `impl TypeChecker { … }` blocks that hang additional
//! methods off the same struct. Re-exports are unnecessary because the
//! impl blocks are picked up by Rust's `impl` resolution as long as they
//! are in scope (i.e. a file that names `mod statements` somewhere).

mod binary_ops;
mod blocks;
mod call_resolution;
mod calls;
mod decls;
mod entry;
mod expressions;
mod flow;
mod generic_bindings;
mod hitl;
mod literal_union;
mod pipe;
mod statements;
mod subtyping;
mod variance;
