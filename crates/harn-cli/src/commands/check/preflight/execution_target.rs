use std::path::Path;

use harn_parser::{DiagnosticCode as Code, SNode};

use super::super::harness_receiver::harness_method_receiver;
use super::{literal_string, resolve_source_relative, PreflightDiagnostic};

/// Whether a method call is one of the directory-scoped process methods on an
/// explicit Harness receiver.
pub(super) fn is_process_execution_method(object: &SNode, method: &str) -> bool {
    matches!(
        harness_method_receiver(object),
        Some(receiver)
            if receiver.capability == harn_builtin_meta::CapabilityId::Process
                && matches!(method, "exec_at" | "shell_at")
    )
}

/// Report a literal execution directory that does not exist, for either
/// spelling of the directory-scoped process calls.
pub(super) fn scan_execution_dir_preflight(
    args: &[SNode],
    file_path: &Path,
    source: &str,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    let Some(dir) = args.first().and_then(literal_string) else {
        return;
    };
    let resolved = resolve_source_relative(file_path, &dir);
    if super::super::result_cache::probe_is_dir(&resolved) {
        return;
    }
    diagnostics.push(PreflightDiagnostic {
        code: Code::ExecutionTargetMissing,
        path: file_path.display().to_string(),
        source: source.to_string(),
        span: args[0].span,
        message: format!(
            "preflight: execution directory '{}' does not exist at {}",
            dir,
            resolved.display()
        ),
        help: Some(
            "use a source-relative directory that exists at preflight time, or create it before execution"
                .to_string(),
        ),
        tags: None,
    });
}
