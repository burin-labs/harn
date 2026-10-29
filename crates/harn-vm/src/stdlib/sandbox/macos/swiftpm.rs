//! SwiftPM compatibility inside an already confined macOS process tree.
//!
//! Direct calls and PATH-resolved project scripts use the same defaults. The
//! generated launchers change arguments, never the surrounding sandbox policy.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::orchestration::CapabilityPolicy;

use super::super::{
    ensure_parent_dirs_scoped, normalized_workspace_roots, openat_file, renameat_name,
    unlinkat_name, warn_once, ScopedMutationTarget,
};

const ADAPTER_ROOT: &str = ".harn-toolchain-cache/swiftpm";
const SUBCOMMANDS: &[&str] = &["build", "test", "run", "package"];
const XCRUN_VALUE_OPTIONS: &[&str] = &["--sdk", "--toolchain"];
const XCRUN_SWITCHES: &[&str] = &[
    "-v",
    "--verbose",
    "-l",
    "--log",
    "-r",
    "--run",
    "-n",
    "--no-cache",
    "-k",
    "--kill-cache",
];

struct DefaultOption {
    name: &'static str,
    value: Option<&'static str>,
}

const DEFAULTS: &[DefaultOption] = &[
    DefaultOption {
        name: "--disable-sandbox",
        value: None,
    },
    DefaultOption {
        name: "--manifest-cache",
        value: Some("local"),
    },
    DefaultOption {
        name: "--cache-path",
        value: Some(".build/harn/swiftpm/cache"),
    },
    DefaultOption {
        name: "--config-path",
        value: Some(".build/harn/swiftpm/config"),
    },
    DefaultOption {
        name: "--security-path",
        value: Some(".build/harn/swiftpm/security"),
    },
];

pub(super) fn compatible_args(program: &str, args: &[String]) -> Vec<String> {
    compatible_args_with_toolset(program, args, None)
}

fn compatible_args_with_toolset(
    program: &str,
    args: &[String],
    toolset: Option<&Path>,
) -> Vec<String> {
    let Some(offset) = subcommand_offset(program, args) else {
        return args.to_vec();
    };
    let rest = &args[offset + 1..];
    let mut rewritten = args[..=offset].to_vec();
    for option in DEFAULTS {
        if !has_option(rest, option.name) {
            rewritten.push(option.name.to_string());
            rewritten.extend(option.value.map(str::to_string));
        }
    }
    if accepts_toolset(args, offset) {
        if let Some(toolset) = toolset {
            rewritten.extend(["--toolset".to_string(), toolset.display().to_string()]);
        }
    }
    rewritten.extend_from_slice(rest);
    rewritten
}

fn accepts_toolset(args: &[String], offset: usize) -> bool {
    !has_option(&args[..offset], "--toolchain")
        && !["--toolset", "--toolchain"]
            .iter()
            .any(|option| has_option(&args[offset + 1..], option))
}

pub(super) fn prepare_args(
    program: &str,
    args: &[String],
    policy: &CapabilityPolicy,
) -> io::Result<Vec<String>> {
    if subcommand_offset(program, args).is_none_or(|offset| !accepts_toolset(args, offset)) {
        return Ok(compatible_args(program, args));
    }
    let Some(root) = normalized_workspace_roots(policy).into_iter().next() else {
        return Ok(compatible_args(program, args));
    };
    let Some(binding) = super::private_temp::prepare(policy)? else {
        return Ok(compatible_args(program, args));
    };
    let swift = if Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        == Some("xcrun")
    {
        Path::new("/usr/bin/swift")
    } else {
        Path::new(program)
    };
    let toolset = super::swiftpm_toolset::prepare(&root, swift, &binding)?;
    Ok(compatible_args_with_toolset(
        program,
        args,
        toolset.as_deref(),
    ))
}

fn subcommand_offset(program: &str, args: &[String]) -> Option<usize> {
    let mut offset = 0;
    match Path::new(program).file_name()?.to_str()? {
        "swift" => {}
        "xcrun" => {
            while let Some(arg) = args.get(offset) {
                if XCRUN_VALUE_OPTIONS.contains(&arg.as_str()) {
                    offset += 2;
                } else if XCRUN_SWITCHES.contains(&arg.as_str())
                    || XCRUN_VALUE_OPTIONS
                        .iter()
                        .any(|name| arg.starts_with(&format!("{name}=")))
                {
                    offset += 1;
                } else {
                    break;
                }
            }
            if args.get(offset).map(String::as_str) != Some("swift") {
                return None;
            }
            offset += 1;
        }
        _ => return None,
    }
    args.get(offset)
        .filter(|arg| SUBCOMMANDS.contains(&arg.as_str()))
        .map(|_| offset)
}

pub(super) fn has_option(args: &[String], option: &str) -> bool {
    args.iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| arg == option || arg.starts_with(&format!("{option}=")))
}

pub(super) fn shell_quote(value: &str) -> String {
    shell_words::quote(value).into_owned()
}

fn launcher_source(name: &str, real_program: &Path, toolset: Option<&Path>) -> String {
    let mut source = format!(
        "#!/bin/bash\n# Generated by Harn's SwiftPM sandbox adapter.\nset -e\nreal={}\nargs=(\"$@\")\noffset=0\n",
        shell_quote(&real_program.to_string_lossy())
    );
    if name == "xcrun" {
        source.push_str(&format!(
            "while (( offset < ${{#args[@]}} )); do\n  case \"${{args[offset]}}\" in\n    {}) ((offset += 2));;\n    {}) ((offset += 1));;\n    *) break;;\n  esac\ndone\n[[ \"${{args[offset]-}}\" == swift ]] || exec \"$real\" \"${{args[@]}}\"\n((offset += 1))\n",
            XCRUN_VALUE_OPTIONS.join("|"),
            XCRUN_SWITCHES.iter().map(|item| item.to_string())
                .chain(XCRUN_VALUE_OPTIONS.iter().map(|item| format!("{item}=*")))
                .collect::<Vec<_>>().join("|")
        ));
    }
    source.push_str(&format!(
        "case \"${{args[offset]-}}\" in\n  {}) ;;\n  *) exec \"$real\" \"${{args[@]}}\";;\nesac\nrest=(\"${{args[@]:offset+1}}\")\ndefaults=()\n",
        SUBCOMMANDS.join("|")
    ));
    for option in DEFAULTS {
        source.push_str(&format!(
            "present=0\nfor arg in \"${{rest[@]}}\"; do\n  [[ \"$arg\" == -- ]] && break\n  case \"$arg\" in {}|{}=*) present=1; break;; esac\ndone\nif (( !present )); then defaults+=({}{}); fi\n",
            option.name, option.name, shell_quote(option.name),
            option.value.map(|value| format!(" {}", shell_quote(value))).unwrap_or_default()
        ));
    }
    if let Some(toolset) = toolset {
        source.push_str(&format!(
            "selected=0\nfor arg in \"${{args[@]:0:offset}}\"; do\n  case \"$arg\" in --toolchain|--toolchain=*) selected=1;; esac\ndone\nfor arg in \"${{rest[@]}}\"; do\n  [[ \"$arg\" == -- ]] && break\n  case \"$arg\" in --toolset|--toolset=*|--toolchain|--toolchain=*) selected=1;; esac\ndone\nif (( !selected )); then defaults+=(--toolset {}); fi\n",
            shell_quote(&toolset.to_string_lossy())
        ));
    }
    source.push_str("exec \"$real\" \"${args[@]:0:offset+1}\" \"${defaults[@]}\" \"${rest[@]}\"\n");
    source
}

/// Add adapters to the child's existing search path without changing which
/// installed tool each adapter ultimately executes. A deliberately empty PATH
/// remains empty, and repeated environment composition does not stack adapters.
pub(in crate::stdlib::sandbox) fn inject_env(
    env: &mut Vec<(String, String)>,
    policy: &CapabilityPolicy,
) {
    if !policy.sandbox_profile.confines_processes()
        || super::super::effective_fallback(policy.sandbox_profile)
            == super::super::SandboxFallback::Off
        || !super::super::active_backend_available()
    {
        return;
    }
    let Some(root) = normalized_workspace_roots(policy).into_iter().next() else {
        return;
    };
    let inherited = env
        .iter()
        .find(|(key, _)| key == "PATH")
        .map(|(_, value)| value.clone())
        .or_else(
            || match crate::stdlib::process::current_session_environment() {
                Some(environment) => environment.launcher_value("PATH").map(str::to_string),
                None => crate::test_env::env_var_seamed("PATH"),
            },
        );
    let Some(inherited) = inherited.filter(|path| !path.is_empty()) else {
        return;
    };
    let binding = super::private_temp::prepare(policy).ok().flatten();
    match adapted_path(&root, &inherited, binding.as_ref()) {
        Ok(Some(path)) => {
            env.retain(|(key, _)| key != "PATH");
            env.push(("PATH".to_string(), path));
        }
        Ok(None) => {}
        Err(error) => warn_once(
            "handler_sandbox_swiftpm_adapter",
            &format!("could not prepare confined SwiftPM launchers: {error}; retaining the original tool search path"),
        ),
    }
}

fn adapted_path(
    root: &Path,
    inherited: &str,
    binding: Option<&super::private_temp::Binding>,
) -> io::Result<Option<String>> {
    let adapter_root = root.join(ADAPTER_ROOT);
    let original_dirs = std::env::split_paths(inherited)
        .filter(|path| !path.starts_with(&adapter_root))
        .collect::<Vec<_>>();
    let original_path = std::env::join_paths(&original_dirs)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let original_env = Some(vec![(
        "PATH".to_string(),
        original_path.to_string_lossy().into_owned(),
    )]);
    let mut launchers = Vec::new();
    for name in ["swift", "xcrun"] {
        let real = crate::stdlib::process::resolve_program_path(name, &original_env, true, &[]);
        if real == name {
            continue;
        }
        let real = PathBuf::from(real);
        let real = if real.is_absolute() {
            real
        } else {
            std::env::current_dir()?.join(real)
        };
        let toolset = match binding {
            Some(binding) => super::swiftpm_toolset::prepare(
                root,
                if name == "swift" {
                    &real
                } else {
                    Path::new("/usr/bin/swift")
                },
                binding,
            )?,
            None => None,
        };
        launchers.push((name, launcher_source(name, &real, toolset.as_deref())));
    }
    if launchers.is_empty() {
        return Ok(None);
    }
    let identity = launchers
        .iter()
        .map(|(name, source)| format!("{name}\0{source}\0"))
        .collect::<String>();
    let relative_dir =
        Path::new(ADAPTER_ROOT).join(harn_kernel::pure::sha256_hex(identity.as_bytes()));
    for (name, source) in launchers {
        write_adapter_file(
            &ScopedMutationTarget {
                root: root.to_path_buf(),
                relative: relative_dir.join(name),
            },
            source.as_bytes(),
        )?;
    }
    let dirs = std::iter::once(root.join(relative_dir)).chain(original_dirs);
    std::env::join_paths(dirs)
        .map(|path| Some(path.to_string_lossy().into_owned()))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

/// Publish executable bytes through the existing descriptor-relative write
/// helpers. Neither a replaced parent nor a leaf symlink can redirect a write.
pub(super) fn write_adapter_file(target: &ScopedMutationTarget, source: &[u8]) -> io::Result<()> {
    let (parent, name) = ensure_parent_dirs_scoped(target)?;
    let parent_fd = parent.as_raw_fd();
    if let Ok(file) = openat_file(
        parent_fd,
        &name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    ) {
        let metadata = file.metadata()?;
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            let mut existing = Vec::new();
            file.take(source.len() as u64 + 1)
                .read_to_end(&mut existing)?;
            if existing == source {
                return Ok(());
            }
        }
    }
    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
    let temporary = format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    );
    let mut file = openat_file(
        parent_fd,
        &temporary,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0o700,
    )?;
    let result = (|| {
        file.set_permissions(std::fs::Permissions::from_mode(0o700))?;
        file.write_all(source)?;
        file.flush()?;
        renameat_name(parent_fd, &temporary, parent_fd, &name)
    })();
    if result.is_err() {
        let _ = unlinkat_name(parent_fd, &temporary, 0);
    }
    result
}

#[cfg(test)]
#[path = "swiftpm_tests.rs"]
mod tests;
