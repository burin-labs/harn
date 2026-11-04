//! Questions asked of a resolved [`SessionGrant`] about its declaration.

use super::{GrantSourceSpec, GrantSpec, SessionGrant};
use crate::environment_registry::{sensitivity_for, EnvironmentSensitivity};

impl SessionGrant {
    pub(super) fn matches_spec(&self, spec: &GrantSpec) -> bool {
        self.name == spec.name.trim()
            && self.source_spec == spec.source
            && self.expose_as_env.as_deref() == spec.expose_as_env.as_deref().map(str::trim)
            && self.for_command.as_deref() == spec.for_command.as_deref().map(str::trim)
            && self.audience == spec.expose_to
    }

    /// Whether this grant's value is a credential to keep out of output.
    ///
    /// A secret-store pointer and a grant bound to one command are credentials
    /// by declaration. A session-wide literal never is. A session-wide `env:`
    /// grant is also how a host passes paths and modes, and hiding those would
    /// rewrite ordinary output, so its variable names decide: the source and the
    /// exposed name both count, in any case, so a rename does not hide a token.
    pub(crate) fn carries_credential(&self) -> bool {
        if self.for_command.is_some() {
            return true;
        }
        match &self.source_spec {
            GrantSourceSpec::SecretStore { .. } => true,
            GrantSourceSpec::Literal { .. } => false,
            GrantSourceSpec::Env { var } => [Some(var.as_str()), self.expose_as_env.as_deref()]
                .into_iter()
                .flatten()
                .any(|name| {
                    sensitivity_for(&name.to_ascii_uppercase())
                        == EnvironmentSensitivity::Credential
                }),
        }
    }
}
