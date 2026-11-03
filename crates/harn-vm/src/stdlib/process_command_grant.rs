//! What a command-bound (`for_command`) grant changes about the one spawn it
//! reaches, beyond exposing its variable.

use crate::security::session_environment::command_basename;
use crate::value::VmError;

/// Whether a command-bound grant that exposes a variable reaches a spawn of
/// `program`. Session-wide grants do not count: they reach every spawn and
/// never motivate spawning a shell command directly.
pub fn command_bound_grant_reaches(program: &str) -> bool {
    super::current_session_environment().is_some_and(|environment| {
        environment.grants().iter().any(|grant| {
            grant.for_command().is_some()
                && grant.exposed_env_var().is_some()
                && grant.reaches_spawn(Some(program))
        })
    })
}

/// Entries a spawn of `program` must carry, last, because a command-bound
/// grant reaches it. A program that can start other programs with its own
/// environment has those exec paths closed here, so the credential stays in
/// the one process the grant named. Empty when no command-bound grant
/// reaches `program`.
///
/// A key some grant reaching this spawn exposes is left to that grant: the
/// host declared it.
pub fn command_grant_env_hardening(program: &str) -> Result<Vec<(String, String)>, VmError> {
    if !command_bound_grant_reaches(program) {
        return Ok(Vec::new());
    }
    let mut env = match command_basename(program) {
        "gh" => crate::stdlib::sandbox::workspace_env::gh_config::granted_gh_env()?,
        _ => return Ok(Vec::new()),
    };
    if let Some(environment) = super::current_session_environment() {
        env.retain(|(key, _)| {
            !environment.grants().iter().any(|grant| {
                grant.exposed_env_var() == Some(key.as_str()) && grant.reaches_spawn(Some(program))
            })
        });
    }
    Ok(env)
}
