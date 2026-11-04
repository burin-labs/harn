//! Questions asked of a resolved [`SessionGrant`] about its declaration.

use super::{GrantSourceSpec, GrantSpec, SessionGrant};

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
    /// The declaration decides: a secret-store pointer, a declared secret
    /// variable, and a grant bound to one command are credentials. A plain
    /// `env:` grant and a literal are not, whatever they are named.
    pub(crate) fn carries_credential(&self) -> bool {
        self.for_command.is_some()
            || matches!(
                self.source_spec,
                GrantSourceSpec::SecretStore { .. } | GrantSourceSpec::SecretEnv { .. }
            )
    }
}
