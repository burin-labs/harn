//! The type checker must accept references to every ambient global the ACP
//! session executor injects, and must still reject genuinely unknown ones.
//!
//! Both tests run with resolved-import checking active (`check_source_with_imports`)
//! because `HARN-NAM-001` (unresolved value identifier) only fires in that mode —
//! the same mode `harn check --workspace` uses, which is where the missing
//! allowlist entry originally surfaced.

use super::{check_source_with_imports, DiagnosticSeverity};
use crate::acp_ambient_globals::AcpAmbientGlobal;

/// A pipeline that imports one std value (to activate resolved-import checking)
/// and then references each identifier in `refs`.
fn source_referencing(refs: &str) -> String {
    format!(
        "import {{ parse_args }} from \"std/cli\"\n\
         pipeline t(task) {{\n\
        \x20 const _args = parse_args(argv, {{}})\n\
         {refs}\n\
         }}"
    )
}

#[test]
fn acp_session_prompt_globals_resolve() {
    // Reference every ambient global the ACP session executor binds. None may
    // raise an unresolved-identifier error: the checker allowlist is derived
    // from the same `AcpAmbientGlobal::ALL` the executor binds from, so the two
    // cannot drift.
    let refs = AcpAmbientGlobal::ALL
        .iter()
        .map(|global| format!("  const _{name} = {name}", name = global.name()))
        .collect::<Vec<_>>()
        .join("\n");
    let diags = check_source_with_imports(&source_referencing(&refs), &["parse_args"]);
    let errs: Vec<&String> = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| &d.message)
        .collect();
    assert!(
        errs.is_empty(),
        "ACP ambient globals should resolve without error, got: {errs:?}"
    );
}

#[test]
fn unknown_global_still_reports_unresolved() {
    // Negative control: proves the resolver actually rejects unknown identifiers
    // in this mode, so the positive test is a real guarantee and not a no-op.
    let diags = check_source_with_imports(
        &source_referencing("  const _x = not_an_ambient_global"),
        &["parse_args"],
    );
    assert!(
        diags.iter().any(|d| {
            d.severity == DiagnosticSeverity::Error && d.message.contains("not_an_ambient_global")
        }),
        "an unknown global identifier must still raise an unresolved-identifier error"
    );
}
