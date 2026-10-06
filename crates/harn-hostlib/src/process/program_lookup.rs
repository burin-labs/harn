//! Missing-program evidence captured against the prepared child's environment.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use super::SpawnSpec;

/// Filesystem presence is lookup evidence, never verifier identity or success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramLookupObservation {
    Absent { program: String },
    Present { path: std::path::PathBuf },
    Unmeasured(ProgramLookupUnmeasured),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramLookupUnmeasured {
    InvocationNotStatic,
    WorkingDirectoryUnavailable,
    ShellStartupUnbounded,
    SearchPathUnavailable,
    FilesystemUnavailable,
}

#[cfg(target_os = "linux")]
pub(super) fn pinned_verifier(
    spec: &SpawnSpec,
) -> Result<Option<harn_vm::verifier_provenance::PinnedVerifierLaunch>, super::ProcessError> {
    let cwd = spec
        .cwd
        .clone()
        .map_or_else(std::env::current_dir, Ok)
        .map_err(|error| {
            super::ProcessError::Spawn(format!("verifier working directory: {error}"))
        })?;
    harn_vm::prepared_run::prepared_source_verifier(&spec.program, &spec.args, &cwd)
        .and_then(|verifier| verifier.map(|value| value.launch()).transpose())
        .map_err(super::ProcessError::Spawn)
}

pub(super) fn missing_program(
    spec: &SpawnSpec,
    command: &Command,
    env_cleared: bool,
) -> Option<String> {
    match observe_program_lookup(spec, command, env_cleared) {
        ProgramLookupObservation::Absent { program } => Some(program),
        ProgramLookupObservation::Present { .. } | ProgramLookupObservation::Unmeasured(_) => None,
    }
}

/// Observe against the prepared command without executing shell or loader code.
/// This does not authorize shell conversion or attest a module selected by it.
pub fn observe_program_lookup(
    spec: &SpawnSpec,
    command: &Command,
    env_cleared: bool,
) -> ProgramLookupObservation {
    use ProgramLookupObservation::{Absent, Unmeasured};
    use ProgramLookupUnmeasured::*;

    let Some(plan) = harn_vm::shells::plan_invocation(&spec.program, &spec.args) else {
        return Unmeasured(InvocationNotStatic);
    };
    let Some(cwd) = command.get_current_dir() else {
        return Unmeasured(WorkingDirectoryUnavailable);
    };
    if !cwd.is_dir() {
        return Unmeasured(WorkingDirectoryUnavailable);
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
            return Unmeasured(ShellStartupUnbounded);
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
        return observe_candidates(Path::new(command.get_program()), &extensions, &plan.program);
    }
    let program = Path::new(&plan.program);
    if program.components().count() > 1 || plan.program.contains('/') {
        return observe_candidates(&cwd.join(program), &extensions, &plan.program);
    }
    // An absent PATH lets some shells use an implementation-defined default.
    let Some(path) = value("PATH") else {
        return Unmeasured(SearchPathUnavailable);
    };
    let mut unavailable = false;
    for directory in std::env::split_paths(&path) {
        let directory = cwd.join(directory);
        match observe_candidates(&directory.join(program), &extensions, &plan.program) {
            present @ ProgramLookupObservation::Present { .. } => return present,
            Unmeasured(_) => unavailable = true,
            Absent { .. } => {}
        }
    }
    if unavailable {
        Unmeasured(FilesystemUnavailable)
    } else {
        Absent {
            program: plan.program,
        }
    }
}

/// Every candidate must be measured absent to produce an absence observation.
fn observe_candidates(
    path: &Path,
    extensions: &[String],
    program: &str,
) -> ProgramLookupObservation {
    let candidates =
        std::iter::once(path.to_path_buf()).chain(extensions.iter().map(|extension| {
            let mut candidate = path.as_os_str().to_os_string();
            candidate.push(extension);
            candidate.into()
        }));
    let mut unavailable = false;
    for candidate in candidates {
        match candidate.metadata() {
            Ok(_) => return ProgramLookupObservation::Present { path: candidate },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => unavailable = true,
        }
    }
    if unavailable {
        ProgramLookupObservation::Unmeasured(ProgramLookupUnmeasured::FilesystemUnavailable)
    } else {
        ProgramLookupObservation::Absent {
            program: program.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(program: &str, args: &[&str]) -> SpawnSpec {
        SpawnSpec {
            builtin: "program_lookup_observation_test",
            program: program.to_string(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            cwd: None,
            env: Default::default(),
            env_remove: Vec::new(),
            env_mode: super::super::EnvMode::Replace,
            use_stdin: false,
            configure_process_group: false,
            owner_death: super::super::OwnerDeathPolicy::None,
            output_capture: super::super::OutputCapture::Pipe,
        }
    }

    #[test]
    fn absent_path_is_unmeasured_until_the_search_path_is_known() {
        let root = tempfile::tempdir().unwrap();
        let spec = spec("absent_verifier_9413", &[]);
        let mut command = Command::new(&spec.program);
        command.env_clear().current_dir(root.path());
        assert_eq!(
            observe_program_lookup(&spec, &command, true),
            ProgramLookupObservation::Unmeasured(ProgramLookupUnmeasured::SearchPathUnavailable),
        );
        assert_eq!(missing_program(&spec, &command, true), None);
        command.env("PATH", root.path());
        assert_eq!(
            observe_program_lookup(&spec, &command, true),
            ProgramLookupObservation::Absent {
                program: spec.program.clone()
            },
        );
        assert_eq!(
            missing_program(&spec, &command, true),
            Some(spec.program.clone())
        );
        let candidate = root.path().join(&spec.program);
        std::fs::write(&candidate, b"not an executable or a loader attestation").unwrap();
        assert_eq!(
            observe_program_lookup(&spec, &command, true),
            ProgramLookupObservation::Present { path: candidate },
        );
        assert_eq!(missing_program(&spec, &command, true), None);
    }

    #[test]
    fn startup_code_is_not_executed_to_measure_program_presence() {
        let root = tempfile::tempdir().unwrap();
        let spec = spec("/bin/bash", &["-c", "absent_verifier_9413"]);
        let marker = root.path().join("startup-fired");
        let startup = root.path().join("startup.sh");
        std::fs::write(&startup, format!("touch '{}'", marker.display())).unwrap();
        let mut command = Command::new(&spec.program);
        command
            .env_clear()
            .current_dir(root.path())
            .env("PATH", root.path())
            .env("BASH_ENV", startup);
        assert_eq!(
            observe_program_lookup(&spec, &command, true),
            ProgramLookupObservation::Unmeasured(ProgramLookupUnmeasured::ShellStartupUnbounded),
        );
        assert!(!marker.exists());
        assert_eq!(missing_program(&spec, &command, true), None);
    }

    #[test]
    fn inaccessible_candidate_is_not_measured_absent() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("ordinary-file");
        std::fs::write(&parent, b"not a directory").unwrap();
        assert_eq!(
            observe_candidates(&parent.join("verifier"), &[], "verifier"),
            ProgramLookupObservation::Unmeasured(ProgramLookupUnmeasured::FilesystemUnavailable),
        );
    }
}
