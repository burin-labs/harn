//! `harness.verdict.*` issuance validator — the host authority that decides
//! whether a real host-executed test run earns a positive verdict.
//!
//! `issue(result_handle)` resolves the opaque handle of a real `run_test`
//! execution (the ONLY producer of that handle) to the host-owned execution
//! record, and returns a PLAIN dict
//! `{outcome, passed, total, artifact_id, artifact_hash, subject?, detail}`
//! built from the disposition the host FROZE at execution time. The VM harness
//! method (`crate::vm::methods::harness`) mints the opaque, non-serializable
//! `VerdictReceipt` from this dict on a `"pass"` outcome — the receipt is never
//! constructed here, so it never crosses the hostlib response-schema boundary.
//!
//! A caller cannot assert a pass, and — the class the earlier path-based design
//! missed — a caller cannot FABRICATE the evidence either: issuance reads no
//! caller-supplied filesystem bytes. It consumes only a `result_handle` that
//! resolves in the host-owned execution store, whose disposition the host
//! computed from bytes IT captured. A file written with `harness.fs.write_text`
//! has no handle; a hand-authored handle string resolves to nothing; a mutated
//! artifact cannot change the frozen summary. Provenance, not just type, is
//! unforgeable.

use harn_vm::value::DictMap;
use harn_vm::{VmDictExt, VmValue};

use crate::error::HostlibError;
use crate::registry::{BuiltinRegistry, HostlibCapability};
use crate::tools::inspect_test_results::get_run;
use crate::tools::payload::{optional_string, require_dict_arg, require_string};

/// The registered builtin name; must match `harness_verdict_ambient("issue")`
/// in `harn_parser::harness_methods`.
pub const ISSUE_BUILTIN: &str = "__harness_verdict_issue";

/// Hostlib capability exposing the verdict issuance validator.
pub struct VerdictCapability;

impl HostlibCapability for VerdictCapability {
    fn module_name(&self) -> &'static str {
        "verdict"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        // Resolves a host-owned execution handle (never a caller path), but stays
        // on the deterministic-tools gate: issuing a verdict is part of the same
        // opt-in tool surface as the `run_test` that produced the handle.
        registry.register_gated_fn("verdict", ISSUE_BUILTIN, "issue", verdict_issue_builtin);
    }
}

fn verdict_issue_builtin(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let map = require_dict_arg(ISSUE_BUILTIN, args)?;
    let result_handle = require_string(ISSUE_BUILTIN, &map, "result_handle")?;
    let subject = optional_string(ISSUE_BUILTIN, &map, "subject")?;

    // GATE 1 — handle resolves. The store's SOLE writer is a real `run_test`
    // spawn (`store_run`), so a caller-authored file, a fabricated handle
    // string, or a mock result never resolves here.
    let Some(run) = get_run(&result_handle) else {
        return Ok(outcome_dict(
            "unavailable",
            0,
            0,
            &result_handle,
            "",
            None,
            subject.as_deref(),
            "no host execution recorded under this result handle; a positive verdict requires a real run_test execution",
        ));
    };
    let owner = run.execution_scope.as_deref();

    // GATE 2 — EXECUTION SCOPE (fail-closed). The run that PRODUCED this evidence
    // must still be the active owner. No active scope, or an active scope that
    // differs from the one recorded at execution time, cannot mint — this is
    // what stops an old green handle from blessing a later, different run
    // (the cross-run replay class). There is no sentinel fallback.
    let active = harn_vm::current_execution_scope();
    let scope_ok = matches!((&active, &run.execution_scope), (Some(a), Some(o)) if a == o);
    if !scope_ok {
        return Ok(outcome_dict(
            "unavailable",
            0,
            0,
            &result_handle,
            &run.content_hash,
            owner,
            subject.as_deref(),
            "result handle belongs to a different or ended execution; a positive verdict must be issued within the run that produced it",
        ));
    }

    // GATE 3 — the host must have OBSERVED the process succeed. A nonzero exit
    // cannot mint a pass even when the captured output looks passing (e.g. a
    // build that failed after echoing stale green test text).
    if run.artifacts.exit_code != 0 {
        return Ok(outcome_dict(
            "fail",
            0,
            0,
            &result_handle,
            &run.content_hash,
            owner,
            subject.as_deref(),
            "host execution exited nonzero",
        ));
    }

    // The disposition is the host's OWN, frozen at execution time from bytes it
    // captured — never re-derived here from mutable input. `None` means the real
    // execution produced nothing a parser recognized: honestly unavailable.
    let Some(summary) = run.summary else {
        return Ok(outcome_dict(
            "unavailable",
            0,
            0,
            &result_handle,
            &run.content_hash,
            owner,
            subject.as_deref(),
            "host execution produced no recognized test results",
        ));
    };
    let total = summary.passed + summary.failed + summary.skipped;
    let (outcome, detail) = if summary.failed > 0 {
        ("fail", "host execution reported failing tests")
    } else if summary.passed > 0 {
        ("pass", "")
    } else {
        ("unavailable", "host execution reported zero passing tests")
    };
    Ok(outcome_dict(
        outcome,
        summary.passed,
        total,
        &result_handle,
        &run.content_hash,
        owner,
        subject.as_deref(),
        detail,
    ))
}

#[allow(clippy::too_many_arguments)]
fn outcome_dict(
    outcome: &str,
    passed: u32,
    total: u32,
    artifact_id: &str,
    artifact_hash: &str,
    execution_scope: Option<&str>,
    subject: Option<&str>,
    detail: &str,
) -> VmValue {
    let mut map = DictMap::new();
    map.put_str("outcome", outcome);
    map.put_int("passed", i64::from(passed));
    map.put_int("total", i64::from(total));
    map.put_str("artifact_id", artifact_id);
    map.put_str("artifact_hash", artifact_hash);
    // The producing-execution owner the receipt binds to (VM-side mint reads it).
    map.put_opt_str("execution_scope", execution_scope);
    map.put_opt_str("subject", subject);
    map.put_str("detail", detail);
    VmValue::dict_map(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::inspect_test_results::{store_run, RawArtifacts, TestSummaryData};
    use harn_vm::{enter_execution_scope, mint_execution_scope};
    use std::sync::Arc;

    /// Build the `issue` arg — a single dict `{result_handle}` — the way the VM
    /// dispatcher passes it.
    fn issue_arg(result_handle: &str) -> Vec<VmValue> {
        let mut map = DictMap::new();
        map.put_str("result_handle", result_handle);
        vec![VmValue::dict_map(map)]
    }

    fn outcome_of(v: &VmValue) -> String {
        v.as_dict()
            .and_then(|d| d.get("outcome"))
            .map(|o| o.as_str_cow().into_owned())
            .unwrap_or_default()
    }

    fn artifacts(stdout: &str, exit_code: i32) -> RawArtifacts {
        RawArtifacts {
            stdout: stdout.to_string(),
            stderr: String::new(),
            exit_code,
            junit_path: None,
            ecosystem: None,
            argv: Vec::new(),
        }
    }

    /// Record a run under `scope`, dropping the scope guard afterward — the owner
    /// is frozen into the `StoredRun` at record time. Used to stage the
    /// cross-scope / no-active-scope NEGATIVES (the POSITIVE below deliberately
    /// drives the real execution path instead).
    fn record_in_scope(
        scope: &Arc<str>,
        arts: RawArtifacts,
        summary: Option<TestSummaryData>,
    ) -> String {
        let _g = enter_execution_scope(scope.clone());
        store_run(arts, summary)
    }

    /// PRODUCTION-PATH POSITIVE (executed): drive the REAL `run_test` builtin
    /// (real subprocess -> host-recorded store entry) and then issue INSIDE the
    /// same execution scope. A direct `store_run` fixture is intentionally NOT
    /// used here — this proves the whole production path, not just the validator.
    #[cfg(unix)]
    #[test]
    fn green_via_real_run_test_execution_issues_pass() {
        let _scope = enter_execution_scope(mint_execution_scope());
        // A real child the host spawns and observes exiting 0 with libtest-format
        // passing output.
        let mut req = DictMap::new();
        req.put(
            "argv",
            VmValue::List(Arc::new(vec![
                VmValue::string("sh"),
                VmValue::string("-c"),
                VmValue::string(
                    "printf 'running 1 test\\ntest a ... ok\\n\\ntest result: ok. \
                     1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\\n'",
                ),
            ])),
        );
        let res = crate::tools::run_test::handle(&[VmValue::dict_map(req)]).expect("run_test ok");
        let handle = res
            .as_dict()
            .and_then(|d| d.get("result_handle"))
            .map(|h| h.as_str_cow().into_owned())
            .expect("run_test returns a result_handle");
        let out = verdict_issue_builtin(&issue_arg(&handle)).expect("issue ok");
        assert_eq!(outcome_of(&out), "pass");
    }

    /// NEGATIVE: a handle the host never recorded — the shape of a caller-authored
    /// "evidence" file or a fabricated handle string — can NEVER reach a pass.
    #[test]
    fn unrecorded_handle_is_never_a_pass() {
        let _scope = enter_execution_scope(mint_execution_scope());
        let out = verdict_issue_builtin(&issue_arg("htr-deadbeef-999999")).expect("issue ok");
        assert_eq!(outcome_of(&out), "unavailable");
    }

    /// NEGATIVE (cross-run replay): a green handle recorded in run A cannot be
    /// issued while run B is active — the owner does not match the active scope.
    #[test]
    fn cross_scope_handle_is_rejected() {
        let scope_a: Arc<str> = mint_execution_scope();
        let handle = record_in_scope(
            &scope_a,
            artifacts("test result: ok. 2 passed; 0 failed", 0),
            Some(TestSummaryData {
                passed: 2,
                failed: 0,
                skipped: 0,
            }),
        );
        let _scope_b = enter_execution_scope(mint_execution_scope());
        let out = verdict_issue_builtin(&issue_arg(&handle)).expect("issue ok");
        assert_eq!(outcome_of(&out), "unavailable");
    }

    /// NEGATIVE (fail-closed): a green handle cannot be issued with NO active
    /// scope — issuance outside any owning run is refused.
    #[test]
    fn no_active_scope_is_rejected() {
        let scope_a: Arc<str> = mint_execution_scope();
        let handle = record_in_scope(
            &scope_a,
            artifacts("test result: ok. 1 passed; 0 failed", 0),
            Some(TestSummaryData {
                passed: 1,
                failed: 0,
                skipped: 0,
            }),
        );
        // No scope entered here.
        let out = verdict_issue_builtin(&issue_arg(&handle)).expect("issue ok");
        assert_eq!(outcome_of(&out), "unavailable");
    }

    /// NEGATIVE (exit gate): a nonzero exit with passing-looking output cannot
    /// mint a pass, even in-scope with a green-looking summary.
    #[test]
    fn nonzero_exit_with_passing_text_is_not_a_pass() {
        let _g = enter_execution_scope(mint_execution_scope());
        let handle = store_run(
            artifacts("test result: ok. 1 passed; 0 failed", 1),
            Some(TestSummaryData {
                passed: 1,
                failed: 0,
                skipped: 0,
            }),
        );
        let out = verdict_issue_builtin(&issue_arg(&handle)).expect("issue ok");
        assert_eq!(outcome_of(&out), "fail");
    }

    /// A real execution the host observed RED is a fail — read from the host's
    /// frozen summary, not asserted by the caller.
    #[test]
    fn red_host_execution_issues_fail() {
        let _g = enter_execution_scope(mint_execution_scope());
        let handle = store_run(
            artifacts("test result: FAILED. 1 passed; 1 failed", 1),
            Some(TestSummaryData {
                passed: 1,
                failed: 1,
                skipped: 0,
            }),
        );
        let out = verdict_issue_builtin(&issue_arg(&handle)).expect("issue ok");
        assert_eq!(outcome_of(&out), "fail");
    }

    /// A recorded execution whose output parsed to nothing (no host summary) is
    /// unavailable, never a fabricated pass.
    #[test]
    fn recorded_but_unparsed_execution_is_unavailable() {
        let _g = enter_execution_scope(mint_execution_scope());
        let handle = store_run(artifacts("not test output", 0), None);
        let out = verdict_issue_builtin(&issue_arg(&handle)).expect("issue ok");
        assert_eq!(outcome_of(&out), "unavailable");
    }
}
