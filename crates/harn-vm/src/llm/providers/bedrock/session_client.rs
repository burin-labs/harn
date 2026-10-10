//! Construct the SDK client context from Harn's captured launch authority.

/// Preserve the catalog's declared legacy token input at the Bedrock owner.
/// The current AWS SDK reads only AWS_SESSION_TOKEN. A nonempty canonical
/// token wins; empty tokens have the SDK's absent-token semantics. Resolve
/// lazily so an unrelated failing legacy secret grant cannot mask a valid one.
pub(super) fn session_token(
    lookup: &dyn Fn(&str) -> Result<Option<String>, crate::VmError>,
) -> Result<Option<String>, crate::VmError> {
    for name in ["AWS_SESSION_TOKEN", "AWS_SECURITY_TOKEN"] {
        if let Some(value) = lookup(name)? {
            let value = value.trim();
            if !value.is_empty() {
                return Ok(Some(value.to_string()));
            }
        }
    }
    Ok(None)
}

/// Granted sessions never call this: their static credential restriction is
/// enforced before SDK discovery. A direct VM caller without a declared
/// session retains inherited behavior by capturing its current launcher.
#[cfg(feature = "cloud-aws")]
pub(super) fn provider_config() -> aws_config::provider_config::ProviderConfig {
    let environment = crate::security::session_environment::current_session_environment()
        .unwrap_or_else(crate::security::SessionEnvironment::inherited);
    let mut snapshot: std::collections::HashMap<String, String> = environment
        .launcher_snapshot()
        .iter()
        .map(|(name, value)| {
            // AWS's snapshot map uses exact keys; Windows process lookup does
            // not. Normalize once so mixed-case exported inputs keep the
            // semantics of the SDK's ambient environment reader.
            let name = if cfg!(windows) {
                name.to_ascii_uppercase()
            } else {
                name.clone()
            };
            (name, value.clone())
        })
        .collect();
    if let Some(token) = session_token(&|name| Ok(snapshot.get(name).cloned()))
        .expect("captured environment lookup is infallible")
    {
        snapshot.insert("AWS_SESSION_TOKEN".to_string(), token);
    }
    aws_config::provider_config::ProviderConfig::default().with_environment_snapshot(snapshot)
}

#[cfg(all(test, feature = "cloud-aws"))]
mod tests;

#[cfg(all(test, feature = "cloud-aws"))]
mod ecs_tests;

#[cfg(all(test, feature = "cloud-aws"))]
mod trace_tests;

#[cfg(all(test, feature = "cloud-aws"))]
mod policy_tests;
