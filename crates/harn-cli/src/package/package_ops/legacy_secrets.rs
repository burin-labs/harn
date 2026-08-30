use super::*;

/// Whether a legacy `required_secrets` entry fails the check or only reports.
///
/// This is the lever for step 3 of the deprecation series in harn#7587, and it
/// is deliberately not an error yet.
///
/// `harn package verify .` runs this same check path
/// (`commands/package_verify.rs:402`) and maps `report.errors` into verify
/// failures, and `harn-bump-fleet` validates every fleet repository with
/// exactly that command (`fleet.toml:163`, `fleet.toml:491`,
/// `.github/workflows/bump-harn.yml:24`). A census of the 36 repositories in
/// `fleet.toml` on 2026-08-29 found 19 of the 29 that carry a `harn.toml`
/// still writing the legacy bare-string form and zero writing the typed form,
/// so promoting this to an error today would fail all 19 on the next bump.
/// That is the outage #7578 just repaired.
///
/// Flip this to `Severity::Error` only after the fleet census reports zero
/// bare-string manifests through a proof that can distinguish a measured zero
/// from a scope it never examined (see burin-labs/harn-bump-fleet#1050).
const LEGACY_SECRET_SEVERITY: Severity = Severity::Warning;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Severity {
    Warning,
    /// Only the step-3 flip and `raising_the_severity_moves_the_diagnostic_into_errors`
    /// construct this today, so the lib build sees it as dead. Keeping the
    /// variant is the point: the deprecation is a one-word edit to
    /// [`LEGACY_SECRET_SEVERITY`] with a test already covering the arm, rather
    /// than a rewrite of the reporting path under time pressure. Remove the
    /// allow when the flip lands.
    #[allow(dead_code)]
    Error,
}

/// One provider that spells at least one `required_secrets` entry as a bare id
/// string instead of a `{ id, direction }` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LegacySecretFinding {
    /// The provider's declared `id`, when it has one. Used for the diagnostic
    /// field so an author can find the offending block.
    pub provider: Option<String>,
    /// Positional index into `[[providers]]`, so a provider with no `id` is
    /// still addressable.
    pub index: usize,
    /// The bare-string entries, in declaration order.
    pub ids: Vec<String>,
}

/// Scan the raw manifest text for legacy bare-string `required_secrets`.
///
/// This reads the TOML rather than the parsed [`Manifest`] on purpose. The
/// deserializer at `package/manifest/provider_setup.rs` maps a bare string to
/// `{ id, direction: outbound }`, so by the time a `Manifest` exists the
/// spelling is erased and the two forms are indistinguishable. Recovering it
/// from the typed value would need a provenance field on
/// `ConnectorRequiredSecretManifest`, and a `#[serde(skip)]` field would
/// silently change equality: `outbound("x")` would stop comparing equal to a
/// legacy-parsed entry with the same id, across the existing suite.
///
/// A manifest that does not parse as TOML yields no findings. The caller
/// already reports the parse failure through the normal load path, and a
/// second message about secret spelling would be noise on a file that has a
/// more basic problem.
pub(crate) fn find_legacy_required_secrets(source: &str) -> Vec<LegacySecretFinding> {
    // `toml::from_str`, not `str::parse`. In toml 1.x the `FromStr` impl on
    // `Value` parses a single TOML *value*, so parsing a whole document
    // through it fails with "unexpected content, expected nothing" and this
    // function would silently report no findings on every manifest. The
    // scanner would look correct and measure nothing.
    let Ok(document) = toml::from_str::<toml::Value>(source) else {
        return Vec::new();
    };
    let Some(providers) = document.get("providers").and_then(toml::Value::as_array) else {
        return Vec::new();
    };

    let mut findings = Vec::new();
    for (index, provider) in providers.iter().enumerate() {
        let ids: Vec<String> = provider
            .get("setup")
            .and_then(|setup| setup.get("required_secrets"))
            .and_then(toml::Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        if ids.is_empty() {
            continue;
        }

        findings.push(LegacySecretFinding {
            provider: provider
                .get("id")
                .and_then(toml::Value::as_str)
                .map(str::to_string),
            index,
            ids,
        });
    }
    findings
}

/// Report legacy `required_secrets` spellings on the package being checked.
///
/// Only `harn package check` and its callers reach this, which is what keeps
/// already-published packages exempt: a package installed from the cache or
/// materialized into a workspace is loaded through
/// `lockfile/resolution.rs::read_package_manifest_from_dir`, never through the
/// check path. The exemption is a property of the code path rather than a test
/// of whether a directory sits under the cache root, and that distinction
/// matters: installed packages are frequently read from the materialized
/// workspace tree, which is not under the cache root at all, so a containment
/// test would exempt the wrong set.
pub(crate) fn validate_required_secret_spelling(
    manifest_path: &Path,
    errors: &mut Vec<PackageCheckDiagnostic>,
    warnings: &mut Vec<PackageCheckDiagnostic>,
) {
    let Ok(source) = fs::read_to_string(manifest_path) else {
        return;
    };
    record_findings(
        LEGACY_SECRET_SEVERITY,
        find_legacy_required_secrets(&source),
        errors,
        warnings,
    );
}

/// Turn findings into diagnostics at the given severity.
///
/// Severity is a parameter rather than being read from
/// [`LEGACY_SECRET_SEVERITY`] inline so the step-3 flip is exercised by a test
/// today. Otherwise the `Error` arm would be unreachable code that nobody has
/// ever run, and the deprecation would be the first time it executed.
fn record_findings(
    severity: Severity,
    findings: Vec<LegacySecretFinding>,
    errors: &mut Vec<PackageCheckDiagnostic>,
    warnings: &mut Vec<PackageCheckDiagnostic>,
) {
    for finding in findings {
        let field = match finding.provider.as_deref() {
            Some(id) => format!("[providers.setup] ({id}).required_secrets"),
            None => format!("[[providers]][{}].setup.required_secrets", finding.index),
        };
        let message = format!(
            "declares {} as bare id string{}; use the typed form so each secret states its direction, \
             for example `required_secrets = [{{ id = \"{}\", direction = \"outbound\" }}]`. \
             A bare id is read as `direction = \"outbound\"` for compatibility with already-published packages",
            finding.ids.join(", "),
            if finding.ids.len() == 1 { "" } else { "s" },
            finding.ids.first().map(String::as_str).unwrap_or("provider/secret"),
        );

        match severity {
            Severity::Error => push_error(errors, field, message),
            Severity::Warning => push_warning(warnings, field, message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY_MANIFEST: &str = r#"
[[providers]]
id = "github"

[providers.setup]
required_secrets = ["github/app-private-key", "github/webhook-secret"]
"#;

    const TYPED_MANIFEST: &str = r#"
[[providers]]
id = "github"

[providers.setup]
required_secrets = [
  { id = "github/app-private-key", direction = "outbound" },
  { id = "github/webhook-secret", direction = "inbound" },
]
"#;

    #[test]
    fn legacy_bare_strings_are_found_with_their_ids() {
        let findings = find_legacy_required_secrets(LEGACY_MANIFEST);
        assert_eq!(
            findings,
            vec![LegacySecretFinding {
                provider: Some("github".to_string()),
                index: 0,
                ids: vec![
                    "github/app-private-key".to_string(),
                    "github/webhook-secret".to_string(),
                ],
            }]
        );
    }

    #[test]
    fn the_typed_form_produces_no_finding() {
        assert!(find_legacy_required_secrets(TYPED_MANIFEST).is_empty());
    }

    /// The negative control for the scanner. A manifest with no providers, no
    /// setup, or an empty list must not report, or the lint would fire on
    /// every package in the fleet regardless of spelling and the signal would
    /// be worthless.
    #[test]
    fn manifests_without_a_legacy_spelling_produce_no_finding() {
        for source in [
            "",
            "[package]\nname = \"x\"\n",
            "[[providers]]\nid = \"github\"\n",
            "[[providers]]\nid = \"github\"\n\n[providers.setup]\nauth_type = \"token\"\n",
            "[[providers]]\nid = \"github\"\n\n[providers.setup]\nrequired_secrets = []\n",
            "this is not valid toml = = =",
        ] {
            assert!(
                find_legacy_required_secrets(source).is_empty(),
                "unexpected finding for source: {source:?}"
            );
        }
    }

    /// The scanner must find the real thing, not just the shape of it. This is
    /// the opening of `harn-github-connector` v0.8.6 at the commit the fleet
    /// pins, reproduced verbatim including the `[[providers.setup.health_checks]]`
    /// blocks that follow the secret list, because those are what a naive
    /// nested lookup would trip over.
    ///
    /// It also serves as the non-null control for the whole module: if the
    /// document ever stops parsing, every other assertion here still passes
    /// vacuously while this one fails.
    #[test]
    fn the_real_fleet_manifest_is_detected() {
        let findings = find_legacy_required_secrets(
            r#"
[package]
name = "harn-github-connector"
version = "0.8.6"

[[providers]]
id = "github"
connector = { harn = "src/webhooks/provider.harn" }
capabilities = ["webhook", "rate_limit", "pagination", "graphql", "oauth"]

[providers.setup]
auth_type = "github-app"
flow = "github-app"
required_secrets = ["github/app-private-key", "github/webhook-secret"]
setup_command = ["harn", "connect", "github"]

[[providers.setup.health_checks]]
id = "app-private-key"
kind = "secret"
secret = "github/app-private-key"

[providers.setup.recovery]
missing_auth = "Store github/app-private-key and github/webhook-secret before enabling GitHub App bindings."
"#,
        );

        assert_eq!(
            findings.len(),
            1,
            "the pinned fleet manifest must be flagged"
        );
        assert_eq!(findings[0].provider.as_deref(), Some("github"));
        assert_eq!(
            findings[0].ids,
            vec![
                "github/app-private-key".to_string(),
                "github/webhook-secret".to_string(),
            ]
        );
    }

    /// A provider with no `id` still has to be addressable, or an author
    /// cannot tell which block to fix.
    #[test]
    fn a_provider_without_an_id_is_reported_by_position() {
        let findings = find_legacy_required_secrets(
            "[[providers]]\n\n[providers.setup]\nrequired_secrets = [\"acme/token\"]\n",
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].provider, None);
        assert_eq!(findings[0].index, 0);
    }

    /// A manifest may mix the spellings. The typed entries are already
    /// compliant, so only the bare ones are reported.
    #[test]
    fn only_the_bare_entries_of_a_mixed_list_are_reported() {
        let findings = find_legacy_required_secrets(
            r#"
[[providers]]
id = "acme"

[providers.setup]
required_secrets = [
  "acme/legacy-token",
  { id = "acme/typed-token", direction = "inbound" },
]
"#,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].ids, vec!["acme/legacy-token".to_string()]);
    }

    /// This is the load-bearing assertion of the whole lint, and it is the one
    /// that must fail if someone flips the severity without doing step 2 of
    /// harn#7587 first.
    ///
    /// `harn package verify .` turns `report.errors` into verify failures and
    /// the fleet bump validates all 19 legacy connector repositories with that
    /// exact command. If a legacy entry lands in `errors`, the next fleet bump
    /// fails on every one of them, which is the outage #7578 repaired.
    #[test]
    fn a_legacy_entry_warns_and_does_not_fail_the_check() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest_path = dir.path().join("harn.toml");
        fs::write(&manifest_path, LEGACY_MANIFEST).expect("write manifest");

        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        validate_required_secret_spelling(&manifest_path, &mut errors, &mut warnings);

        assert!(
            errors.is_empty(),
            "a legacy entry must not fail the check while the fleet still ships 19 of them: {errors:?}"
        );
        assert_eq!(warnings.len(), 1, "expected exactly one warning");
        assert!(
            warnings[0].message.contains("github/app-private-key"),
            "the warning must name the offending ids: {}",
            warnings[0].message
        );
        assert!(
            warnings[0].message.contains("direction"),
            "the warning must point at the typed form: {}",
            warnings[0].message
        );
    }

    #[test]
    fn the_typed_form_produces_no_diagnostic_at_all() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest_path = dir.path().join("harn.toml");
        fs::write(&manifest_path, TYPED_MANIFEST).expect("write manifest");

        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        validate_required_secret_spelling(&manifest_path, &mut errors, &mut warnings);

        assert!(errors.is_empty());
        assert!(warnings.is_empty());
    }

    /// Prove the step-3 flip actually works, so the deprecation is not the
    /// first time the `Error` arm ever runs. Raising the severity must move
    /// the same diagnostic from `warnings` into `errors`, which is what makes
    /// `harn package check` exit 1 and `harn package verify .` report a
    /// failure.
    #[test]
    fn raising_the_severity_moves_the_diagnostic_into_errors() {
        let findings = find_legacy_required_secrets(LEGACY_MANIFEST);
        assert_eq!(findings.len(), 1, "fixture must produce a finding to route");

        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        record_findings(Severity::Error, findings, &mut errors, &mut warnings);

        assert_eq!(errors.len(), 1, "the Error arm must populate errors");
        assert!(warnings.is_empty(), "nothing should land in warnings");
        assert!(errors[0].message.contains("github/app-private-key"));
    }

    /// The severity lever is the deprecation switch. Assert its current value
    /// so flipping it is a deliberate edit that shows up in review rather than
    /// a silent behavior change.
    #[test]
    fn the_severity_lever_is_still_a_warning() {
        assert_eq!(
            LEGACY_SECRET_SEVERITY,
            Severity::Warning,
            "flipping this to Error fails `harn package verify .` on every unmigrated connector; \
             do step 2 of harn#7587 first"
        );
    }
}
