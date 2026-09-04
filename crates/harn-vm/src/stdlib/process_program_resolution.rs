//! Resolve a bare program name (`node`, not `/usr/bin/node`) to an absolute
//! path before a child is spawned. Split out of `process.rs` (via `#[path]`,
//! same pattern as `process_tests.rs`) to keep that file under the
//! source-length ratchet cap.
//!
//! Two production seams call into this: `run_captured_spawn`
//! (`harness.process.exec`, which already has a fully-resolved child env map
//! to search) and `sandbox::std_command_for`/`tokio_command_for` (the seam
//! behind the agent's `run` tool, which does not — `resolve_program_path_for_spawn`
//! always searches THIS process's own live `PATH` instead). Searching here,
//! in the parent, with this process's own filesystem access, removes the
//! child's PATH resolution from the spawn path entirely instead of leaving
//! it to the OS at spawn time against the child's own environment block
//! (harn#7993).

/// The value `name` will have in the child a spawn seam is about to launch,
/// without spawning anything — mirrors that child env's three build modes
/// (a closed `resolved_environment`, an explicit `env_clear` replace, or a
/// merge overlay atop this process's own env) so a resolver can search the
/// PATH the child will actually see.
fn effective_env_value(
    name: &str,
    resolved_environment: &Option<Vec<(String, String)>>,
    env_clear: bool,
    overlay: &[(String, String)],
) -> Option<String> {
    let matches_name = |key: &str| {
        if cfg!(windows) {
            key.eq_ignore_ascii_case(name)
        } else {
            key == name
        }
    };
    let find = |env: &[(String, String)]| {
        env.iter()
            .find(|(key, _)| matches_name(key))
            .map(|(_, value)| value.clone())
    };
    if let Some(env) = resolved_environment {
        return find(env);
    }
    if env_clear {
        return find(overlay);
    }
    // Merge mode, no closed session env: `overlay` only overrides specific
    // keys and the child otherwise inherits this process's env wholesale.
    find(overlay).or_else(|| std::env::var(name).ok())
}

/// Resolve a bare program name to an absolute path by searching the child's
/// `PATH`/`PATHEXT` here in the parent's filesystem, instead of leaving that
/// search to the OS against the child's env block. Falls through unchanged
/// for a path-shaped or unresolvable name — a missing command still fails
/// as before.
pub(crate) fn resolve_program_path(
    program: &str,
    resolved_environment: &Option<Vec<(String, String)>>,
    env_clear: bool,
    overlay: &[(String, String)],
) -> String {
    if program.contains(std::path::MAIN_SEPARATOR) || program.contains('/') {
        return program.to_string();
    }
    let Some(path_value) = effective_env_value("PATH", resolved_environment, env_clear, overlay)
    else {
        return program.to_string();
    };
    let extensions: Vec<String> = if cfg!(windows) {
        effective_env_value("PATHEXT", resolved_environment, env_clear, overlay)
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string())
            .split(';')
            .filter(|ext| !ext.is_empty())
            .map(|ext| ext.to_string())
            .collect()
    } else {
        Vec::new()
    };
    for dir in std::env::split_paths(&path_value) {
        let exact = dir.join(program);
        if is_executable_candidate(&exact) {
            return exact.display().to_string();
        }
        for ext in &extensions {
            let candidate = dir.join(format!("{program}{ext}"));
            if is_executable_candidate(&candidate) {
                return candidate.display().to_string();
            }
        }
    }
    program.to_string()
}

/// A real file, and (on Unix, where the executable bit is meaningful and
/// PATH lookup must respect it) executable by someone.
fn is_executable_candidate(candidate: &std::path::Path) -> bool {
    let Ok(metadata) = candidate.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// [`resolve_program_path`] for a seam with no `CapturedSpawn` env map of its
/// own — `sandbox::std_command_for`, the seam behind the agent's `run` tool
/// (harn#7993). Deliberately always searches THIS process's own live `PATH`,
/// never a session-resolved or otherwise child-shaped one: if the child's
/// PATH is the broken half of this seam, resolving against it would just
/// reproduce the failure instead of working around it. Env *policy* (what
/// the child's environment variables end up being) is untouched — this only
/// decides which file `Command::new` is given, and an absolute path never
/// depends on the child's PATH to be found.
pub(crate) fn resolve_program_path_for_spawn(program: &str) -> String {
    resolve_program_path(program, &None, false, &[])
}
