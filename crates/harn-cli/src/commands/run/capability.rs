//! `harn run` capability-profile launch surface.
//!
//! This is the launcher-side boundary for session-scoped capability grants
//! (harn#4992). The runtime (`harn_vm::security::session_grants`) owns the
//! grant *semantics* — resolution, receipts, hermetic/lane enforcement, and
//! the closed-env resolver. This module owns only the CLI parsing: it turns
//! `--capability-profile` / `--grant` flag strings into the runtime's typed,
//! value-free [`GrantSpec`] set and hands them over. harn's runtime never
//! parses flag strings.
//!
//! ## The two profiles
//!
//! A `harn run` invocation selects one of two credential postures, mirroring
//! the runtime's [`SessionProfileKind`]:
//!
//!   * **Hermetic** — no credential crosses into a spawned subprocess. The
//!     child sees the closed allowlist alone. Declared with
//!     `--capability-profile hermetic` (grants are rejected).
//!   * **Lane** — a declared, receipted grant set crosses the boundary and
//!     nothing else. Declared by passing one or more `--grant` flags (or
//!     `--capability-profile lane` explicitly).
//!
//! Passing **neither** flag leaves the legacy no-profile path untouched:
//! subprocesses inherit the launcher environment exactly as before. Selecting
//! a profile is opt-in, so no existing run changes behavior.
//!
//! ## Grant grammar
//!
//! ```text
//! --grant NAME=SOURCE[,expose=ENV_VAR]
//!   SOURCE := env:VAR_NAME
//!           | secret://ACCOUNT/KEY
//! ```
//!
//! `NAME` is how a tool asks for the credential. `SOURCE` names where it comes
//! from — a launcher environment variable (snapshotted at launch) or a
//! `secret_store` pointer (resolved lazily on exposure). The optional
//! `,expose=ENV_VAR` suffix injects the value into spawned subprocesses as
//! `ENV_VAR`; without it the grant is carried but not exposed to
//! `process.exec`.
//!
//! ```text
//! harn run --grant gh_token=secret://gh/token,expose=GH_TOKEN open_pr.harn
//! harn run --grant fireworks=env:FIREWORKS_API_KEY,expose=FIREWORKS_API_KEY lane.harn
//! ```

use clap::ValueEnum;

use harn_vm::security::{
    GrantReceipt, GrantSourceSpec, GrantSpec, SessionProfile, SessionProfileKind,
};

/// The `--capability-profile` value. A CLI-local enum so `clap`'s `ValueEnum`
/// derive stays out of `harn-vm`; it maps 1:1 to [`SessionProfileKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum CapabilityProfileArg {
    /// No credential crosses into subprocesses; grants are rejected.
    Hermetic,
    /// Credentials cross only through the declared `--grant` set.
    Lane,
}

impl From<CapabilityProfileArg> for SessionProfileKind {
    fn from(arg: CapabilityProfileArg) -> Self {
        match arg {
            CapabilityProfileArg::Hermetic => SessionProfileKind::Hermetic,
            CapabilityProfileArg::Lane => SessionProfileKind::Lane,
        }
    }
}

/// The parsed, value-free capability posture for a `harn run` invocation.
///
/// Holds the profile kind and the declared grant specs (never resolved
/// values), so it is safe to clone and pass through the run-options plumbing.
/// The env snapshot happens later, once, at [`launch`](Self::launch).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CapabilityProfileConfig {
    kind: SessionProfileKind,
    grants: Vec<GrantSpec>,
}

impl CapabilityProfileConfig {
    /// Resolve the declared posture from the `--capability-profile` / `--grant`
    /// flags. Returns `Ok(None)` when neither is present — the legacy
    /// no-profile path — and `Ok(Some(config))` for an explicit posture.
    ///
    /// The kind is a typed launch input, not an accident of which flags were
    /// passed: an explicit `--capability-profile` wins; otherwise the presence
    /// of any `--grant` selects `lane`. A hermetic posture with grants is a
    /// launch error here (the runtime enforces the same invariant — belt and
    /// suspenders).
    pub(crate) fn from_flags(
        profile: Option<CapabilityProfileArg>,
        grants: &[String],
    ) -> Result<Option<Self>, String> {
        let kind = match (profile, grants.is_empty()) {
            (None, true) => return Ok(None),
            (Some(arg), _) => SessionProfileKind::from(arg),
            (None, false) => SessionProfileKind::Lane,
        };
        if matches!(kind, SessionProfileKind::Hermetic) && !grants.is_empty() {
            return Err(format!(
                "--capability-profile hermetic forbids credentials, but {} --grant flag(s) were given",
                grants.len()
            ));
        }
        let grants = grants
            .iter()
            .map(|spec| parse_grant_spec(spec))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(Self { kind, grants }))
    }

    /// Resolve the posture into a runtime [`SessionProfile`], snapshotting each
    /// `env:` grant against the launcher environment. Fails loudly if a named
    /// launcher variable is absent or the grant set violates the profile.
    pub(crate) fn launch(&self) -> Result<SessionProfile, String> {
        SessionProfile::launch(self.kind, self.grants.clone(), &|var| {
            std::env::var(var).ok()
        })
        .map_err(|error| error.to_string())
    }
}

/// Parse one `--grant NAME=SOURCE[,expose=ENV_VAR]` string into a typed,
/// value-free [`GrantSpec`]. This is the sole flag-string parser; the runtime
/// receives only the structured result.
fn parse_grant_spec(spec: &str) -> Result<GrantSpec, String> {
    let (name, rest) = spec.split_once('=').ok_or_else(|| {
        format!(
            "invalid --grant '{spec}': expected NAME=SOURCE (for example gh_token=env:GH_TOKEN)"
        )
    })?;
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("invalid --grant '{spec}': the grant name is empty"));
    }
    // The optional `,expose=ENV` suffix. A source (`env:VAR` / `secret://a/k`)
    // never contains a comma, so the first comma unambiguously begins options.
    let (source_str, expose_as_env) = match rest.split_once(',') {
        None => (rest, None),
        Some((source, options)) => {
            let expose = options.strip_prefix("expose=").ok_or_else(|| {
                format!(
                    "invalid --grant '{spec}': unknown option '{options}' (only ',expose=ENV_VAR' is supported)"
                )
            })?;
            let expose = expose.trim();
            if expose.is_empty() {
                return Err(format!(
                    "invalid --grant '{spec}': ',expose=' needs a target environment variable name"
                ));
            }
            (source, Some(expose.to_string()))
        }
    };
    let source = parse_grant_source(source_str, spec)?;
    Ok(GrantSpec {
        name: name.to_string(),
        source,
        expose_as_env,
    })
}

/// Parse the `SOURCE` half of a grant into a typed [`GrantSourceSpec`].
fn parse_grant_source(source: &str, spec: &str) -> Result<GrantSourceSpec, String> {
    let source = source.trim();
    if let Some(var) = source.strip_prefix("env:") {
        let var = var.trim();
        if var.is_empty() {
            return Err(format!(
                "invalid --grant '{spec}': 'env:' needs a launcher variable name"
            ));
        }
        return Ok(GrantSourceSpec::Env {
            var: var.to_string(),
        });
    }
    if let Some(pointer) = source.strip_prefix("secret://") {
        let (account, key) = pointer.split_once('/').ok_or_else(|| {
            format!(
                "invalid --grant '{spec}': secret source must be secret://ACCOUNT/KEY (missing '/')"
            )
        })?;
        let (account, key) = (account.trim(), key.trim());
        if account.is_empty() || key.is_empty() {
            return Err(format!(
                "invalid --grant '{spec}': secret source must be secret://ACCOUNT/KEY (empty account or key)"
            ));
        }
        return Ok(GrantSourceSpec::SecretStore {
            account: account.to_string(),
            key: key.to_string(),
        });
    }
    Err(format!(
        "invalid --grant '{spec}': source must be 'env:VAR' or 'secret://ACCOUNT/KEY'"
    ))
}

/// Launch the declared profile, disclose it on `stderr`, and return the
/// installed ambient scope plus the non-secret receipts. The scope clears the
/// profile on drop, so the caller holds it for the run's duration; a launch
/// failure (a missing launcher variable, a grant on a hermetic profile) is
/// surfaced as an error string for the caller to fail the run loudly.
pub(crate) fn launch_scope(
    config: &CapabilityProfileConfig,
    stderr: &mut String,
) -> Result<(SessionProfileScope, Vec<GrantReceipt>), String> {
    let profile = config.launch()?;
    let receipts = profile.receipts();
    stderr.push_str(&capability_disclosure(&profile));
    Ok((SessionProfileScope::install(profile), receipts))
}

/// One line naming a launched profile's credential posture for the run's
/// stderr — the credential-facing counterpart to the sandbox root disclosure.
/// It names grants (their target env var and source kind) but never a value,
/// so a lane run is never silent about the credentials it carries.
fn capability_disclosure(profile: &SessionProfile) -> String {
    if profile.is_hermetic() {
        return "capability profile: hermetic — no credentials cross into subprocesses\n"
            .to_string();
    }
    let receipts = profile.receipts();
    if receipts.is_empty() {
        return "capability profile: lane — no grants declared\n".to_string();
    }
    let grants = receipts
        .iter()
        .map(|receipt| {
            if receipt.exposed_as_env {
                format!(
                    "{} ({}, exposed to subprocess env)",
                    receipt.name, receipt.source_kind
                )
            } else {
                format!("{} ({}, not exposed)", receipt.name, receipt.source_kind)
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("capability profile: lane — grants: {grants}\n")
}

/// Installs a resolved [`SessionProfile`] as the current task's ambient
/// capability profile and clears it on drop, so the closed-env resolver governs
/// every subprocess spawned during the run and nothing leaks past it.
pub(crate) struct SessionProfileScope;

impl SessionProfileScope {
    fn install(profile: SessionProfile) -> Self {
        harn_vm::stdlib::process::set_session_profile(Some(profile));
        Self
    }
}

impl Drop for SessionProfileScope {
    fn drop(&mut self) {
        harn_vm::stdlib::process::set_session_profile(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_vm::security::GrantSource;

    fn grant(spec: &str) -> GrantSpec {
        parse_grant_spec(spec).expect("spec should parse")
    }

    #[test]
    fn parses_env_grant_with_exposure() {
        let g = grant("fireworks=env:FIREWORKS_API_KEY,expose=FIREWORKS_API_KEY");
        assert_eq!(g.name, "fireworks");
        assert_eq!(g.expose_as_env.as_deref(), Some("FIREWORKS_API_KEY"));
        assert_eq!(
            g.source,
            GrantSourceSpec::Env {
                var: "FIREWORKS_API_KEY".to_string()
            }
        );
    }

    #[test]
    fn parses_secret_grant_with_exposure() {
        let g = grant("gh_token=secret://gh/token,expose=GH_TOKEN");
        assert_eq!(g.name, "gh_token");
        assert_eq!(g.expose_as_env.as_deref(), Some("GH_TOKEN"));
        assert_eq!(
            g.source,
            GrantSourceSpec::SecretStore {
                account: "gh".to_string(),
                key: "token".to_string(),
            }
        );
    }

    #[test]
    fn parses_grant_without_exposure() {
        let g = grant("coord=env:HARN_COORD_URL");
        assert_eq!(g.name, "coord");
        assert!(g.expose_as_env.is_none());
    }

    #[test]
    fn secret_key_may_contain_slashes() {
        let g = grant("k=secret://acct/path/to/key,expose=K");
        assert_eq!(
            g.source,
            GrantSourceSpec::SecretStore {
                account: "acct".to_string(),
                key: "path/to/key".to_string(),
            }
        );
    }

    #[test]
    fn rejects_missing_equals() {
        assert!(parse_grant_spec("env:GH_TOKEN").is_err());
    }

    #[test]
    fn rejects_empty_name() {
        assert!(parse_grant_spec("=env:X").is_err());
    }

    #[test]
    fn rejects_unknown_source_scheme() {
        let err = parse_grant_spec("t=file:/etc/passwd").unwrap_err();
        assert!(err.contains("env:VAR' or 'secret://"), "{err}");
    }

    #[test]
    fn rejects_empty_env_var() {
        assert!(parse_grant_spec("t=env:").is_err());
    }

    #[test]
    fn rejects_malformed_secret_pointer() {
        assert!(parse_grant_spec("t=secret://only-account").is_err());
        assert!(parse_grant_spec("t=secret:///key").is_err());
    }

    #[test]
    fn rejects_unknown_option_and_empty_expose() {
        assert!(parse_grant_spec("t=env:X,bogus=Y").is_err());
        assert!(parse_grant_spec("t=env:X,expose=").is_err());
    }

    #[test]
    fn from_flags_no_flags_is_legacy_path() {
        assert_eq!(CapabilityProfileConfig::from_flags(None, &[]), Ok(None));
    }

    #[test]
    fn from_flags_grants_imply_lane() {
        let config = CapabilityProfileConfig::from_flags(None, &["t=env:X".to_string()])
            .unwrap()
            .expect("grants select a lane profile");
        assert_eq!(config.kind, SessionProfileKind::Lane);
        assert_eq!(config.grants.len(), 1);
    }

    #[test]
    fn from_flags_explicit_hermetic_has_no_grants() {
        let config = CapabilityProfileConfig::from_flags(Some(CapabilityProfileArg::Hermetic), &[])
            .unwrap()
            .expect("explicit hermetic is a posture");
        assert_eq!(config.kind, SessionProfileKind::Hermetic);
        assert!(config.grants.is_empty());
    }

    #[test]
    fn from_flags_hermetic_with_grants_is_rejected() {
        let err = CapabilityProfileConfig::from_flags(
            Some(CapabilityProfileArg::Hermetic),
            &["t=env:X".to_string()],
        )
        .unwrap_err();
        assert!(err.contains("hermetic forbids"), "{err}");
    }

    #[test]
    fn launch_snapshots_env_and_records_receipt() {
        // SAFETY: single-threaded test; the var is removed before returning.
        let key = "HARN_TEST_GRANT_LAUNCH_VAR";
        std::env::set_var(key, "snapshot-value");
        let config =
            CapabilityProfileConfig::from_flags(None, &[format!("t=env:{key},expose=CHILD_VAR")])
                .unwrap()
                .unwrap();
        let profile = config.launch().expect("launch resolves the env snapshot");
        std::env::remove_var(key);

        let receipts = profile.receipts();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].name, "t");
        assert_eq!(receipts[0].source_kind, GrantSource::Env.as_str());
        assert!(receipts[0].exposed_as_env);

        let disclosure = capability_disclosure(&profile);
        assert!(disclosure.contains("lane"), "{disclosure}");
        assert!(
            disclosure.contains("exposed to subprocess env"),
            "{disclosure}"
        );
        assert!(
            !disclosure.contains("snapshot-value"),
            "disclosure leaked a value"
        );
    }

    #[test]
    fn launch_missing_env_var_fails_loudly() {
        let config = CapabilityProfileConfig::from_flags(
            None,
            &["t=env:HARN_TEST_ABSENT_GRANT_VAR".to_string()],
        )
        .unwrap()
        .unwrap();
        assert!(config.launch().is_err());
    }

    #[test]
    fn hermetic_disclosure_names_the_posture() {
        let disclosure = capability_disclosure(&SessionProfile::hermetic());
        assert!(disclosure.contains("hermetic"), "{disclosure}");
    }
}
