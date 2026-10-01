//! `harn doctor sandbox`: which process confinement this host actually
//! enforces, measured rather than inferred.
//!
//! Runs every case of the process-sandbox conformance contract through the
//! same process tools an agent uses, so a claim of confinement can be checked
//! before it is relied on. The detector that `harn doctor` reports says what
//! the backend believes it can do; this says what a child was observed to do.

use harn_hostlib::sandbox::conformance::{run_conformance, ConformanceReport};
use harn_vm::process_sandbox::conformance::Verdict;

use crate::json_envelope::{to_string_pretty, JsonEnvelope};

/// Stable schema version for `harn doctor sandbox --json`.
pub(crate) const DOCTOR_SANDBOX_SCHEMA_VERSION: u32 = 1;

/// Run the conformance cases and print the report. Returns the exit code:
/// zero only when every case that applies on this platform was measured and
/// holds.
pub(crate) fn run(json: bool) -> i32 {
    let report = run_conformance();
    let holds = report.failing().is_empty()
        && report.not_measured().is_empty()
        && report.not_enforced().is_empty();
    if json {
        println!(
            "{}",
            to_string_pretty(&JsonEnvelope::ok(DOCTOR_SANDBOX_SCHEMA_VERSION, &report))
        );
    } else {
        print!("{}", render(&report));
    }
    i32::from(!holds)
}

fn render(report: &ConformanceReport) -> String {
    let mut out = format!(
        "Process sandbox: backend={} mechanism={} enforcing={}\n\n",
        report.backend, report.filesystem_mechanism, report.enforcing
    );
    for case in &report.cases {
        let (status, note) = match &case.verdict {
            Verdict::Conforms => ("ok", String::new()),
            Verdict::Escaped { target } => ("ESCAPED", target.clone()),
            Verdict::Overrefused { target, observed } => {
                ("OVERREFUSED", format!("{target} ({observed:?})"))
            }
            Verdict::Contradiction { reason, .. } => ("CONTRADICTION", reason.clone()),
            Verdict::ProbeBroken { reason } => ("PROBE BROKEN", reason.clone()),
            Verdict::NotMeasured { reason } => ("not measured", reason.clone()),
            Verdict::NotEnforced { target } => ("not enforced", target.clone()),
            Verdict::NotApplicable { reason } => ("n/a", reason.clone()),
        };
        out.push_str(&format!("  {status:<14} {}", case.case));
        if !note.is_empty() {
            out.push_str(&format!("  {note}"));
        }
        out.push('\n');
    }
    let failed = report.failing().len();
    let not_measured = report.not_measured().len();
    let not_enforced = report.not_enforced().len();
    out.push_str(&format!(
        "\n{} cases: {} hold, {failed} failed, {not_measured} not measured, \
         {not_enforced} not enforced\n",
        report.cases.len(),
        report.conforming(),
    ));
    if not_enforced > 0 {
        out.push_str(
            "This backend does not confine the not-enforced cases, and says so; do not rely on \
             it for them.\n",
        );
    }
    if failed == 0 && not_measured > 0 {
        out.push_str(
            "Confinement is not enforced on this host for the unmeasured cases; do not rely on it.\n",
        );
    }
    out
}
