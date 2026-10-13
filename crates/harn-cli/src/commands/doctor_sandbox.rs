//! `harn doctor sandbox`: which process confinement this host actually
//! enforces, measured rather than inferred.
//!
//! Runs every case of the process-sandbox conformance contract through the
//! same process tools an agent uses, so a claim of confinement can be checked
//! before it is relied on. The detector that `harn doctor` reports says what
//! the backend believes it can do; this says what a child was observed to do.

use std::io::Write;

use harn_hostlib::sandbox::conformance::{run_conformance_with_progress, ConformanceReport};
use harn_vm::process_sandbox::conformance::Verdict;
use harn_vm::process_sandbox::enforcement::MechanismLimit;
use serde::Serialize;

use crate::json_envelope::{to_string_pretty, JsonEnvelope};

/// Stable schema version for `harn doctor sandbox --json`.
pub(crate) const DOCTOR_SANDBOX_SCHEMA_VERSION: u32 = 1;

/// The conformance report, plus how the host's mechanism falls short of the
/// backend's enforcement row (such as an older Landlock ABI).
#[derive(Serialize)]
struct DoctorSandboxReport {
    #[serde(flatten)]
    report: ConformanceReport,
    mechanism_limits: Vec<MechanismLimit>,
}

/// Run the conformance cases and print the report. Returns the exit code:
/// zero only when every case that applies on this platform was measured and
/// holds.
pub(crate) fn run(json: bool) -> i32 {
    let report = run_conformance_with_progress(|progress| {
        eprintln!("{}", progress.receipt_line());
        let _ = std::io::stderr().flush();
    });
    let report = DoctorSandboxReport {
        mechanism_limits: harn_vm::process_sandbox::active_backend_mechanism_limits(),
        report,
    };
    let holds = report.report.holds();
    if json {
        println!("{}", to_string_pretty(&envelope(&report)));
    } else {
        print!("{}", render(&report));
    }
    i32::from(!holds)
}

fn envelope(report: &DoctorSandboxReport) -> JsonEnvelope<&DoctorSandboxReport> {
    if report.report.holds() {
        return JsonEnvelope::ok(DOCTOR_SANDBOX_SCHEMA_VERSION, report);
    }
    let mut envelope = JsonEnvelope::err(
        DOCTOR_SANDBOX_SCHEMA_VERSION,
        "sandbox_conformance_failed",
        "Sandbox confinement did not hold for every applicable case.",
    );
    envelope.data = Some(report);
    envelope
}

fn render(doctor: &DoctorSandboxReport) -> String {
    let report = &doctor.report;
    let mut out = format!(
        "Process sandbox: backend={} mechanism={} enforcing={}\n",
        report.backend, report.filesystem_mechanism, report.enforcing
    );
    for limit in &doctor.mechanism_limits {
        out.push_str(&format!(
            "  limit: {} ({})\n",
            limit.as_str(),
            limit.description()
        ));
    }
    out.push('\n');
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
