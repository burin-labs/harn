#![recursion_limit = "256"]

pub mod acp;
mod bootstrap;
pub mod cli;
mod cli_bytecode;
pub mod commands;
mod compiler_context;
#[doc(hidden)]
pub mod dispatch;
mod entrypoint;
pub mod env_guard;
mod eval_cli;
pub mod exit;
pub mod format;
pub mod json_envelope;
pub use commands::check::{
    decode_lint_envelope, decode_lint_json, lint_json_schema, DecodedLintEnvelope, LintDecodeError,
    LintDecodeOptions, LintReportWire,
};
#[cfg(feature = "hostlib")]
mod local_embed;
mod net;
pub mod package;
mod path_policy;
mod provider_bootstrap;
mod provider_info;
mod run_records;
mod runtime;
pub mod skill_loader;
pub mod skill_provenance;
mod source_exec;
pub mod test_report;
pub mod test_runner;
pub mod test_timing {
    pub use harn_test_runner::DurationSummary;
}
#[doc(hidden)]
pub mod tests;
mod typecheck_imports;
mod worker_tenant;
pub use commands::dispatch_explain::DISPATCH_AUDIT_SCHEMA_VERSION;
pub(crate) use compiler_context::{
    compiler_for_source, compiler_for_standalone_source, compiler_with_imported_enum_candidates,
    ensure_builtin_signatures_installed,
};
pub use harn_skills::{get_embedded_skill, list_embedded_skills, EmbeddedSkill, SkillFrontmatter};
// Items that used to live directly in this file. A bare item at the crate
// root is visible crate-wide, so re-exporting the modules' `pub(crate)` items
// here keeps every existing `crate::<item>` path resolving unchanged.
pub(crate) use self::entrypoint::*;
pub(crate) use self::eval_cli::*;
pub(crate) use self::exit::*;
pub(crate) use self::provider_info::*;
pub(crate) use self::run_records::*;
pub(crate) use self::source_exec::*;

use clap::{error::ErrorKind, CommandFactory, Parser as ClapParser};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::{env, fs, panic, process, thread};

use cli::{
    Cli, Command, CompletionShell, EvalCommand, GuardCommand, MergeCaptainCommand,
    MergeCaptainMockCommand, ModelInfoArgs, PackageArtifactsCommand, PackageCacheCommand,
    PackageCommand, PackageScaffoldCommand, PgCommand, ProviderCommand, SkillCommand,
    SkillKeyCommand, SkillTrustCommand, TimeCommand, ToolCommand,
};
use harn_lexer::Lexer;
use harn_modules::project_config;
use harn_parser::{DiagnosticSeverity, Parser, TypeChecker};
use runtime::{build_cli_runtime, cli_runtime_mode, CliRuntimeMode};
/// The CLI's name for the shared runtime stack contract. Kept as a re-export
/// rather than its own number so the CLI and the serve hosts cannot drift.
pub const CLI_RUNTIME_STACK_SIZE: usize = harn_vm::RUNTIME_STACK_SIZE;
static BROKEN_PIPE_PANIC_HOOK: Once = Once::new();

#[cfg(feature = "hostlib")]
pub(crate) fn install_default_hostlib(vm: &mut harn_vm::Vm) {
    let embed = crate::local_embed::resolve_embed_capability();
    let _ = harn_hostlib::install_default_with_embed_and_harn_reference_resolver(
        vm,
        embed,
        Some(harn_serve::harn_reference_resolver()),
    );
    // The `rules` capability lives in its own crate (it depends on
    // `harn-rules`, which depends on `harn-hostlib`, so it can't ship inside
    // `install_default`). Wire it in alongside the defaults.
    harn_rules_hostlib::install(vm);
}

#[cfg(not(feature = "hostlib"))]
pub(crate) fn install_default_hostlib(_vm: &mut harn_vm::Vm) {}

/// Entry point used by `src/main.rs`. Hosts the CLI runtime thread and
/// drives the async dispatcher in `async_main`.
pub fn run() {
    install_broken_pipe_panic_hook();
    harn_vm::initialize_runtime_assets();
    let raw_args = normalize_serve_args(bootstrap::args_after_pre_runtime_command());
    // Defeat rlib dead-code stripping of `#[harn_builtin]`-emitted statics
    // (linkme issue #36). Without this touch the linker can drop every
    // builtin's distributed-slice entry, leaving `ALL_BUILTIN_DEFS` empty
    // and surfacing as a swarm of `HARN-NAM-002` errors at first call.
    harn_vm::stdlib::force_link();

    ensure_builtin_signatures_installed();

    let runtime_mode = cli_runtime_mode(&raw_args);

    let handle = thread::Builder::new()
        .name("harn-cli".to_string())
        .stack_size(CLI_RUNTIME_STACK_SIZE)
        .spawn(move || {
            let runtime = build_cli_runtime(runtime_mode);
            runtime.block_on(async_main(raw_args, runtime_mode));
            // Drain any queued OTLP exports while the tokio runtime
            // is still alive. The auto-registered `OtelSink` uses a
            // batch processor with `runtime::Tokio`; if we let the
            // runtime drop before this call, in-flight spans never
            // reach the configured collector. No-op when OTel is not
            // configured.
            if let Err(error) = harn_vm::events::shutdown_otel_sink() {
                eprintln!("[harn] OTel exporter shutdown failed: {error}");
            }
        })
        .unwrap_or_else(|error| {
            eprintln!("failed to start CLI runtime thread: {error}");
            process::exit(1);
        });

    if let Err(payload) = handle.join() {
        if runtime::is_broken_pipe_panic_payload(payload.as_ref()) {
            process::exit(0);
        }
        std::panic::resume_unwind(payload);
    }
}

fn install_broken_pipe_panic_hook() {
    BROKEN_PIPE_PANIC_HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if runtime::is_broken_pipe_panic_payload(info.payload()) {
                return;
            }
            previous(info);
        }));
    });
}
