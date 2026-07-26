//! `harn run` environment-policy launch surface.
//!
//! This is the launcher-side boundary for session-scoped environment grants
//! (harn#4992). The runtime (`harn_vm::security::session_environment`) owns the
//! grant *semantics* — resolution, receipts, isolated/granted enforcement, and
//! the closed-env resolver. This module owns only the CLI parsing: it turns
//! `--environment-policy` / `--grant` flag strings into the runtime's typed,
//! value-free [`GrantSpec`] set and hands them over. harn's runtime never
//! parses flag strings.
//!
//! ## Policies
//!
//! Every run selects `inherited`, `isolated`, or `granted`; omission means
//! `inherited`. The resolved environment governs in-process reads and spawned
//! commands alike.
//!
//! ## Grant grammar
//!
//! ```text
//! --grant NAME=SOURCE[,expose=ENV_VAR]
//!   SOURCE := env:VAR_NAME
//!           | secret://ACCOUNT/KEY
//! ```
//!
//! `NAME` identifies the grant in receipts and diagnostics. `SOURCE` names where it comes
//! from — a launcher environment variable (snapshotted at launch) or a
//! `secret_store` pointer (resolved lazily on exposure). The optional
//! `,expose=ENV_VAR` suffix makes the value available under `ENV_VAR` to the
//! whole session, including `harness.env`, providers, and spawned subprocesses.
//! Without it the grant is carried and receipted but not exposed.
//!
//! ```text
//! harn run --grant gh_token=secret://gh/token,expose=GH_TOKEN open_pr.harn
//! harn run --grant fireworks=env:FIREWORKS_API_KEY,expose=FIREWORKS_API_KEY agent.harn
//! ```

use clap::ValueEnum;

use harn_vm::security::{
    EnvironmentPolicyKind, GrantReceipt, GrantSourceSpec, GrantSpec, SessionEnvironment,
};

/// The `--environment-policy` value. A CLI-local enum so `clap`'s `ValueEnum`
/// derive stays out of `harn-vm`; it maps 1:1 to [`EnvironmentPolicyKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum EnvironmentPolicyArg {
    /// Preserve a snapshot of the launcher environment.
    Inherited,
    /// Admit only runtime essentials; reject grants.
    Isolated,
    /// Admit runtime essentials plus declared grants.
    Granted,
}

impl From<EnvironmentPolicyArg> for EnvironmentPolicyKind {
    fn from(arg: EnvironmentPolicyArg) -> Self {
        match arg {
            EnvironmentPolicyArg::Inherited => EnvironmentPolicyKind::Inherited,
            EnvironmentPolicyArg::Isolated => EnvironmentPolicyKind::Isolated,
            EnvironmentPolicyArg::Granted => EnvironmentPolicyKind::Granted,
        }
    }
}

/// The parsed, value-free environment configuration for a `harn run` invocation.
///
/// Holds the policy kind and the declared grant specs (never resolved
/// values), so it is safe to clone and pass through the run-options plumbing.
/// The env snapshot happens later, once, at [`launch`](Self::launch).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EnvironmentPolicyConfig {
    kind: EnvironmentPolicyKind,
    grants: Vec<GrantSpec>,
}

impl Default for EnvironmentPolicyConfig {
    fn default() -> Self {
        Self {
            kind: EnvironmentPolicyKind::Inherited,
            grants: Vec::new(),
        }
    }
}

impl EnvironmentPolicyConfig {
    /// Resolve the declared configuration from the `--environment-policy` / `--grant`
    /// flags. Omission selects `inherited`; grants select `granted`.
    pub(crate) fn from_flags(
        policy: Option<EnvironmentPolicyArg>,
        grants: &[String],
    ) -> Result<Self, String> {
        let kind = match (policy, grants.is_empty()) {
            (None, true) => EnvironmentPolicyKind::Inherited,
            (Some(arg), _) => EnvironmentPolicyKind::from(arg),
            (None, false) => EnvironmentPolicyKind::Granted,
        };
        if !matches!(kind, EnvironmentPolicyKind::Granted) && !grants.is_empty() {
            return Err(format!(
                "[environment_policy.grants_forbidden] --environment-policy {} forbids grants, but {} --grant flag(s) were given; use '--environment-policy granted' or remove the grants",
                kind.as_str(), grants.len()
            ));
        }
        let grants = grants
            .iter()
            .map(|spec| parse_grant_spec(spec))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { kind, grants })
    }

    /// Resolve the configuration into a runtime [`SessionEnvironment`], snapshotting each
    /// `env:` grant against the launcher environment. Fails loudly if a named
    /// launcher variable is absent or the grant set violates the policy.
    pub(crate) fn launch(
        &self,
    ) -> Result<SessionEnvironment, harn_vm::security::EnvironmentPolicyError> {
        SessionEnvironment::launch(self.kind, self.grants.clone(), &|var| {
            std::env::var(var).ok()
        })
    }
}

/// Parse one longhand `--grant NAME=SOURCE[,expose=ENV_VAR]` string.
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

/// Launch the declared policy, disclose it on `stderr`, and return the
/// installed ambient scope plus the non-secret receipts. The scope clears the
/// environment on drop, so the caller holds it for the run's duration; a launch
/// failure (a missing launcher variable, a grant on an isolated policy) is
/// surfaced as an error string for the caller to fail the run loudly.
pub(crate) fn launch_scope(
    config: &EnvironmentPolicyConfig,
    stderr: &mut String,
) -> Result<
    (
        SessionEnvironmentScope,
        EnvironmentPolicyKind,
        Vec<GrantReceipt>,
    ),
    harn_vm::security::EnvironmentPolicyError,
> {
    let environment = config.launch()?;
    let kind = environment.kind();
    let receipts = environment.receipts();
    stderr.push_str(&environment_disclosure(&environment));
    Ok((
        SessionEnvironmentScope::install(environment),
        kind,
        receipts,
    ))
}

/// One line naming a launched environment policy for the run's
/// stderr — the credential-facing counterpart to the sandbox root disclosure.
/// It names grants (their target env var and source kind) but never a value,
/// so a granted run is never silent about the values it carries.
fn environment_disclosure(environment: &SessionEnvironment) -> String {
    let receipts = environment.receipts();
    if receipts.is_empty() {
        return format!(
            "environment policy: {} — applies to this session and its subprocesses\n",
            environment.kind().as_str()
        );
    }
    let grants = receipts
        .iter()
        .map(|receipt| {
            if let Some(target) = receipt.exposed_as_env.as_deref() {
                format!(
                    "{} ({}, exposed as {target})",
                    receipt.name, receipt.source_kind
                )
            } else {
                format!(
                    "{} ({}, carried, not exposed)",
                    receipt.name, receipt.source_kind
                )
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "environment policy: {} — session-wide grants: {grants}\n",
        environment.kind().as_str()
    )
}

/// Installs a resolved [`SessionEnvironment`] as the current task's ambient
/// environment policy and clears it on drop, so the closed-env resolver governs
/// every subprocess spawned during the run and nothing leaks past it.
pub(crate) struct SessionEnvironmentScope;

impl SessionEnvironmentScope {
    fn install(environment: SessionEnvironment) -> Self {
        harn_vm::stdlib::process::set_session_environment(Some(environment));
        Self
    }
}

impl Drop for SessionEnvironmentScope {
    fn drop(&mut self) {
        harn_vm::stdlib::process::set_session_environment(None);
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
    fn longhand_grant_named_provider_is_not_the_shorthand() {
        let g = parse_grant_spec("provider=env:X").expect("longhand");
        assert_eq!(g.name, "provider");
        assert_eq!(
            g.source,
            GrantSourceSpec::Env {
                var: "X".to_string()
            }
        );
    }

    #[test]
    fn from_flags_no_flags_is_inherited() {
        let config = EnvironmentPolicyConfig::from_flags(None, &[]).unwrap();
        assert_eq!(config.kind, EnvironmentPolicyKind::Inherited);
        assert!(config.grants.is_empty());
    }

    #[test]
    fn from_flags_grants_imply_granted() {
        let config = EnvironmentPolicyConfig::from_flags(None, &["t=env:X".to_string()]).unwrap();
        assert_eq!(config.kind, EnvironmentPolicyKind::Granted);
        assert_eq!(config.grants.len(), 1);
    }

    #[test]
    fn from_flags_explicit_isolated_has_no_grants() {
        let config =
            EnvironmentPolicyConfig::from_flags(Some(EnvironmentPolicyArg::Isolated), &[]).unwrap();
        assert_eq!(config.kind, EnvironmentPolicyKind::Isolated);
        assert!(config.grants.is_empty());
    }

    #[test]
    fn from_flags_isolated_with_grants_is_rejected() {
        let err = EnvironmentPolicyConfig::from_flags(
            Some(EnvironmentPolicyArg::Isolated),
            &["t=env:X".to_string()],
        )
        .unwrap_err();
        assert!(err.contains("isolated forbids"), "{err}");
    }

    #[test]
    fn launch_snapshots_env_and_records_receipt() {
        // SAFETY: single-threaded test; the var is removed before returning.
        let key = "HARN_TEST_GRANT_LAUNCH_VAR";
        std::env::set_var(key, "snapshot-value");
        let config =
            EnvironmentPolicyConfig::from_flags(None, &[format!("t=env:{key},expose=CHILD_VAR")])
                .unwrap();
        let environment = config.launch().expect("launch resolves the env snapshot");
        std::env::remove_var(key);

        let receipts = environment.receipts();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].name, "t");
        assert_eq!(receipts[0].source_kind, GrantSource::Env.as_str());
        assert_eq!(receipts[0].exposed_as_env.as_deref(), Some("CHILD_VAR"));

        let disclosure = environment_disclosure(&environment);
        assert!(disclosure.contains("granted"), "{disclosure}");
        assert!(disclosure.contains("exposed as CHILD_VAR"), "{disclosure}");
        assert!(
            !disclosure.contains("snapshot-value"),
            "disclosure leaked a value"
        );
    }

    #[test]
    fn launch_missing_env_var_fails_loudly() {
        let config = EnvironmentPolicyConfig::from_flags(
            None,
            &["t=env:HARN_TEST_ABSENT_GRANT_VAR".to_string()],
        )
        .unwrap();
        assert!(config.launch().is_err());
    }

    #[test]
    fn isolated_disclosure_names_the_posture() {
        let disclosure = environment_disclosure(&SessionEnvironment::isolated());
        assert!(disclosure.contains("isolated"), "{disclosure}");
    }
}
