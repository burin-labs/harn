//! `harn provider catalog overlay-audit` — report the entries in a
//! product-local `providers.toml` overlay that the baseline catalog already
//! covers.
//!
//! The audit itself lives in `harn_vm::llm_config`; this module is the
//! presentation and exit-code layer.

use std::collections::BTreeMap;

use harn_vm::llm_config::{audit_overlay, embedded_config, OverlayFinding, OverlayFindingKind};
use serde_json::json;

use crate::cli::ProvidersOverlayAuditArgs;

use super::artifacts::load_overlay_config;

pub(crate) fn run_overlay_audit(args: &ProvidersOverlayAuditArgs) -> Result<(), String> {
    let overlay = load_overlay_config(&args.overlay)?;
    for warning in &overlay.diagnostics {
        eprintln!("warning: {warning}");
    }
    let Some(config) = overlay.config else {
        return Err(format!("overlay {} is empty", args.overlay.display()));
    };

    let findings = audit_overlay(&embedded_config(None), &config);
    let actionable = findings.iter().filter(|f| f.is_actionable()).count();
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "overlay": args.overlay.display().to_string(),
                "actionable": actionable,
                "advisory": findings.len() - actionable,
                "findings": findings,
            }))
            .map_err(|error| format!("failed to render audit JSON: {error}"))?
        );
    } else {
        print!("{}", render(&args.overlay.display().to_string(), &findings));
    }

    if args.check && actionable > 0 {
        return Err(format!(
            "{}: {actionable} overlay entr{} the baseline catalog already covers",
            args.overlay.display(),
            if actionable == 1 { "y" } else { "ies" }
        ));
    }
    Ok(())
}

/// Group findings by the fix they call for, because the fix is what a reader
/// acts on — an operator works through one group at a time, not one entry.
fn render(overlay_path: &str, findings: &[OverlayFinding]) -> String {
    if findings.is_empty() {
        return format!("{overlay_path}: every overlay entry still earns its keep\n");
    }

    let mut groups: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for finding in findings {
        groups
            .entry(group_of(finding))
            .or_default()
            .push(describe(finding));
    }

    let mut out = format!("{overlay_path}: {} findings\n", findings.len());
    for (heading, entries) in groups {
        out.push_str(&format!("\n  {heading} ({})\n", entries.len()));
        for entry in entries {
            out.push_str(&entry);
        }
    }
    out
}

fn group_of(finding: &OverlayFinding) -> &'static str {
    match (&finding.kind, finding.preserves_catalog()) {
        (OverlayFindingKind::Redundant { .. }, true) => {
            "delete — the merged catalog is identical without them"
        }
        (OverlayFindingKind::Redundant { .. }, false) => {
            "delete — a verbatim copy that also drops baseline fields"
        }
        (OverlayFindingKind::Narrowable { .. }, true) => {
            "narrow to a field patch — every other field then tracks upstream"
        }
        (OverlayFindingKind::Narrowable { .. }, false) => {
            "narrow to a field patch — also restores baseline fields the row drops"
        }
        (OverlayFindingKind::Dangling { .. }, _) => {
            "review — nothing in the catalog matches; may be a route that arrives later"
        }
        (OverlayFindingKind::DuplicateOfBaseline { .. }, _) => {
            "review — an ordered rule the baseline already declares"
        }
    }
}

fn describe(finding: &OverlayFinding) -> String {
    let address = finding.address();
    let restored = match finding.restored_fields() {
        Some([]) | None => String::new(),
        Some(fields) => format!("      restores {}\n", fields.join(", ")),
    };
    match &finding.kind {
        OverlayFindingKind::Redundant { .. } => format!("    {address}\n{restored}"),
        OverlayFindingKind::Dangling { target } => format!("    {address} -> missing {target}\n"),
        OverlayFindingKind::DuplicateOfBaseline { baseline_index } => format!(
            "    {address} duplicates baseline {}.{baseline_index}\n",
            finding.section
        ),
        OverlayFindingKind::Narrowable {
            patch_toml,
            inherited_fields,
            ..
        } => {
            let mut entry = format!(
                "    {address} — hands back {}\n{restored}",
                inherited_fields.join(", ")
            );
            for line in patch_toml.lines() {
                entry.push_str(&format!("      {line}\n"));
            }
            entry
        }
    }
}
