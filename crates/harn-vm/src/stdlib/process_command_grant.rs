//! What a command-bound (`for_command`) grant changes about the one spawn it
//! reaches, beyond exposing its variable.
//!
//! A program that starts other programs with its own environment would hand
//! a credential to programs the grant never named. Such a program has an
//! [`ExecProfile`], and a command-bound grant reaches it only through
//! [`granted_spawn`], which closes those exec paths for one spawn. Every
//! other seam receives the session environment with the grant stripped
//! ([`strip_profiled_grants`]), so a seam that never calls `granted_spawn`
//! runs the program without the credential instead of with an open exec path.

use std::collections::BTreeMap;
use std::path::Path;

use crate::security::session_environment::command_basename;
use crate::stdlib::sandbox::workspace_env::{gh_config, grant_spawn};
use crate::value::{VmDictExt, VmError, VmValue};

/// How one program can run something else with its environment, and what a
/// granted spawn of it sets so that it cannot.
pub(crate) struct ExecProfile {
    pub(crate) program: &'static str,
    /// Variables set empty, which the program reads as "run nothing".
    pub(crate) empty_env: &'static [&'static str],
    /// Variables naming a program to run, set to one that runs nothing.
    pub(crate) refusing_env: &'static [&'static str],
    /// Variables pointing at a subdirectory of the per-spawn directory.
    pub(crate) dir_env: &'static [(&'static str, &'static str)],
    /// Grant variables the program receives as a stored login instead of an
    /// environment variable. A command-bound grant exposing any other name
    /// is refused for a profiled program.
    pub(crate) credential_env: &'static [&'static str],
    /// Variables set to these values on every granted spawn.
    pub(crate) fixed_env: &'static [(&'static str, &'static str)],
    /// Fill the per-spawn directory from the user's own configuration and
    /// store the credential there, where only the program's host gets it.
    pub(crate) seed: fn(&Path, &str) -> std::io::Result<()>,
    /// Why these arguments would print the credential, if they would.
    pub(crate) refusal: fn(&[String]) -> Option<&'static str>,
    /// Whether these arguments may run with the grant at all. Everything
    /// else runs without it, so an unrecognized subcommand never gets it.
    pub(crate) grants: fn(&[String]) -> bool,
}

const PROFILES: &[&ExecProfile] = &[&gh_config::GH_PROFILE];

/// A program `git` runs that a granted spawn must not: an empty hooks
/// directory, no filesystem monitor, no `ext::` transport, and plain `ssh`
/// from the trusted `PATH`. Command-scope config outranks every config file
/// a repository or workspace could carry.
const GIT_HARDENING: [(&str, &str); 4] = [
    ("core.hooksPath", ""),
    ("core.fsmonitor", "false"),
    ("protocol.ext.allow", "never"),
    ("core.sshCommand", "ssh"),
];
const HOOKS_DIR: &str = "hooks";

/// A program gh or git runs in place of a browser or editor. `/usr/bin/false`
/// exists on every supported Unix host and is not on a workspace path.
#[cfg(unix)]
pub(crate) const REFUSING_COMMAND: &str = "/usr/bin/false";
/// Windows has no `false`; a device name is not an executable, so a spawn of
/// it fails before anything runs.
#[cfg(not(unix))]
pub(crate) const REFUSING_COMMAND: &str = "NUL";

fn profile_for(program: &str) -> Option<&'static ExecProfile> {
    let name = command_basename(program);
    PROFILES
        .iter()
        .copied()
        .find(|profile| profile.program == name)
}

fn command_bound_vars(program: &str) -> Vec<String> {
    super::current_session_environment()
        .map(|environment| {
            environment
                .grants()
                .iter()
                .filter(|grant| grant.for_command().is_some() && grant.reaches_spawn(Some(program)))
                .filter_map(|grant| grant.exposed_env_var().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a command-bound grant that exposes a variable reaches a spawn of
/// `program`. Session-wide grants do not count: they reach every spawn and
/// never motivate spawning a shell command directly.
pub fn command_bound_grant_reaches(program: &str) -> bool {
    !command_bound_vars(program).is_empty()
}

/// Remove from a session environment every variable a command-bound grant
/// exposes to `program`, when `program` has an [`ExecProfile`].
pub(crate) fn strip_profiled_grants(program: &str, env: &mut BTreeMap<String, String>) {
    if profile_for(program).is_some() {
        for var in command_bound_vars(program) {
            env.remove(&var);
        }
    }
}

/// Caller and ambient variables a granted spawn keeps. Everything else the
/// caller sets or the session carries is dropped: proxy, TLS, debug and
/// token variables would each route or reveal the credential.
fn kept_variable(key: &str) -> bool {
    matches!(key, "LANG" | "TERM" | "TMPDIR" | "TZ" | "NO_COLOR") || key.starts_with("LC_")
}

/// One spawn that receives a command-bound grant: the canonical program file
/// to run, the whole caller-supplied environment it may set (`env`, which
/// replaces the request's), and the inherited names to remove (`remove`).
/// Holds the per-spawn directory, which is removed when this is dropped, so
/// keep it alive until the child has exited.
pub struct GrantedSpawn {
    pub program: String,
    pub env: Vec<(String, String)>,
    pub remove: Vec<String>,
    _dir: grant_spawn::SpawnDir,
}

impl GrantedSpawn {
    /// Hold while launching the program: its own sandbox profile may then
    /// read its private directory, which every other confined child is
    /// denied.
    pub fn launching(&self) -> grant_spawn::OwnSpawnLaunch {
        grant_spawn::OwnSpawnLaunch::begin(self._dir.path())
    }
}

/// Prepare a spawn of `program args` that receives the command-bound grants
/// reaching it, or `None` when it receives none.
///
/// No grant is given when the program has no [`ExecProfile`] (the plain
/// session environment already carries it), when the profile does not grant
/// these arguments (for gh, anything but a read), when `overlay` sets a variable
/// that could reopen an exec path, or when `program` does not resolve to a
/// native executable outside every workspace and write root
/// ([`grant_spawn`]). Arguments that would print or pass on the credential
/// are refused with an error, as is a write root overlapping the user's git,
/// SSH or gh configuration. Any failure to build the private directory is an error, never
/// a grant with an open exec path.
pub fn granted_spawn(
    program: &str,
    args: &[String],
    cwd: &Path,
    overlay: &BTreeMap<String, String>,
) -> Result<Option<GrantedSpawn>, VmError> {
    let Some(profile) = profile_for(program) else {
        return Ok(None);
    };
    let vars = command_bound_vars(program);
    if vars.is_empty() {
        return Ok(None);
    }
    if let Some(reason) = (profile.refusal)(args) {
        return Err(refusal(profile, reason.to_string()));
    }
    if !(profile.grants)(args) || overlay.keys().any(|key| reopens_exec_path(profile, key)) {
        return Ok(None);
    }
    let Some(session) = super::session_env_with_command_grants(program)? else {
        return Ok(None);
    };
    let untrusted = grant_spawn::untrusted_roots(cwd);
    let path = session.get("PATH").cloned().unwrap_or_default();
    let entries = grant_spawn::trusted_path_entries(&path, &untrusted);
    let Some(canonical) = grant_spawn::trusted_program(program, &entries, &untrusted) else {
        return Ok(None);
    };
    let home = crate::user_dirs::home_dir()
        .and_then(|home| std::fs::canonicalize(home).ok())
        .filter(|home| !untrusted.iter().any(|root| home.starts_with(root)))
        .ok_or_else(|| {
            refusal(
                profile,
                "the home directory is missing or inside a workspace".to_string(),
            )
        })?;
    // Git and gh read these as the user's own; a root a confined child can
    // write that contains or lies inside one would let it plant config.
    if let Some(path) = grant_spawn::trusted_config_paths(&home)
        .into_iter()
        .find(|path| {
            untrusted
                .iter()
                .any(|root| path.starts_with(root) || root.starts_with(path))
        })
    {
        return Err(refusal(
            profile,
            format!(
                "{} overlaps a directory a workspace or confined process can write",
                path.display()
            ),
        ));
    }
    if let Some(var) = vars
        .iter()
        .find(|var| !profile.credential_env.contains(&var.as_str()))
    {
        return Err(refusal(
            profile,
            format!("a grant for it may expose only a stored login, not {var}"),
        ));
    }
    // The values were registered for redaction when the session environment
    // resolved them; the token reaches the program only through its stored
    // login, never its environment.
    let Some(token) = vars.iter().find_map(|var| session.get(var).cloned()) else {
        return Ok(None);
    };
    let mut subdirs: Vec<&str> = profile.dir_env.iter().map(|(_, dir)| *dir).collect();
    subdirs.push(HOOKS_DIR);
    let dir = grant_spawn::create_spawn_dir(&untrusted, &subdirs)
        .and_then(|dir| (profile.seed)(dir.path(), &token).map(|()| dir))
        .map_err(|error| refusal(profile, format!("preparing its private directory: {error}")))?;
    let path = std::env::join_paths(&entries)
        .map_err(|error| refusal(profile, format!("joining the trusted PATH: {error}")))?;
    let mut env: Vec<(String, String)> = overlay
        .iter()
        .filter(|(key, _)| kept_variable(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    env.extend([
        ("PATH".to_string(), path.to_string_lossy().into_owned()),
        ("HOME".to_string(), home.display().to_string()),
        (
            "XDG_CONFIG_HOME".to_string(),
            home.join(".config").display().to_string(),
        ),
        ("GIT_CONFIG_PARAMETERS".to_string(), String::new()),
        (
            "GIT_CONFIG_COUNT".to_string(),
            GIT_HARDENING.len().to_string(),
        ),
    ]);
    let hooks = dir.path().join(HOOKS_DIR).display().to_string();
    for (index, (key, value)) in GIT_HARDENING.iter().enumerate() {
        let value = if value.is_empty() {
            hooks.as_str()
        } else {
            value
        };
        env.push((format!("GIT_CONFIG_KEY_{index}"), key.to_string()));
        env.push((format!("GIT_CONFIG_VALUE_{index}"), value.to_string()));
    }
    env.extend(
        profile
            .empty_env
            .iter()
            .map(|key| (key.to_string(), String::new())),
    );
    env.extend(
        profile
            .refusing_env
            .iter()
            .map(|key| (key.to_string(), REFUSING_COMMAND.to_string())),
    );
    env.extend(profile.dir_env.iter().map(|(key, subdir)| {
        (
            key.to_string(),
            dir.path().join(subdir).display().to_string(),
        )
    }));
    env.extend(
        profile
            .fixed_env
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string())),
    );
    env.extend(grant_spawn::network_env());
    // Every inherited name the spawn does not set or keep is removed, from
    // the session set and from this process alike.
    let remove = session
        .into_keys()
        .chain(std::env::vars_os().filter_map(|(key, _)| key.into_string().ok()))
        .filter(|key| !kept_variable(key) && !env.iter().any(|(set, _)| set == key))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(Some(GrantedSpawn {
        program: canonical.display().to_string(),
        env,
        remove,
        _dir: dir,
    }))
}

/// A caller variable that could point the program, or a helper it runs, at
/// something else to execute. A request that sets one gets no grant.
fn reopens_exec_path(profile: &ExecProfile, key: &str) -> bool {
    matches!(
        key,
        "PATH" | "HOME" | "PAGER" | "BROWSER" | "EDITOR" | "VISUAL"
    ) || ["GIT_", "GH_", "SSH_", "LD_", "DYLD_", "XDG_"]
        .iter()
        .any(|prefix| key.starts_with(prefix))
        || profile.empty_env.contains(&key)
        || profile.refusing_env.contains(&key)
        || profile.dir_env.iter().any(|(name, _)| *name == key)
}

fn refusal(profile: &ExecProfile, reason: String) -> VmError {
    let mut error = crate::value::DictMap::new();
    error.put_str("code", "environment_policy.command_grant_refused");
    error.put_str("program", profile.program);
    error.put_str("reason", &reason);
    error.put_str(
        "message",
        format!(
            "[environment_policy.command_grant_refused] {}: {reason}; the grant was not exposed",
            profile.program
        ),
    );
    VmError::Thrown(VmValue::dict(error))
}
