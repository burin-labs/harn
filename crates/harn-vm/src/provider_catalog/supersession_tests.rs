//! Tests for the integrity of `superseded_by` migration pointers.
//!
//! A supersession pointer is only useful if following it lands on a route that
//! still works. Two ways it can rot: the target never resolves at all, or it
//! resolves onto a row that has since been deprecated itself. Both warn rather
//! than error, because the row stays usable either way — but both have to be
//! visible, since nothing downstream re-checks a pointer once it is written.

use super::tests::install_overlay;
use super::*;

#[test]
fn dangling_superseded_by_and_unknown_serving_tier_status_warn() {
    let _guard = install_overlay(
        r#"
[providers.warn_co]
display_name = "Warn Co"
base_url = "https://example.test/v1"
auth_style = "bearer"
auth_env = "WARN_API_KEY"
chat_endpoint = "/chat/completions"

[models."warn/old"]
name = "Warn Old"
provider = "warn_co"
context_window = 4096
deprecated = true
deprecation_note = "Retiring soon."
superseded_by = "warn/does-not-exist"
serving_tiers = [
  { id = "fast", mode = "synchronous", economics = "premium", request = { param = "speed", value = "fast" }, status = "turbo", pricing = { input_per_mtok = 1.0, output_per_mtok = 2.0 } },
]
"#,
    );
    let report = validate_current();
    assert!(
        report
            .warnings
            .iter()
            .any(|message| message.contains("superseded_by warn/does-not-exist")),
        "expected dangling superseded_by warning, got {:?}",
        report.warnings
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|message| message.contains("serving_tiers[fast].status")
                && message.contains("turbo")),
        "expected serving_tiers status warning, got {:?}",
        report.warnings
    );
}

#[test]
fn superseded_by_pointing_at_a_deprecated_row_warns() {
    // The dangling case above is caught because the target does not resolve.
    // This one resolves — which is exactly why it is easy to miss: every
    // downstream consumer reads `warn/dead-target` as a live migration route
    // and sends the caller from one retired model onto another.
    let _guard = install_overlay(
        r#"
[providers.warn_co]
display_name = "Warn Co"
base_url = "https://example.test/v1"
auth_style = "bearer"
auth_env = "WARN_API_KEY"
chat_endpoint = "/chat/completions"

[models."warn/dead-target"]
name = "Warn Dead Target"
provider = "warn_co"
context_window = 4096
deprecated = true
deprecation_note = "Also retiring."

[models."warn/chained"]
name = "Warn Chained"
provider = "warn_co"
context_window = 4096
deprecated = true
deprecation_note = "Retiring soon."
superseded_by = "warn/dead-target"

[models."warn/live-target"]
name = "Warn Live Target"
provider = "warn_co"
context_window = 4096

[models."warn/healthy"]
name = "Warn Healthy"
provider = "warn_co"
context_window = 4096
deprecated = true
deprecation_note = "Retiring soon."
superseded_by = "warn/live-target"
"#,
    );
    let report = validate_current();
    assert!(
        report.warnings.iter().any(|message| {
            message.contains("warn/chained")
                && message.contains("superseded_by warn/dead-target")
                && message.contains("itself deprecated")
        }),
        "expected dead-chain superseded_by warning, got {:?}",
        report.warnings
    );
    // The healthy pointer resolves to an active row and must stay silent, so
    // the check cannot pass by warning on every supersession pointer.
    assert!(
        !report
            .warnings
            .iter()
            .any(|message| message.contains("warn/healthy")),
        "active migration target must not warn, got {:?}",
        report.warnings
    );
}
