//! Presence-only reads of the session environment, for status questions that
//! must not read a stored secret.

use super::session_env_var_with;

/// What [`super::session_env_var`] would find for `name`, answered without reading a
/// stored secret.
///
/// A `secret_store` grant is checked for presence only; when present, the
/// returned value is [`GRANTED_SECRET_PRESENT`], never the secret. Any other
/// value is the same one [`super::session_env_var`] returns, which may itself be a
/// secret reference the caller checks with
/// [`crate::secrets::secret_ref_is_present`]. `Err` carries a store that could
/// only answer through a dialog this process will not show.
pub(crate) fn session_env_presence(name: &str) -> Result<Option<String>, SessionEnvPresenceError> {
    let needs_approval = std::cell::Cell::new(false);
    let present = |account: &str, key: &str| {
        let reference = format!("{}{}/{}", crate::secrets::SECRET_REF_SCHEME, account, key);
        match crate::secrets::secret_ref_is_present(&reference) {
            Ok(Some(true)) => Some(GRANTED_SECRET_PRESENT.to_string()),
            Ok(_) => None,
            Err(error) => {
                needs_approval.set(needs_approval.get() || error.needs_user_approval());
                None
            }
        }
    };
    // An unresolved grant may surface as a policy error or as no value; either
    // way, a store that wanted a dialog is the fact worth reporting.
    match session_env_var_with(name, &present) {
        Ok(Some(value)) => Ok(Some(value)),
        _ if needs_approval.get() => Err(SessionEnvPresenceError::NeedsUserApproval),
        Ok(None) => Ok(None),
        Err(_) => Err(SessionEnvPresenceError::Policy),
    }
}

/// Stands in for a granted secret that [`session_env_presence`] found but did
/// not read. It is not a credential and must never be sent anywhere.
pub(crate) const GRANTED_SECRET_PRESENT: &str = "<granted secret present>";

#[derive(Debug)]
pub(crate) enum SessionEnvPresenceError {
    Policy,
    NeedsUserApproval,
}
