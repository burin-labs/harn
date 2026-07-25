//! Session-scoped capability grants.
//!
//! A launched session runs under one of two named profiles that model
//! opposite credential requirements:
//!
//!   * [`SessionProfileKind::Hermetic`] — grants are structurally forbidden.
//!     `grants: []` is the *runtime definition* of hermetic, enforced at
//!     launch: attaching any grant to a hermetic profile is a launch error,
//!     not a warning. This is the identity a replayable, no-credentials run
//!     (evals) requires, and it stays hermetic by construction rather than by
//!     an accident of which flags were passed.
//!   * [`SessionProfileKind::Lane`] — carries a declared set of grants. A
//!     grant is a scoped, revocable, receipted *pointer* to an upstream
//!     credential source (the `secret_store` facade, or a launcher env var).
//!     It is the sole path by which a credential crosses the sandbox
//!     boundary; nothing else from the launcher's environment or home
//!     directory reaches the child.
//!
//! # Ownership boundary
//!
//! The launcher (e.g. the Burin CLI) parses its own `--grant name=spec`
//! strings **at its boundary** and hands harn a typed, value-free
//! [`GrantSpec`] set — carried in the session/ACP config. harn does not parse
//! flag strings. harn owns the typed contract: [`GrantSpec`] in,
//! [`SessionGrant`]/[`SessionProfile`] resolution, [`GrantReceipt`] schema,
//! and profile enforcement.
//!
//! A [`GrantSpec`] is value-free: an `env:` source names the launcher variable
//! (not its value); a `secret_store` source is an account/key *pointer*. So a
//! spec is safe to serialize into a session config. The resolved
//! [`SessionGrant`] may hold a snapshotted secret value, so it is deliberately
//! **not** `Serialize` and never lands in a record — only [`GrantReceipt`]
//! ({name, source_kind, exposed_as_env}) is persisted, and it omits even the
//! secret pointer.
//!
//! Two non-leakage properties are enforced by the type system:
//!
//!   * The value-bearing types ([`SessionGrant`], [`SessionProfile`], and the
//!     private `ResolvedRef`) are not `Serialize`. The compiler refuses to
//!     serialize a type that can hold a secret, so a record cannot leak one.
//!   * An `env:` source is snapshotted at launch, so the child never reads the
//!     launcher's live environment; a later env mutation does not change what
//!     the session sees.
//!
//! Materializing a `secret_store` pointer into a value happens through the
//! embedder's `resolve_secret` closure (backed by the `secret_store` facade),
//! so this crate takes no dependency on the hostlib that registers it.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Where a granted credential originates. Recorded in receipts as a stable
/// string; never carries the value itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantSource {
    /// Snapshotted from a launcher environment variable at launch time.
    Env,
    /// A pointer into the `secret_store` facade, resolved on use.
    SecretStore,
}

impl GrantSource {
    /// Stable wire string used in receipts and diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            GrantSource::Env => "env",
            GrantSource::SecretStore => "secret_store",
        }
    }
}

/// The value-free source of a grant, as declared in the session config. An
/// `Env` source names a launcher variable (not its value); a `SecretStore`
/// source is an account/key pointer. Both are safe to serialize.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantSourceSpec {
    /// Snapshot the named launcher environment variable at launch.
    Env { var: String },
    /// A `secret_store` account/key pointer, resolved lazily on exposure.
    SecretStore { account: String, key: String },
}

impl GrantSourceSpec {
    fn kind(&self) -> GrantSource {
        match self {
            GrantSourceSpec::Env { .. } => GrantSource::Env,
            GrantSourceSpec::SecretStore { .. } => GrantSource::SecretStore,
        }
    }
}

/// A single grant as declared by the launcher in the session config. Typed and
/// value-free: harn receives this already-structured (the launcher did any
/// string parsing at its own boundary) and validates/resolves it once, here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantSpec {
    /// Logical grant name (the credential a tool asks for by name).
    pub name: String,
    /// Where the credential comes from.
    pub source: GrantSourceSpec,
    /// The target process-env variable to expose the value as, if any. `None`
    /// (the default) means the grant is not exposed to `process.exec`. This is
    /// the sole place the exposure default lives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expose_as_env: Option<String>,
}

impl GrantSpec {
    /// Validate the declared shape (non-empty name/fields) and resolve against
    /// the launcher environment, snapshotting an `env:` source so the child
    /// never reads the live environment. A `secret_store` source is carried
    /// through as a pointer, resolved lazily on exposure.
    fn resolve(
        self,
        env_lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Result<SessionGrant, GrantError> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(GrantError::EmptyName);
        }
        if let Some(var) = self.expose_as_env.as_deref() {
            if var.trim().is_empty() {
                return Err(GrantError::EmptyExposeVar {
                    name: name.to_string(),
                });
            }
        }
        let source_kind = self.source.kind();
        let resolved_ref = match self.source {
            GrantSourceSpec::Env { var } => {
                let var = var.trim();
                if var.is_empty() {
                    return Err(GrantError::EmptyEnvVar {
                        name: name.to_string(),
                    });
                }
                let value = env_lookup(var).ok_or_else(|| GrantError::MissingEnv {
                    name: name.to_string(),
                    var: var.to_string(),
                })?;
                ResolvedRef::EnvSnapshot(value)
            }
            GrantSourceSpec::SecretStore { account, key } => {
                let (account, key) = (account.trim(), key.trim());
                if account.is_empty() || key.is_empty() {
                    return Err(GrantError::EmptySecretRef {
                        name: name.to_string(),
                    });
                }
                ResolvedRef::SecretStore {
                    account: account.to_string(),
                    key: key.to_string(),
                }
            }
        };
        Ok(SessionGrant {
            name: name.to_string(),
            source_kind,
            expose_as_env: self.expose_as_env.map(|var| var.trim().to_string()),
            resolved_ref,
        })
    }
}

/// The resolved backing of a grant. Private so no consumer can branch on it;
/// intentionally not `Serialize` so a snapshotted value can never leak into a
/// record.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ResolvedRef {
    /// A value captured from the launcher env at launch. Held here and never
    /// re-read from the live environment.
    EnvSnapshot(String),
    /// A `secret_store` pointer, resolved to a value only on exposure. Kept as
    /// a pointer (not a snapshot) so the upstream source stays the single
    /// source of truth and the grant remains revocable.
    SecretStore { account: String, key: String },
}

/// A grant validated and resolved once at the launch boundary. Consumers read
/// this record; they do not re-branch on `source_kind` or re-check exposure.
///
/// Deliberately not `Serialize`: it can hold a snapshotted secret value, so it
/// must never land in a record. Use [`GrantReceipt`] for anything persisted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionGrant {
    name: String,
    source_kind: GrantSource,
    expose_as_env: Option<String>,
    resolved_ref: ResolvedRef,
}

impl SessionGrant {
    /// The grant's logical name (the credential a tool asks for by name).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Where the credential originates.
    pub fn source_kind(&self) -> GrantSource {
        self.source_kind
    }

    /// The process-env variable this grant is exposed as, if any.
    pub fn exposed_env_var(&self) -> Option<&str> {
        self.expose_as_env.as_deref()
    }

    /// The `(VAR, value)` pair this grant publishes, or `None` when it declared
    /// no `expose_as_env` target.
    ///
    /// This is the single place the source kind is branched on — an
    /// `EnvSnapshot` is already a value; a `secret_store` pointer is resolved
    /// here, on use, through the embedder's `resolve_secret` closure. Every
    /// exposure path goes through this one method, so a consumer never sees the
    /// source kind and the two paths cannot disagree about what a grant means.
    fn exposure(
        &self,
        resolve_secret: &dyn Fn(&str, &str) -> Option<String>,
    ) -> Option<Result<(String, String), GrantError>> {
        let var = self.expose_as_env.as_ref()?;
        let value = match &self.resolved_ref {
            ResolvedRef::EnvSnapshot(value) => value.clone(),
            ResolvedRef::SecretStore { account, key } => match resolve_secret(account, key) {
                Some(value) => value,
                None => {
                    return Some(Err(GrantError::MissingSecret {
                        name: self.name.clone(),
                    }))
                }
            },
        };
        Some(Ok((var.clone(), value)))
    }

    /// The non-secret receipt for this grant.
    pub fn receipt(&self) -> GrantReceipt {
        GrantReceipt {
            name: self.name.clone(),
            source_kind: self.source_kind.as_str().to_string(),
            exposed_as_env: self.expose_as_env.is_some(),
        }
    }
}

/// Which named profile a session launches under. The profile is a typed launch
/// input, not an emergent property of which flags were passed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionProfileKind {
    /// No credentials may cross the boundary. Grants are forbidden. The
    /// default: absent an explicit lane profile, a session is hermetic.
    #[default]
    Hermetic,
    /// Autonomous lane: credentials cross only through declared grants.
    Lane,
}

impl SessionProfileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionProfileKind::Hermetic => "hermetic",
            SessionProfileKind::Lane => "lane",
        }
    }
}

/// A launched session's resolved credential profile.
///
/// Not `Serialize` (its grants may hold snapshotted values); serialize
/// [`SessionProfile::receipts`] instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionProfile {
    kind: SessionProfileKind,
    grants: Vec<SessionGrant>,
}

impl SessionProfile {
    /// Launch a profile from the config's declared grant specs, resolving each
    /// against the launcher environment.
    ///
    /// A hermetic profile **rejects any grant at launch** — hermeticity is an
    /// enforced structural property, not an assertion made after the fact. A
    /// lane profile resolves and carries the declared grant set.
    pub fn launch(
        kind: SessionProfileKind,
        specs: Vec<GrantSpec>,
        env_lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, GrantError> {
        if matches!(kind, SessionProfileKind::Hermetic) && !specs.is_empty() {
            return Err(GrantError::HermeticForbidsGrants {
                attempted: specs.len(),
            });
        }
        let grants = specs
            .into_iter()
            .map(|spec| spec.resolve(env_lookup))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SessionProfile { kind, grants })
    }

    /// A hermetic profile with no grants — the runtime definition of hermetic.
    pub fn hermetic() -> Self {
        SessionProfile {
            kind: SessionProfileKind::Hermetic,
            grants: Vec::new(),
        }
    }

    pub fn kind(&self) -> SessionProfileKind {
        self.kind
    }

    pub fn is_hermetic(&self) -> bool {
        matches!(self.kind, SessionProfileKind::Hermetic)
    }

    /// The resolved grants. Empty for a hermetic profile, always.
    pub fn grants(&self) -> &[SessionGrant] {
        &self.grants
    }

    /// The non-secret receipts recorded on the session run-record. Empty for a
    /// hermetic profile, which makes `grants: []` a *checked* property.
    pub fn receipts(&self) -> Vec<GrantReceipt> {
        self.grants.iter().map(SessionGrant::receipt).collect()
    }

    /// Materialize the process environment overlay for `process.exec`: the
    /// `(VAR, value)` pairs for every grant that opted into `expose_as_env`.
    /// Empty for a hermetic profile.
    ///
    /// Callers receive uniform pairs and never see the source kind;
    /// [`SessionGrant::exposure`] owns that branch.
    ///
    /// # Least privilege
    ///
    /// v1 exposes to the session's `process.exec` env. The intended tightening
    /// is a per-tool binding — the granted var visible only inside the exec
    /// whose tool declared it, not session-wide ambient. That is a follow-up;
    /// the ambient scope here is documented, not locked into the contract.
    pub fn env_exposure(
        &self,
        resolve_secret: &dyn Fn(&str, &str) -> Option<String>,
    ) -> Result<Vec<(String, String)>, GrantError> {
        self.grants
            .iter()
            .filter_map(|grant| grant.exposure(resolve_secret))
            .collect()
    }

    /// The value this profile exposes under a single environment variable, or
    /// `None` if no grant targets it.
    ///
    /// The narrow counterpart of [`env_exposure`](Self::env_exposure), for a
    /// consumer resolving one variable — harn's own provider-credential lookup.
    /// It resolves *only* the grant that targets `var`, which matters for a
    /// `secret_store` grant: probing an unrelated variable must not reach the
    /// secret store, and one unresolvable grant must not mask an unrelated
    /// credential. The last matching grant wins, mirroring the overlay order of
    /// `env_exposure`'s pairs.
    pub fn env_exposure_for(
        &self,
        var: &str,
        resolve_secret: &dyn Fn(&str, &str) -> Option<String>,
    ) -> Result<Option<String>, GrantError> {
        let Some(grant) = self
            .grants
            .iter()
            .rfind(|grant| grant.expose_as_env.as_deref() == Some(var))
        else {
            return Ok(None);
        };
        grant
            .exposure(resolve_secret)
            .transpose()
            .map(|pair| pair.map(|(_, value)| value))
    }
}

/// A non-secret record of a grant, safe to persist on a session run-record.
///
/// Carries the grant name, the source kind, and whether it was exposed to the
/// process environment — never the value, and never a reversible reference to
/// it. `secret_store` pointers (account/key) are intentionally omitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantReceipt {
    pub name: String,
    pub source_kind: String,
    pub exposed_as_env: bool,
}

/// Errors raised while validating, resolving, or enforcing session grants. All
/// are launch-boundary failures; none carries a secret value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrantError {
    /// A grant spec had an empty name.
    EmptyName,
    /// An `env` source named an empty variable.
    EmptyEnvVar { name: String },
    /// A `secret_store` source named an empty account or key.
    EmptySecretRef { name: String },
    /// An `expose_as_env` target was an empty variable name.
    EmptyExposeVar { name: String },
    /// An `env` source referenced a variable absent from the launcher env.
    MissingEnv { name: String, var: String },
    /// A grant was declared on a hermetic profile.
    HermeticForbidsGrants { attempted: usize },
    /// A `secret_store` grant could not be resolved on exposure.
    MissingSecret { name: String },
}

impl fmt::Display for GrantError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GrantError::EmptyName => write!(f, "grant spec has an empty name"),
            GrantError::EmptyEnvVar { name } => {
                write!(f, "grant '{name}' env source names an empty variable")
            }
            GrantError::EmptySecretRef { name } => {
                write!(f, "grant '{name}' secret source names an empty account/key")
            }
            GrantError::EmptyExposeVar { name } => {
                write!(f, "grant '{name}' expose target is an empty variable")
            }
            GrantError::MissingEnv { name, var } => write!(
                f,
                "grant '{name}' env source variable '{var}' is not set in the launcher environment"
            ),
            GrantError::HermeticForbidsGrants { attempted } => write!(
                f,
                "hermetic profile forbids grants, but {attempted} were declared"
            ),
            GrantError::MissingSecret { name } => {
                write!(
                    f,
                    "grant '{name}' could not be resolved from the secret store"
                )
            }
        }
    }
}

impl std::error::Error for GrantError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn env_from(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |var: &str| {
            pairs
                .iter()
                .find(|(name, _)| *name == var)
                .map(|(_, value)| value.to_string())
        }
    }

    fn env_grant(name: &str, var: &str, expose: Option<&str>) -> GrantSpec {
        GrantSpec {
            name: name.to_string(),
            source: GrantSourceSpec::Env {
                var: var.to_string(),
            },
            expose_as_env: expose.map(str::to_string),
        }
    }

    fn secret_grant(name: &str, account: &str, key: &str, expose: Option<&str>) -> GrantSpec {
        GrantSpec {
            name: name.to_string(),
            source: GrantSourceSpec::SecretStore {
                account: account.to_string(),
                key: key.to_string(),
            },
            expose_as_env: expose.map(str::to_string),
        }
    }

    #[test]
    fn hermetic_rejects_any_grant_at_launch() {
        let specs = vec![secret_grant("gh_token", "gh", "token", None)];
        let err = SessionProfile::launch(SessionProfileKind::Hermetic, specs, &no_env)
            .expect_err("hermetic must reject grants");
        assert_eq!(err, GrantError::HermeticForbidsGrants { attempted: 1 });

        // Belt and suspenders: an empty hermetic launch and the convenience
        // constructor are both structurally empty, and hermetic is the default.
        let profile =
            SessionProfile::launch(SessionProfileKind::Hermetic, vec![], &no_env).unwrap();
        assert!(profile.is_hermetic());
        assert!(profile.grants().is_empty());
        assert!(profile.receipts().is_empty());
        assert!(SessionProfile::hermetic().grants().is_empty());
        assert_eq!(SessionProfileKind::default(), SessionProfileKind::Hermetic);
    }

    #[test]
    fn lane_grant_resolves_once_into_typed_record() {
        let env = env_from(&[("FIREWORKS_API_KEY", "fw-secret-value")]);
        let specs = vec![
            env_grant("fireworks", "FIREWORKS_API_KEY", Some("FIREWORKS_API_KEY")),
            secret_grant("gh_token", "gh", "token", Some("GH_TOKEN")),
        ];
        let profile = SessionProfile::launch(SessionProfileKind::Lane, specs, &env).unwrap();

        let grants = profile.grants();
        assert_eq!(grants.len(), 2);
        // Downstream reads the typed record without re-branching on the spec.
        assert_eq!(grants[0].name(), "fireworks");
        assert_eq!(grants[0].source_kind(), GrantSource::Env);
        assert_eq!(grants[0].exposed_env_var(), Some("FIREWORKS_API_KEY"));
        assert_eq!(grants[1].name(), "gh_token");
        assert_eq!(grants[1].source_kind(), GrantSource::SecretStore);
        assert_eq!(grants[1].exposed_env_var(), Some("GH_TOKEN"));

        // Exposure materializes uniform (VAR, value) pairs. The secret store
        // pointer is resolved here, once, through the embedder closure.
        let resolve_secret = |account: &str, key: &str| -> Option<String> {
            (account == "gh" && key == "token").then(|| "ghp-secret-token".to_string())
        };
        let mut pairs = profile.env_exposure(&resolve_secret).unwrap();
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                (
                    "FIREWORKS_API_KEY".to_string(),
                    "fw-secret-value".to_string()
                ),
                ("GH_TOKEN".to_string(), "ghp-secret-token".to_string()),
            ]
        );
    }

    #[test]
    fn secret_pointer_is_not_resolved_at_launch() {
        // Resolution of a secret_store grant must not read the value at launch.
        // A panicking secret resolver proves exposure is lazy, and an unexposed
        // grant never calls the resolver at all.
        let specs = vec![secret_grant("gh_token", "gh", "token", None)];
        let profile = SessionProfile::launch(SessionProfileKind::Lane, specs, &no_env).unwrap();
        let never = |_: &str, _: &str| -> Option<String> {
            panic!("secret resolver must not run for an unexposed grant")
        };
        assert!(profile.env_exposure(&never).unwrap().is_empty());
    }

    #[test]
    fn env_grant_snapshots_value_at_launch() {
        // The launcher env yields "live-at-launch"; the snapshot must hold that
        // value afterward — the child never reads the live environment.
        let at_launch = env_from(&[("TOKEN", "live-at-launch")]);
        let specs = vec![env_grant("t", "TOKEN", Some("TOKEN"))];
        let profile = SessionProfile::launch(SessionProfileKind::Lane, specs, &at_launch).unwrap();

        let never_secret = |_: &str, _: &str| -> Option<String> { None };
        let pairs = profile.env_exposure(&never_secret).unwrap();
        assert_eq!(
            pairs,
            vec![("TOKEN".to_string(), "live-at-launch".to_string())]
        );
        // The resolved grant is unaffected by any later env — it holds the
        // launch-time snapshot.
        assert_eq!(
            profile.env_exposure(&never_secret).unwrap(),
            vec![("TOKEN".to_string(), "live-at-launch".to_string())]
        );
    }

    #[test]
    fn receipts_record_shape_and_never_the_value() {
        let env = env_from(&[("FIREWORKS_API_KEY", "fw-secret-value")]);
        let specs = vec![
            env_grant("fireworks", "FIREWORKS_API_KEY", Some("FIREWORKS_API_KEY")),
            secret_grant("gh_token", "gh", "token", None),
        ];
        let profile = SessionProfile::launch(SessionProfileKind::Lane, specs, &env).unwrap();

        let receipts = profile.receipts();
        assert_eq!(
            receipts,
            vec![
                GrantReceipt {
                    name: "fireworks".to_string(),
                    source_kind: "env".to_string(),
                    exposed_as_env: true,
                },
                GrantReceipt {
                    name: "gh_token".to_string(),
                    source_kind: "secret_store".to_string(),
                    exposed_as_env: false,
                },
            ]
        );

        // The serialized receipts must never contain the snapshotted value or
        // the secret pointer. (SessionGrant/SessionProfile are not Serialize,
        // so this is also enforced at compile time; assert it at runtime too.)
        let json = serde_json::to_string(&receipts).unwrap();
        assert!(
            !json.contains("fw-secret-value"),
            "receipt leaked env value"
        );
        assert!(!json.contains("gh/token"), "receipt leaked secret pointer");
        assert!(json.contains("\"source_kind\":\"env\""));
        assert!(json.contains("\"source_kind\":\"secret_store\""));
    }

    #[test]
    fn grant_spec_is_value_free_over_the_wire() {
        // A GrantSpec (the config contract) carries the env var NAME and the
        // secret pointer, never a value — safe to serialize into a config.
        let spec = env_grant("fireworks", "FIREWORKS_API_KEY", Some("FIREWORKS_API_KEY"));
        let json = serde_json::to_string(&spec).unwrap();
        let round: GrantSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(round, spec);
        assert!(json.contains("\"env\""));
        assert!(json.contains("FIREWORKS_API_KEY"));

        // Profile kind is a typed, defaulted config field (hermetic by default).
        assert_eq!(
            serde_json::from_str::<SessionProfileKind>("\"lane\"").unwrap(),
            SessionProfileKind::Lane
        );
    }

    #[test]
    fn missing_env_source_fails_at_launch() {
        let specs = vec![env_grant("t", "ABSENT_VAR", None)];
        let err = SessionProfile::launch(SessionProfileKind::Lane, specs, &no_env)
            .expect_err("absent env var must fail resolution");
        assert_eq!(
            err,
            GrantError::MissingEnv {
                name: "t".to_string(),
                var: "ABSENT_VAR".to_string(),
            }
        );
    }

    #[test]
    fn resolve_rejects_empty_fields() {
        let env = env_from(&[("X", "v")]);
        assert_eq!(
            SessionProfile::launch(
                SessionProfileKind::Lane,
                vec![env_grant("", "X", None)],
                &env
            ),
            Err(GrantError::EmptyName)
        );
        assert_eq!(
            SessionProfile::launch(
                SessionProfileKind::Lane,
                vec![env_grant("t", "", None)],
                &env
            ),
            Err(GrantError::EmptyEnvVar {
                name: "t".to_string()
            })
        );
        assert_eq!(
            SessionProfile::launch(
                SessionProfileKind::Lane,
                vec![secret_grant("t", "acct", "", None)],
                &env
            ),
            Err(GrantError::EmptySecretRef {
                name: "t".to_string()
            })
        );
        assert_eq!(
            SessionProfile::launch(
                SessionProfileKind::Lane,
                vec![env_grant("t", "X", Some(" "))],
                &env
            ),
            Err(GrantError::EmptyExposeVar {
                name: "t".to_string()
            })
        );
    }
}
