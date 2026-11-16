//! Reader resources resolved before approval compares them to workspace authority.

use super::*;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum PermissionReaderPath {
    Known(String),
    UnresolvedWorkingDirectory { operand: String },
}

/// The workspace remains the authority root. A command's cwd only locates its
/// reader operands; it cannot substitute a new workspace boundary.
pub(crate) fn permission_reader_paths(
    args: &JsonValue,
    workspace: &Path,
) -> Vec<PermissionReaderPath> {
    let context = serde_json::json!({"request": args});
    let argv = request_argv(&context).ok().flatten();
    let commands = if let Some(argv) = &argv {
        vec![argv
            .iter()
            .map(|word| shell_quote_arg(word))
            .collect::<Vec<_>>()
            .join(" ")]
    } else {
        ["command", "cmd"]
            .iter()
            .filter_map(|key| {
                args.get(key)
                    .and_then(JsonValue::as_str)
                    .filter(|source| !source.trim().is_empty())
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>()
    };
    if commands.is_empty() {
        return Vec::new();
    }
    let cwd = args
        .get("cwd")
        .and_then(JsonValue::as_str)
        .filter(|cwd| !cwd.is_empty());
    let base = cwd
        .map(|cwd| {
            crate::workspace_path::expand_home_path(cwd, crate::user_dirs::home_dir().as_deref())
                .map(PathBuf::from)
                .unwrap_or_else(|| workspace.join(cwd))
        })
        .unwrap_or_else(|| workspace.to_path_buf());
    let base = match crate::run_tape::workspace_path(base) {
        Ok(base) => base,
        Err(_) => {
            return vec![PermissionReaderPath::UnresolvedWorkingDirectory {
                operand: cwd.unwrap_or_default().to_owned(),
            }];
        }
    };
    let base = base.canonicalize().unwrap_or(base);
    let physical_workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let relative = base
        .strip_prefix(&physical_workspace)
        .ok()
        .and_then(Path::to_str);
    let mut paths = Vec::new();
    let mut readers = Vec::new();
    if cwd.is_some() {
        paths.push(base.to_string_lossy().into_owned());
    }
    let dialect = request_shell_dialect(&context).dialect;
    for command in commands {
        let mut unproven_directory = false;
        let planned = if let Some(argv) = &argv {
            simple_reader_paths(argv, &base)
        } else if let Some(paths) =
            dialect.and_then(|dialect| literal_reader_paths(&command, dialect, &base))
        {
            Some(paths)
        } else if dialect == Some(crate::shells::ShellDialect::Posix) {
            relative.and_then(|relative| {
                let steps = crate::shells::plan_posix_sequence(&command, relative)?;
                // An earlier process can change directory entries used by a
                // later cd. The plan describes cwd text, not their stability.
                // Only a single process may benefit from a planned directory.
                if steps.len() != 1 {
                    unproven_directory = steps.iter().any(|step| step.cwd != relative);
                    return None;
                }
                let mut planned = Vec::new();
                for step in steps {
                    let directory = physical_workspace.join(&step.cwd);
                    if !step.cwd.is_empty() {
                        planned.push(directory.to_string_lossy().into_owned());
                    }
                    planned.extend(simple_reader_paths(&step.argv, &directory)?);
                }
                Some(planned)
            })
        } else {
            None
        };
        if let Some(planned) = planned {
            paths.extend(planned);
        } else {
            // The original root is not a substitute for an unknown cwd: an
            // inside-looking relative operand could actually name an outside
            // file. Use canonical semantic stages, not command text matching.
            unproven_directory |= dialect != Some(crate::shells::ShellDialect::Posix) || {
                let analysis = super::super::catastrophic::analyze_shell_dialect(
                    crate::shells::ShellDialect::Posix,
                    &command,
                );
                analysis.unresolved
                    || analysis.io_unresolved
                    || analysis
                        .stages
                        .iter()
                        .any(|stage| stage.argv.first().is_some_and(|name| name == "cd"))
            };
            for operand in credential_read_path_candidates(&command) {
                if unproven_directory && !Path::new(&operand).is_absolute() {
                    readers.push(PermissionReaderPath::UnresolvedWorkingDirectory { operand });
                    continue;
                }
                paths.push(workspace.join(&operand).to_string_lossy().into_owned());
                paths.push(base.join(&operand).to_string_lossy().into_owned());
            }
        }
    }
    readers.extend(paths.into_iter().map(PermissionReaderPath::Known));
    readers.sort();
    readers.dedup();
    readers
}

fn literal_reader_paths(
    command: &str,
    dialect: crate::shells::ShellDialect,
    directory: &Path,
) -> Option<Vec<String>> {
    let shape = super::super::catastrophic::shell_command_shape(dialect, command);
    let analysis = super::super::catastrophic::analyze_shell_dialect(dialect, command);
    if shape.exit_evidence.scope != "all_commands"
        || analysis.unresolved
        || analysis.io_unresolved
        || !analysis.redirects.is_empty()
    {
        return None;
    }
    let segments = shape.segments?;
    if segments.len() != 1 || !segments[0].operator.is_empty() || segments[0].stages.len() != 1 {
        return None;
    }
    let stage = &segments[0].stages[0];
    if stage.kind != "command" {
        return None;
    }
    simple_reader_paths(&stage.words, directory)
}

fn simple_reader_paths(argv: &[String], directory: &Path) -> Option<Vec<String>> {
    let analysis = super::super::catastrophic::analyze_argv(argv);
    if analysis.unresolved
        || analysis.io_unresolved
        || analysis.stages.iter().any(|stage| {
            stage.source_argv.as_slice() != argv
                || stage.argv.as_slice() != argv
                || stage.dynamic.iter().any(|dynamic| *dynamic)
                || stage.argv.first().is_some_and(|name| name == "cd")
        })
    {
        return None;
    }
    Some(
        credential_read_path_candidates_from_stages(&analysis.stages)
            .into_iter()
            .map(|operand| directory.join(operand).to_string_lossy().into_owned())
            .collect(),
    )
}
