//! Session-scoped environment policy and grants.
//!
//! Every launched session has exactly one policy:
//!
//! - [`EnvironmentPolicyKind::Inherited`] preserves a launch-time snapshot.
//! - [`EnvironmentPolicyKind::Isolated`] admits runtime essentials only.
//! - [`EnvironmentPolicyKind::Granted`] adds declared grants to those
//!   essentials.
//!
//! A child receives the parent's resolved object and may call
//! [`SessionEnvironment::narrow`]; it never rereads the ambient host
//! environment or gains authority its parent did not have.
//!
//! # Ownership boundary
//!
//! The launcher (e.g. the Burin CLI) parses its own `--grant name=spec`
//! strings **at its boundary** and hands harn a typed, value-free
//! [`GrantSpec`] set — carried in the session/ACP config. harn does not parse
//! flag strings. harn owns the typed contract: [`GrantSpec`] in,
//! [`SessionGrant`]/[`SessionEnvironment`] resolution, [`GrantReceipt`] schema,
//! and policy enforcement.
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
//!   * The value-bearing types ([`SessionGrant`], [`SessionEnvironment`], and the
//!     private `ResolvedRef`) are not `Serialize`. The compiler refuses to
//!     serialize a type that can hold a secret, so a record cannot leak one.
//!   * An `env:` source is snapshotted at launch, so the child never reads the
//!     launcher's live environment; a later env mutation does not change what
//!     the session sees.
//!
//! Materializing a `secret_store` pointer into a value happens through the
//! embedder's `resolve_secret` closure (backed by the `secret_store` facade),
//! so this crate takes no dependency on the hostlib that registers it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

/// Where a granted value originates. Recorded in receipts as a stable
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
    /// Logical grant name used in receipts and diagnostics.
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
    ) -> Result<SessionGrant, EnvironmentPolicyError> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(EnvironmentPolicyError::EmptyName);
        }
        if let Some(var) = self.expose_as_env.as_deref() {
            if var.trim().is_empty() {
                return Err(EnvironmentPolicyError::EmptyExposeVar {
                    name: name.to_string(),
                });
            }
        }
        let source_kind = self.source.kind();
        let source_spec = self.source.clone();
        let resolved_ref = match self.source {
            GrantSourceSpec::Env { var } => {
                let var = var.trim();
                if var.is_empty() {
                    return Err(EnvironmentPolicyError::EmptyEnvVar {
                        name: name.to_string(),
                    });
                }
                let value = env_lookup(var).ok_or_else(|| EnvironmentPolicyError::MissingEnv {
                    name: name.to_string(),
                    var: var.to_string(),
                })?;
                ResolvedRef::EnvSnapshot(value)
            }
            GrantSourceSpec::SecretStore { account, key } => {
                let (account, key) = (account.trim(), key.trim());
                if account.is_empty() || key.is_empty() {
                    return Err(EnvironmentPolicyError::EmptySecretRef {
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
            source_spec,
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
    source_spec: GrantSourceSpec,
    expose_as_env: Option<String>,
    resolved_ref: ResolvedRef,
}

impl SessionGrant {
    fn matches_spec(&self, spec: &GrantSpec) -> bool {
        self.name == spec.name.trim()
            && self.source_spec == spec.source
            && self.expose_as_env.as_deref() == spec.expose_as_env.as_deref().map(str::trim)
    }
    /// The grant's logical name used in receipts and diagnostics.
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
    ) -> Option<Result<(String, String), EnvironmentPolicyError>> {
        let var = self.expose_as_env.as_ref()?;
        let value = match &self.resolved_ref {
            ResolvedRef::EnvSnapshot(value) => value.clone(),
            ResolvedRef::SecretStore { account, key } => match resolve_secret(account, key) {
                Some(value) => value,
                None => {
                    return Some(Err(EnvironmentPolicyError::MissingSecret {
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
            exposed_as_env: self.expose_as_env.clone(),
        }
    }
}

/// Which environment policy a session launches under. This is a typed launch
/// input, not an emergent property of which flags were passed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentPolicyKind {
    /// Preserve a launch-time snapshot of the launcher's environment.
    #[default]
    Inherited,
    /// Admit only non-secret runtime essentials. Grants are forbidden.
    Isolated,
    /// Admit runtime essentials plus the declared grant set.
    Granted,
}

impl EnvironmentPolicyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EnvironmentPolicyKind::Inherited => "inherited",
            EnvironmentPolicyKind::Isolated => "isolated",
            EnvironmentPolicyKind::Granted => "granted",
        }
    }
}

/// A launched session's resolved environment.
///
/// Not `Serialize` (its grants may hold snapshotted values); serialize
/// [`SessionEnvironment::receipts`] instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEnvironment {
    kind: EnvironmentPolicyKind,
    launcher_snapshot: BTreeMap<String, String>,
    grants: Vec<SessionGrant>,
}

impl SessionEnvironment {
    /// Capture the default policy at a session launch boundary.
    pub fn inherited() -> Self {
        Self::launch(EnvironmentPolicyKind::Inherited, Vec::new(), &|name| {
            std::env::var(name).ok()
        })
        .expect("the inherited policy has no fallible grant configuration")
    }

    /// Launch an environment from the config's declared grant specs, resolving each
    /// against the launcher environment.
    ///
    /// An isolated policy **rejects any grant at launch** — isolation is an
    /// enforced structural property, not an assertion made after the fact. A
    /// granted policy resolves and carries the declared grant set.
    pub fn launch(
        kind: EnvironmentPolicyKind,
        specs: Vec<GrantSpec>,
        env_lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, EnvironmentPolicyError> {
        let mut launcher_snapshot = capture_process_environment();
        for name in super::environment_policy::ENV_ALLOWLIST {
            if let Some(value) = env_lookup(name) {
                launcher_snapshot.insert((*name).to_string(), value);
            }
        }
        Self::launch_from_snapshot(kind, specs, launcher_snapshot, env_lookup)
    }

    /// Resolve a session from one authoritative launcher snapshot.
    ///
    /// `env_lookup` exists for embedders that supply a typed environment
    /// source. Production callers normally pass a lookup over the same
    /// snapshot, while tests can provide a small deterministic map.
    pub fn launch_from_snapshot(
        kind: EnvironmentPolicyKind,
        specs: Vec<GrantSpec>,
        launcher_snapshot: BTreeMap<String, String>,
        env_lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, EnvironmentPolicyError> {
        if !matches!(kind, EnvironmentPolicyKind::Granted) && !specs.is_empty() {
            return Err(EnvironmentPolicyError::PolicyForbidsGrants {
                policy: kind,
                attempted: specs.len(),
            });
        }
        validate_unique_specs(&specs)?;
        let grants = specs
            .into_iter()
            .map(|spec| spec.resolve(env_lookup))
            .collect::<Result<Vec<_>, _>>()?;
        let launcher_snapshot = if matches!(kind, EnvironmentPolicyKind::Inherited) {
            launcher_snapshot
        } else {
            launcher_snapshot
                .into_iter()
                .filter(|(name, _)| {
                    super::environment_policy::ENV_ALLOWLIST.contains(&name.as_str())
                })
                .collect()
        };
        Ok(SessionEnvironment {
            kind,
            launcher_snapshot,
            grants,
        })
    }

    /// An isolated environment with no grants.
    pub fn isolated() -> Self {
        Self::launch(EnvironmentPolicyKind::Isolated, Vec::new(), &|name| {
            std::env::var(name).ok()
        })
        .expect("the isolated policy has no fallible grant configuration")
    }

    pub fn kind(&self) -> EnvironmentPolicyKind {
        self.kind
    }

    pub fn is_isolated(&self) -> bool {
        matches!(self.kind, EnvironmentPolicyKind::Isolated)
    }

    /// Whether provider SDKs may use platform-managed ambient discovery such
    /// as AWS shared config, metadata services, or application-default
    /// credentials.
    pub fn allows_implicit_discovery(&self) -> bool {
        matches!(self.kind, EnvironmentPolicyKind::Inherited)
    }

    /// Derive a child session without allowing it to gain environment
    /// authority that its parent did not have.
    pub fn narrow(
        &self,
        requested: EnvironmentPolicyKind,
        specs: Vec<GrantSpec>,
    ) -> Result<Self, EnvironmentPolicyError> {
        match (self.kind, requested) {
            (EnvironmentPolicyKind::Inherited, EnvironmentPolicyKind::Inherited)
                if specs.is_empty() =>
            {
                Ok(self.clone())
            }
            (EnvironmentPolicyKind::Inherited, EnvironmentPolicyKind::Isolated)
                if specs.is_empty() =>
            {
                Ok(Self {
                    kind: requested,
                    launcher_snapshot: self.launcher_snapshot.clone(),
                    grants: Vec::new(),
                })
            }
            (EnvironmentPolicyKind::Inherited, EnvironmentPolicyKind::Granted) => {
                if let Some(spec) = specs
                    .iter()
                    .find(|spec| matches!(spec.source, GrantSourceSpec::SecretStore { .. }))
                {
                    return Err(EnvironmentPolicyError::ChildPolicyExceedsParent {
                        parent: self.kind,
                        requested,
                        offending_grant: Some(spec.name.trim().to_string()),
                        detail: "an inherited parent can grant only values in its launch-time environment snapshot; secret-store authority must be granted to the parent first".to_string(),
                    });
                }
                let snapshot = self.launcher_snapshot.clone();
                Self::launch_from_snapshot(requested, specs, snapshot.clone(), &|name| {
                    snapshot.get(name).cloned()
                })
            }
            (EnvironmentPolicyKind::Granted, EnvironmentPolicyKind::Isolated)
                if specs.is_empty() =>
            {
                Ok(Self {
                    kind: requested,
                    launcher_snapshot: self.launcher_snapshot.clone(),
                    grants: Vec::new(),
                })
            }
            (EnvironmentPolicyKind::Granted, EnvironmentPolicyKind::Granted) => {
                validate_unique_specs(&specs)?;
                let mut grants = Vec::with_capacity(specs.len());
                for spec in &specs {
                    let Some(grant) = self.grants.iter().find(|grant| grant.matches_spec(spec))
                    else {
                        return Err(EnvironmentPolicyError::ChildPolicyExceedsParent {
                            parent: self.kind,
                            requested,
                            offending_grant: Some(spec.name.trim().to_string()),
                            detail: format!(
                                "grant '{}' is not an unchanged subset of the parent grants",
                                spec.name.trim()
                            ),
                        });
                    };
                    grants.push(grant.clone());
                }
                Ok(Self {
                    kind: requested,
                    launcher_snapshot: self.launcher_snapshot.clone(),
                    grants,
                })
            }
            _ => Err(EnvironmentPolicyError::ChildPolicyExceedsParent {
                parent: self.kind,
                requested,
                offending_grant: specs.first().map(|spec| spec.name.trim().to_string()),
                detail:
                    "a child may keep or reduce its parent's environment access, never widen it"
                        .to_string(),
            }),
        }
    }

    pub(crate) fn launcher_value(&self, name: &str) -> Option<&str> {
        self.launcher_snapshot.get(name).map(String::as_str)
    }

    pub(crate) fn launcher_snapshot(&self) -> &BTreeMap<String, String> {
        &self.launcher_snapshot
    }

    /// The resolved grants. Empty for an isolated policy, always.
    pub fn grants(&self) -> &[SessionGrant] {
        &self.grants
    }

    /// The non-secret receipts recorded on the session run-record. Empty for an
    /// isolated policy, which makes `grants: []` a checked property.
    pub fn receipts(&self) -> Vec<GrantReceipt> {
        self.grants.iter().map(SessionGrant::receipt).collect()
    }

    /// Materialize the process environment overlay for `process.exec`: the
    /// `(VAR, value)` pairs for every grant that opted into `expose_as_env`.
    /// Empty for an isolated policy.
    ///
    /// Callers receive uniform pairs and never see the source kind;
    /// [`SessionGrant::exposure`] owns that branch.
    ///
    /// Exposure is session-wide: `harness.env`, providers, and every spawned
    /// command consult the same target mapping.
    pub fn env_exposure(
        &self,
        resolve_secret: &dyn Fn(&str, &str) -> Option<String>,
    ) -> Result<Vec<(String, String)>, EnvironmentPolicyError> {
        self.grants
            .iter()
            .filter_map(|grant| grant.exposure(resolve_secret))
            .collect()
    }

    /// The value this environment exposes under a single environment variable, or
    /// `None` if no grant targets it.
    ///
    /// The narrow counterpart of [`env_exposure`](Self::env_exposure), for a
    /// consumer resolving one variable — harn's own provider-credential lookup.
    /// It resolves *only* the grant that targets `var`, which matters for a
    /// `secret_store` grant: probing an unrelated variable must not reach the
    /// secret store, and one unresolvable grant must not mask an unrelated
    /// credential. Launch validation guarantees at most one matching grant.
    pub fn env_exposure_for(
        &self,
        var: &str,
        resolve_secret: &dyn Fn(&str, &str) -> Option<String>,
    ) -> Result<Option<String>, EnvironmentPolicyError> {
        let Some(grant) = self
            .grants
            .iter()
            .find(|grant| grant.expose_as_env.as_deref() == Some(var))
        else {
            return Ok(None);
        };
        grant
            .exposure(resolve_secret)
            .transpose()
            .map(|pair| pair.map(|(_, value)| value))
    }
}

fn capture_process_environment() -> BTreeMap<String, String> {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

fn validate_unique_specs(specs: &[GrantSpec]) -> Result<(), EnvironmentPolicyError> {
    let mut names = BTreeSet::new();
    let mut targets = BTreeSet::new();
    for spec in specs {
        let name = spec.name.trim();
        if !name.is_empty() && !names.insert(name) {
            return Err(EnvironmentPolicyError::DuplicateGrant {
                name: name.to_string(),
            });
        }
        if let Some(target) = spec.expose_as_env.as_deref().map(str::trim) {
            if !target.is_empty() && !targets.insert(target) {
                return Err(EnvironmentPolicyError::DuplicateExposureTarget {
                    target: target.to_string(),
                });
            }
        }
    }
    Ok(())
}

/// A non-secret record of a grant, safe to persist on a session run-record.
///
/// Carries the grant name, source kind, and optional environment target — never
/// the value or a reversible source reference. `secret_store` account/key
/// pointers are intentionally omitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantReceipt {
    pub name: String,
    pub source_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposed_as_env: Option<String>,
}

/// Errors raised while validating, resolving, or enforcing session grants. All
/// are launch-boundary failures; none carries a secret value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvironmentPolicyError {
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
    /// A grant was declared on a policy that does not accept grants.
    PolicyForbidsGrants {
        policy: EnvironmentPolicyKind,
        attempted: usize,
    },
    /// More than one grant used the same logical name.
    DuplicateGrant { name: String },
    /// More than one grant targeted the same environment variable.
    DuplicateExposureTarget { target: String },
    /// A child requested more environment authority than its parent.
    ChildPolicyExceedsParent {
        parent: EnvironmentPolicyKind,
        requested: EnvironmentPolicyKind,
        offending_grant: Option<String>,
        detail: String,
    },
    /// A `secret_store` grant could not be resolved on exposure.
    MissingSecret { name: String },
}

impl fmt::Display for EnvironmentPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EnvironmentPolicyError::EmptyName => write!(
                f,
                "[environment_policy.empty_grant_name] grant spec has an empty name"
            ),
            EnvironmentPolicyError::EmptyEnvVar { name } => {
                write!(
                    f,
                    "[environment_policy.empty_source_variable] grant '{name}' env source names an empty variable"
                )
            }
            EnvironmentPolicyError::EmptySecretRef { name } => {
                write!(
                    f,
                    "[environment_policy.empty_secret_reference] grant '{name}' secret source names an empty account/key"
                )
            }
            EnvironmentPolicyError::EmptyExposeVar { name } => {
                write!(
                    f,
                    "[environment_policy.empty_exposure_target] grant '{name}' expose target is an empty variable"
                )
            }
            EnvironmentPolicyError::MissingEnv { name, var } => write!(
                f,
                "[environment_policy.source_variable_missing] grant '{name}' env source variable '{var}' is not set in the launcher environment; set it before launch or choose another source"
            ),
            EnvironmentPolicyError::PolicyForbidsGrants { policy, attempted } => write!(
                f,
                "[environment_policy.grants_forbidden] environment policy '{}' forbids grants, but {attempted} were declared; use 'granted' or remove the grants",
                policy.as_str()
            ),
            EnvironmentPolicyError::DuplicateGrant { name } => write!(
                f,
                "[environment_policy.duplicate_grant] grant name '{name}' is declared more than once; give every grant a unique name"
            ),
            EnvironmentPolicyError::DuplicateExposureTarget { target } => write!(
                f,
                "[environment_policy.duplicate_exposure_target] environment target '{target}' is exposed by more than one grant; choose one grant for each target"
            ),
            EnvironmentPolicyError::ChildPolicyExceedsParent {
                parent,
                requested,
                offending_grant: _,
                detail,
            } => write!(
                f,
                "[environment_policy.child_exceeds_parent] child policy '{}' exceeds parent policy '{}': {detail}",
                requested.as_str(),
                parent.as_str()
            ),
            EnvironmentPolicyError::MissingSecret { name } => {
                write!(
                    f,
                    "[environment_policy.secret_unavailable] grant '{name}' is unavailable from the secret store; restore access, rotate the reference, or remove the grant"
                )
            }
        }
    }
}

impl std::error::Error for EnvironmentPolicyError {}

impl EnvironmentPolicyError {
    /// Stable machine-readable code for CLI, ACP, and host integrations.
    pub fn code(&self) -> &'static str {
        match self {
            Self::EmptyName => "environment_policy.empty_grant_name",
            Self::EmptyEnvVar { .. } => "environment_policy.empty_source_variable",
            Self::EmptySecretRef { .. } => "environment_policy.empty_secret_reference",
            Self::EmptyExposeVar { .. } => "environment_policy.empty_exposure_target",
            Self::MissingEnv { .. } => "environment_policy.source_variable_missing",
            Self::PolicyForbidsGrants { .. } => "environment_policy.grants_forbidden",
            Self::DuplicateGrant { .. } => "environment_policy.duplicate_grant",
            Self::DuplicateExposureTarget { .. } => "environment_policy.duplicate_exposure_target",
            Self::ChildPolicyExceedsParent { .. } => "environment_policy.child_exceeds_parent",
            Self::MissingSecret { .. } => "environment_policy.secret_unavailable",
        }
    }

    /// Non-secret structured diagnostic for JSON-RPC and machine consumers.
    pub fn to_json(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "code": self.code(),
            "message": self.to_string(),
        });
        let object = value
            .as_object_mut()
            .expect("environment policy diagnostic is an object");
        match self {
            Self::EmptyEnvVar { name }
            | Self::EmptySecretRef { name }
            | Self::EmptyExposeVar { name }
            | Self::MissingSecret { name } => {
                object.insert("grant".to_string(), serde_json::json!(name));
            }
            Self::MissingEnv { name, var } => {
                object.insert("grant".to_string(), serde_json::json!(name));
                object.insert("sourceVariable".to_string(), serde_json::json!(var));
            }
            Self::PolicyForbidsGrants { policy, attempted } => {
                object.insert("policy".to_string(), serde_json::json!(policy.as_str()));
                object.insert("attemptedGrants".to_string(), serde_json::json!(attempted));
            }
            Self::DuplicateGrant { name } => {
                object.insert("grant".to_string(), serde_json::json!(name));
            }
            Self::DuplicateExposureTarget { target } => {
                object.insert("target".to_string(), serde_json::json!(target));
            }
            Self::ChildPolicyExceedsParent {
                parent,
                requested,
                offending_grant,
                detail,
            } => {
                object.insert(
                    "parentPolicy".to_string(),
                    serde_json::json!(parent.as_str()),
                );
                object.insert(
                    "requestedPolicy".to_string(),
                    serde_json::json!(requested.as_str()),
                );
                object.insert("detail".to_string(), serde_json::json!(detail));
                if let Some(grant) = offending_grant {
                    object.insert("grant".to_string(), serde_json::json!(grant));
                }
            }
            Self::EmptyName => {}
        }
        value
    }
}

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
    fn isolated_rejects_any_grant_at_launch() {
        let specs = vec![secret_grant("gh_token", "gh", "token", None)];
        let err = SessionEnvironment::launch(EnvironmentPolicyKind::Isolated, specs, &no_env)
            .expect_err("isolated must reject grants");
        assert_eq!(
            err,
            EnvironmentPolicyError::PolicyForbidsGrants {
                policy: EnvironmentPolicyKind::Isolated,
                attempted: 1
            }
        );

        // Both isolated constructors are structurally empty. The overall
        // session default remains inherited.
        let environment =
            SessionEnvironment::launch(EnvironmentPolicyKind::Isolated, vec![], &no_env).unwrap();
        assert!(environment.is_isolated());
        assert!(environment.grants().is_empty());
        assert!(environment.receipts().is_empty());
        assert!(SessionEnvironment::isolated().grants().is_empty());
        assert_eq!(
            EnvironmentPolicyKind::default(),
            EnvironmentPolicyKind::Inherited
        );
    }

    #[test]
    fn granted_policy_resolves_once_into_typed_record() {
        let env = env_from(&[("FIREWORKS_API_KEY", "fw-secret-value")]);
        let specs = vec![
            env_grant("fireworks", "FIREWORKS_API_KEY", Some("FIREWORKS_API_KEY")),
            secret_grant("gh_token", "gh", "token", Some("GH_TOKEN")),
        ];
        let environment =
            SessionEnvironment::launch(EnvironmentPolicyKind::Granted, specs, &env).unwrap();

        let grants = environment.grants();
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
        let mut pairs = environment.env_exposure(&resolve_secret).unwrap();
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
        let environment =
            SessionEnvironment::launch(EnvironmentPolicyKind::Granted, specs, &no_env).unwrap();
        let never = |_: &str, _: &str| -> Option<String> {
            panic!("secret resolver must not run for an unexposed grant")
        };
        assert!(environment.env_exposure(&never).unwrap().is_empty());
    }

    #[test]
    fn env_grant_snapshots_value_at_launch() {
        // The launcher env yields "live-at-launch"; the snapshot must hold that
        // value afterward — the child never reads the live environment.
        let at_launch = env_from(&[("TOKEN", "live-at-launch")]);
        let specs = vec![env_grant("t", "TOKEN", Some("TOKEN"))];
        let environment =
            SessionEnvironment::launch(EnvironmentPolicyKind::Granted, specs, &at_launch).unwrap();

        let never_secret = |_: &str, _: &str| -> Option<String> { None };
        let pairs = environment.env_exposure(&never_secret).unwrap();
        assert_eq!(
            pairs,
            vec![("TOKEN".to_string(), "live-at-launch".to_string())]
        );
        // The resolved grant is unaffected by any later env — it holds the
        // launch-time snapshot.
        assert_eq!(
            environment.env_exposure(&never_secret).unwrap(),
            vec![("TOKEN".to_string(), "live-at-launch".to_string())]
        );
    }

    #[test]
    fn restricted_policies_do_not_retain_unrelated_launcher_values() {
        let snapshot = BTreeMap::from([
            ("PATH".to_string(), "/bin".to_string()),
            (
                "UNRELATED_SECRET".to_string(),
                "must-not-be-retained".to_string(),
            ),
        ]);
        let granted = SessionEnvironment::launch_from_snapshot(
            EnvironmentPolicyKind::Granted,
            Vec::new(),
            snapshot,
            &no_env,
        )
        .unwrap();
        assert_eq!(granted.launcher_value("PATH"), Some("/bin"));
        assert_eq!(granted.launcher_value("UNRELATED_SECRET"), None);
    }

    #[test]
    fn receipts_record_shape_and_never_the_value() {
        let env = env_from(&[("FIREWORKS_API_KEY", "fw-secret-value")]);
        let specs = vec![
            env_grant("fireworks", "FIREWORKS_API_KEY", Some("FIREWORKS_API_KEY")),
            secret_grant("gh_token", "gh", "token", None),
        ];
        let environment =
            SessionEnvironment::launch(EnvironmentPolicyKind::Granted, specs, &env).unwrap();

        let receipts = environment.receipts();
        assert_eq!(
            receipts,
            vec![
                GrantReceipt {
                    name: "fireworks".to_string(),
                    source_kind: "env".to_string(),
                    exposed_as_env: Some("FIREWORKS_API_KEY".to_string()),
                },
                GrantReceipt {
                    name: "gh_token".to_string(),
                    source_kind: "secret_store".to_string(),
                    exposed_as_env: None,
                },
            ]
        );

        // The serialized receipts must never contain the snapshotted value or
        // the secret pointer. (SessionGrant/SessionEnvironment are not Serialize,
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

        // Policy kind is a typed, defaulted config field (inherited by default).
        assert_eq!(
            serde_json::from_str::<EnvironmentPolicyKind>("\"granted\"").unwrap(),
            EnvironmentPolicyKind::Granted
        );
        assert_eq!(
            EnvironmentPolicyKind::default(),
            EnvironmentPolicyKind::Inherited
        );
    }

    #[test]
    fn missing_env_source_fails_at_launch() {
        let specs = vec![env_grant("t", "ABSENT_VAR", None)];
        let err = SessionEnvironment::launch(EnvironmentPolicyKind::Granted, specs, &no_env)
            .expect_err("absent env var must fail resolution");
        assert_eq!(
            err,
            EnvironmentPolicyError::MissingEnv {
                name: "t".to_string(),
                var: "ABSENT_VAR".to_string(),
            }
        );
    }

    #[test]
    fn resolve_rejects_empty_fields() {
        let env = env_from(&[("X", "v")]);
        assert_eq!(
            SessionEnvironment::launch(
                EnvironmentPolicyKind::Granted,
                vec![env_grant("", "X", None)],
                &env
            ),
            Err(EnvironmentPolicyError::EmptyName)
        );
        assert_eq!(
            SessionEnvironment::launch(
                EnvironmentPolicyKind::Granted,
                vec![env_grant("t", "", None)],
                &env
            ),
            Err(EnvironmentPolicyError::EmptyEnvVar {
                name: "t".to_string()
            })
        );
        assert_eq!(
            SessionEnvironment::launch(
                EnvironmentPolicyKind::Granted,
                vec![secret_grant("t", "acct", "", None)],
                &env
            ),
            Err(EnvironmentPolicyError::EmptySecretRef {
                name: "t".to_string()
            })
        );
        assert_eq!(
            SessionEnvironment::launch(
                EnvironmentPolicyKind::Granted,
                vec![env_grant("t", "X", Some(" "))],
                &env
            ),
            Err(EnvironmentPolicyError::EmptyExposeVar {
                name: "t".to_string()
            })
        );
    }

    #[test]
    fn duplicate_names_and_targets_fail_with_stable_codes() {
        let env = env_from(&[("A", "a"), ("B", "b")]);
        let duplicate_name = SessionEnvironment::launch(
            EnvironmentPolicyKind::Granted,
            vec![
                env_grant("token", "A", Some("A")),
                env_grant("token", "B", Some("B")),
            ],
            &env,
        )
        .unwrap_err();
        assert_eq!(duplicate_name.code(), "environment_policy.duplicate_grant");

        let duplicate_target = SessionEnvironment::launch(
            EnvironmentPolicyKind::Granted,
            vec![
                env_grant("a", "A", Some("TOKEN")),
                env_grant("b", "B", Some("TOKEN")),
            ],
            &env,
        )
        .unwrap_err();
        assert_eq!(
            duplicate_target.code(),
            "environment_policy.duplicate_exposure_target"
        );
    }

    #[test]
    fn child_policy_can_only_narrow_parent_authority() {
        let snapshot = BTreeMap::from([
            ("TOKEN".to_string(), "parent-value".to_string()),
            ("PATH".to_string(), "/bin".to_string()),
        ]);
        let parent = SessionEnvironment::launch_from_snapshot(
            EnvironmentPolicyKind::Inherited,
            Vec::new(),
            snapshot.clone(),
            &|name| snapshot.get(name).cloned(),
        )
        .unwrap();
        let child = parent
            .narrow(
                EnvironmentPolicyKind::Granted,
                vec![env_grant("token", "TOKEN", Some("TOKEN"))],
            )
            .unwrap();
        assert_eq!(child.kind(), EnvironmentPolicyKind::Granted);
        assert_eq!(child.grants().len(), 1);

        let error = child
            .narrow(EnvironmentPolicyKind::Inherited, Vec::new())
            .unwrap_err();
        assert_eq!(error.code(), "environment_policy.child_exceeds_parent");
        assert_eq!(error.to_json()["parentPolicy"], "granted");
        assert_eq!(error.to_json()["requestedPolicy"], "inherited");

        let error = child
            .narrow(
                EnvironmentPolicyKind::Granted,
                vec![env_grant("other", "OTHER_TOKEN", Some("OTHER_TOKEN"))],
            )
            .unwrap_err();
        let diagnostic = error.to_json();
        assert_eq!(
            diagnostic["code"],
            "environment_policy.child_exceeds_parent"
        );
        assert_eq!(diagnostic["parentPolicy"], "granted");
        assert_eq!(diagnostic["requestedPolicy"], "granted");
        assert_eq!(diagnostic["grant"], "other");
        assert!(diagnostic["message"]
            .as_str()
            .unwrap()
            .contains("unchanged subset of the parent grants"));
    }
}
