//! `harness.verdict.*` issuance validator — the host authority that decides
//! whether a real host-executed test run earns a positive verdict.
//!
//! `issue(result_handle)` resolves the opaque handle of a real `run_test`
//! execution to the host-owned execution
//! record, and returns a PLAIN dict
//! `{outcome, passed, total, artifact_id, artifact_hash, subject?, detail}`
//! built from the disposition the host FROZE at execution time. The VM harness
//! method (`crate::vm::methods::harness`) mints the opaque, non-serializable
//! `VerdictReceipt` from this dict on a `"pass"` outcome — the receipt is never
//! constructed here, so it never crosses the hostlib response-schema boundary.
//!
//! A caller cannot assert a pass, choose the positive-authority command, or —
//! the class the earlier path-based design
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
use crate::tools::inspect_test_results::{get_run, AuthorizedTestPlanIdentity};
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

    // GATE 1 — handle resolves. The store's sole writer is a real `run_test`
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
            run.artifacts.authorized_test_plan.as_ref(),
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
            run.artifacts.authorized_test_plan.as_ref(),
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
            run.artifacts.authorized_test_plan.as_ref(),
            subject.as_deref(),
            "host execution produced no recognized test results",
        ));
    };
    let total = summary.passed + summary.failed + summary.skipped;
    if summary.failed > 0 {
        return Ok(outcome_dict(
            "fail",
            summary.passed,
            total,
            &result_handle,
            &run.content_hash,
            owner,
            run.artifacts.authorized_test_plan.as_ref(),
            subject.as_deref(),
            "host execution reported failing tests",
        ));
    }
    if summary.passed == 0 {
        return Ok(outcome_dict(
            "unavailable",
            0,
            total,
            &result_handle,
            &run.content_hash,
            owner,
            run.artifacts.authorized_test_plan.as_ref(),
            subject.as_deref(),
            "host execution reported zero passing tests",
        ));
    }

    // GATE 4 — positive evidence must come from the plan selected by the host's
    // workspace discovery boundary. Explicit caller argv is still useful for
    // diagnostics and may produce a red verdict, but passing-looking output
    // from an arbitrary command is not test authority and cannot mint PASS.
    let Some(plan) = run.artifacts.authorized_test_plan.as_ref() else {
        return Ok(outcome_dict(
            "unavailable",
            0,
            total,
            &result_handle,
            &run.content_hash,
            owner,
            None,
            subject.as_deref(),
            "execution was not produced by a host-discovered test plan bound to the active workspace",
        ));
    };
    Ok(outcome_dict(
        "pass",
        summary.passed,
        total,
        &result_handle,
        &run.content_hash,
        owner,
        Some(plan),
        subject.as_deref(),
        "",
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
    plan: Option<&AuthorizedTestPlanIdentity>,
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
    map.put_opt_str("plan_id", plan.map(|identity| identity.plan_id.as_str()));
    map.put_opt_str(
        "workspace_hash",
        plan.map(|identity| identity.workspace_hash.as_str()),
    );
    map.put_opt_str(
        "command_hash",
        plan.map(|identity| identity.command_hash.as_str()),
    );
    map.put_opt_str("subject", subject);
    map.put_str("detail", detail);
    VmValue::dict_map(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::inspect_test_results::{store_run, RawArtifacts, TestSummaryData};
    use harn_lexer::Lexer;
    use harn_parser::Parser;
    use harn_vm::orchestration::RunExecutionRecord;
    use harn_vm::{enter_execution_scope, mint_execution_scope, register_vm_stdlib, Compiler, Vm};
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::TempDir;

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
            authorized_test_plan: None,
        }
    }

    fn cargo_fixture(parent: &Path, name: &str) -> TempDir {
        let dir = tempfile::Builder::new()
            .prefix(name)
            .tempdir_in(parent)
            .expect("fixture dir");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[lib]\npath = \"lib.rs\"\n"
            ),
        )
        .expect("fixture manifest");
        std::fs::write(
            dir.path().join("lib.rs"),
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn green() {}\n}\n",
        )
        .expect("fixture source");
        dir
    }

    fn run_test_in(cwd: &Path, argv: Option<Vec<&str>>, filter: Option<&str>) -> VmValue {
        let mut req = DictMap::new();
        req.put_str("cwd", cwd.to_string_lossy());
        if let Some(argv) = argv {
            req.put(
                "argv",
                VmValue::List(Arc::new(argv.into_iter().map(VmValue::string).collect())),
            );
        }
        req.put_opt_str("filter", filter);
        crate::tools::run_test::handle(&[VmValue::dict_map(req)]).expect("run_test ok")
    }

    fn result_handle(result: &VmValue) -> String {
        result
            .as_dict()
            .and_then(|dict| dict.get("result_handle"))
            .map(|handle| handle.as_str_cow().into_owned())
            .expect("run_test returns a result_handle")
    }

    fn compile(source: &str) -> harn_vm::Chunk {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().expect("tokenize");
        let mut parser = Parser::new(tokens);
        let program = parser.parse().expect("parse");
        Compiler::new().compile(&program).expect("compile")
    }

    fn harn_string(value: &Path) -> String {
        value
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    }

    fn overlap_vm(barrier: Arc<tokio::sync::Barrier>) -> Vm {
        let mut vm = Vm::new();
        register_vm_stdlib(&mut vm);
        let _ = crate::install_default(&mut vm);
        vm.register_async_builtin("__test_overlap", move |_ctx, _args| {
            let barrier = barrier.clone();
            async move {
                barrier.wait().await;
                Ok(VmValue::Nil)
            }
        });
        vm
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

    /// EXECUTED NEGATIVE: arbitrary caller argv can print perfect passing test
    /// prose, but it has no host-discovered plan identity and cannot mint PASS.
    #[cfg(unix)]
    #[test]
    fn arbitrary_passing_command_cannot_issue_pass() {
        let _scope = enter_execution_scope(mint_execution_scope());
        let cwd = std::env::current_dir().expect("cwd");
        let res = run_test_in(
            &cwd,
            Some(vec![
                "sh",
                "-c",
                "printf 'running 1 test\\ntest a ... ok\\n\\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\\n'",
            ]),
            None,
        );
        let handle = result_handle(&res);
        let out = verdict_issue_builtin(&issue_arg(&handle)).expect("issue ok");
        assert_eq!(outcome_of(&out), "unavailable");
    }

    /// EXECUTED POSITIVE: omitting argv delegates plan selection to the host's
    /// workspace discovery boundary. The exact discovered Cargo plan executes,
    /// records measured green results, and can issue inside its owning run.
    #[test]
    fn host_discovered_test_plan_issues_pass() {
        let workspace = TempDir::new().expect("workspace");
        let fixture = cargo_fixture(workspace.path(), "verdict_positive");
        harn_vm::stdlib::process::set_thread_execution_context(Some(RunExecutionRecord {
            cwd: Some(fixture.path().to_string_lossy().into_owned()),
            project_root: Some(fixture.path().to_string_lossy().into_owned()),
            ..RunExecutionRecord::default()
        }));
        let _scope = enter_execution_scope(mint_execution_scope());
        let res = run_test_in(fixture.path(), None, None);
        let out = verdict_issue_builtin(&issue_arg(&result_handle(&res))).expect("issue ok");
        harn_vm::stdlib::process::set_thread_execution_context(None);
        assert_eq!(outcome_of(&out), "pass");
    }

    /// EXECUTED NEGATIVE: a caller-selected filter narrows the discovered plan
    /// to a partial diagnostic run, so even a green result is non-authoritative.
    #[test]
    fn filtered_discovered_test_plan_cannot_issue_pass() {
        let workspace = TempDir::new().expect("workspace");
        let fixture = cargo_fixture(workspace.path(), "verdict_filtered");
        harn_vm::stdlib::process::set_thread_execution_context(Some(RunExecutionRecord {
            cwd: Some(fixture.path().to_string_lossy().into_owned()),
            project_root: Some(fixture.path().to_string_lossy().into_owned()),
            ..RunExecutionRecord::default()
        }));
        let _scope = enter_execution_scope(mint_execution_scope());
        let res = run_test_in(fixture.path(), None, Some("green"));
        let out = verdict_issue_builtin(&issue_arg(&result_handle(&res))).expect("issue ok");
        harn_vm::stdlib::process::set_thread_execution_context(None);
        assert_eq!(outcome_of(&out), "unavailable");
    }

    /// Deterministic top-level overlap oracle. Both VMs execute and record their
    /// own host-discovered plan, then rendezvous while both top-level futures are
    /// pending on one thread. Per-poll ambient isolation must restore each VM's
    /// owner before issuance, so both receipts mint under their producing run.
    #[tokio::test(flavor = "current_thread")]
    async fn overlapping_top_level_vms_issue_only_their_own_receipts() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let workspace = TempDir::new().expect("workspace");
                let fixture = cargo_fixture(workspace.path(), "verdict_overlap");
                harn_vm::stdlib::process::set_thread_execution_context(Some(RunExecutionRecord {
                    cwd: Some(fixture.path().to_string_lossy().into_owned()),
                    project_root: Some(fixture.path().to_string_lossy().into_owned()),
                    ..RunExecutionRecord::default()
                }));

                let source = |cwd: &Path| {
                    format!(
                        r#"
import {{ verdict_disposition, verdict_from_run }} from "std/agent/verdict"

pipeline default(_task) {{
  hostlib_enable("tools:deterministic")
  const run = hostlib_tools_run_test({{cwd: "{}", timeout_ms: 120000}})
  __test_overlap()
  return verdict_disposition(verdict_from_run(harness, run))
}}
"#,
                        harn_string(cwd)
                    )
                };
                let chunk_a = compile(&source(fixture.path()));
                let chunk_b = compile(&source(fixture.path()));
                let barrier = Arc::new(tokio::sync::Barrier::new(2));
                let mut vm_a = overlap_vm(barrier.clone());
                let mut vm_b = overlap_vm(barrier);

                let (result_a, result_b) =
                    tokio::join!(vm_a.execute(&chunk_a), vm_b.execute(&chunk_b));
                harn_vm::stdlib::process::set_thread_execution_context(None);
                assert_eq!(result_a.expect("vm A").display(), "pass");
                assert_eq!(result_b.expect("vm B").display(), "pass");
            })
            .await;
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
