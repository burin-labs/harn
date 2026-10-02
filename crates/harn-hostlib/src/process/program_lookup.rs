//! Missing-program evidence captured against the prepared child's environment.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use super::SpawnSpec;

pub(super) fn missing_program(
    spec: &SpawnSpec,
    command: &Command,
    env_cleared: bool,
) -> Option<String> {
    let plan = harn_vm::shells::plan_invocation(&spec.program, &spec.args)?;
    let cwd = command.get_current_dir()?;
    if !cwd.is_dir() {
        return None;
    }
    let value = |name: &str| -> Option<OsString> {
        if let Some(value) = plan.environment.get(name) {
            return Some(value.into());
        }
        let matches = |key: &std::ffi::OsStr| {
            if cfg!(windows) {
                key.to_string_lossy().eq_ignore_ascii_case(name)
            } else {
                key == name
            }
        };
        match command.get_envs().find(|(key, _)| matches(key)) {
            Some((_, value)) => value.map(OsString::from),
            None if !env_cleared => std::env::var_os(name),
            None => None,
        }
    };
    if plan.requires_clean_shell_environment {
        let defines_function = |name: &std::ffi::OsStr| {
            name.to_str().is_some_and(|name| {
                value(name).is_some_and(|value| {
                    name.starts_with("BASH_FUNC_")
                        || value.to_string_lossy().trim_start().starts_with("()")
                })
            })
        };
        if value("BASH_ENV").is_some()
            || value("ENV").is_some()
            || command.get_envs().any(|(name, _)| defines_function(name))
            || plan
                .environment
                .keys()
                .any(|name| defines_function(name.as_ref()))
            || (!env_cleared && std::env::vars_os().any(|(name, _)| defines_function(&name)))
        {
            return None;
        }
    }
    let mut extensions = if cfg!(windows) {
        value("PATHEXT")
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into())
            .to_string_lossy()
            .split(';')
            .filter(|ext| !ext.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    if cfg!(windows)
        && !extensions
            .iter()
            .any(|extension| extension.eq_ignore_ascii_case(".exe"))
    {
        extensions.push(".exe".to_string());
    }
    // The sandbox spawn seam may already have pinned a direct executable to
    // an absolute path. It will run that file even if the child PATH differs.
    if plan.program == spec.program
        && Path::new(command.get_program()).is_absolute()
        && Path::new(command.get_program()).file_name() == Path::new(&spec.program).file_name()
    {
        return absent_executable(Path::new(command.get_program()), &extensions)
            .then_some(plan.program);
    }
    let program = Path::new(&plan.program);
    if program.components().count() > 1 || plan.program.contains('/') {
        return absent_executable(&cwd.join(program), &extensions).then_some(plan.program);
    }
    // An absent PATH lets some shells use an implementation-defined default.
    let path = value("PATH")?;
    for directory in std::env::split_paths(&path) {
        let directory = cwd.join(directory);
        if !absent_executable(&directory.join(program), &extensions) {
            return None;
        }
    }
    Some(plan.program)
}

fn absent_executable(path: &Path, extensions: &[String]) -> bool {
    definitely_absent(path)
        && extensions.iter().all(|extension| {
            let mut candidate = path.as_os_str().to_os_string();
            candidate.push(extension);
            definitely_absent(Path::new(&candidate))
        })
}

fn definitely_absent(path: &Path) -> bool {
    // Permission errors and non-executable existing files are not not-found.
    matches!(path.metadata(), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}
