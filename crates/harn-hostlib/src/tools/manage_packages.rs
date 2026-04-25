//! `tools/manage_packages` — install / add / remove / update / refresh deps
//! across the package ecosystems hostlib can drive.
//!
//! Schema: `schemas/tools/manage_packages.{request,response}.json`.
//!
//! Behavior:
//! - `ecosystem` is required when `cwd` doesn't contain a recognized
//!   manifest. (Swift's implementation refused to infer; we keep that
//!   contract — callers must opt in to a specific manager.)
//! - `lockfile_changed` is computed by snapshotting the relevant lockfile
//!   path before/after the spawn and comparing mtimes. We don't read
//!   contents to keep the cost predictable on large lockfiles.
//! - Approval/UX is the embedder's responsibility — by the time this
//!   builtin is called, the host has already obtained user consent.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::lang::{detect, Ecosystem};
use crate::tools::payload::{
    optional_bool, optional_string, optional_string_list, require_dict_arg, require_string,
};
use crate::tools::proc::{self, SpawnRequest};
use crate::tools::response::ResponseBuilder;

pub(crate) const NAME: &str = "hostlib_tools_manage_packages";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    Install,
    Add,
    Remove,
    Update,
    Refresh,
}

impl Operation {
    fn parse(s: &str) -> Option<Operation> {
        match s {
            "install" => Some(Operation::Install),
            "add" => Some(Operation::Add),
            "remove" => Some(Operation::Remove),
            "update" => Some(Operation::Update),
            "refresh" => Some(Operation::Refresh),
            _ => None,
        }
    }
}

pub(crate) fn handle(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let map = require_dict_arg(NAME, args)?;
    let operation_str = require_string(NAME, &map, "operation")?;
    let operation = Operation::parse(&operation_str).ok_or(HostlibError::InvalidParameter {
        builtin: NAME,
        param: "operation",
        message: format!(
            "expected one of install, add, remove, update, refresh — got {operation_str:?}"
        ),
    })?;

    let cwd_raw = optional_string(NAME, &map, "cwd")?;
    let cwd_path = proc::parse_cwd(NAME, cwd_raw.as_deref())?;
    let cwd_for_detect = cwd_path
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let ecosystem = match optional_string(NAME, &map, "ecosystem")? {
        Some(name) => Ecosystem::parse(&name).ok_or(HostlibError::InvalidParameter {
            builtin: NAME,
            param: "ecosystem",
            message: format!("unknown ecosystem {name:?}"),
        })?,
        None => detect(&cwd_for_detect).ok_or(HostlibError::InvalidParameter {
            builtin: NAME,
            param: "ecosystem",
            message: "no recognized manifest in cwd; pass `ecosystem` explicitly".to_string(),
        })?,
    };

    let packages = optional_string_list(NAME, &map, "packages")?.unwrap_or_default();
    let dev = optional_bool(NAME, &map, "dev")?.unwrap_or(false);

    let argv =
        build_argv(ecosystem, operation, &packages, dev).ok_or(HostlibError::InvalidParameter {
            builtin: NAME,
            param: "operation",
            message: format!(
                "operation {} not implemented for ecosystem {}",
                operation_str,
                ecosystem.name()
            ),
        })?;

    let lockfile = lockfile_for(ecosystem).map(|name| cwd_for_detect.join(name));
    let lockfile_before = lockfile.as_deref().and_then(snapshot_mtime);

    let (program, args_tail) = match argv.split_first() {
        Some((first, rest)) => (first.clone(), rest.to_vec()),
        None => {
            return Err(HostlibError::Backend {
                builtin: NAME,
                message: "internal: empty argv".to_string(),
            });
        }
    };

    let outcome = proc::run(SpawnRequest {
        builtin: NAME,
        program,
        args: args_tail,
        cwd: cwd_path,
        env: BTreeMap::new(),
        stdin: None,
        timeout: None,
    })?;

    let lockfile_after = lockfile.as_deref().and_then(snapshot_mtime);
    let lockfile_changed = lockfile_before != lockfile_after;

    Ok(ResponseBuilder::new()
        .str("operation", operation_str)
        .str("ecosystem", ecosystem.name())
        .int("exit_code", outcome.exit_code as i64)
        .str("stdout", outcome.stdout)
        .str("stderr", outcome.stderr)
        .int("duration_ms", outcome.duration.as_millis() as i64)
        .bool("lockfile_changed", lockfile_changed)
        .build())
}

fn lockfile_for(eco: Ecosystem) -> Option<&'static str> {
    Some(match eco {
        Ecosystem::Cargo => "Cargo.lock",
        Ecosystem::Npm => "package-lock.json",
        Ecosystem::Pnpm => "pnpm-lock.yaml",
        Ecosystem::Yarn => "yarn.lock",
        Ecosystem::Uv => "uv.lock",
        Ecosystem::Poetry => "poetry.lock",
        Ecosystem::Bundler => "Gemfile.lock",
        Ecosystem::Composer => "composer.lock",
        Ecosystem::Swift => "Package.resolved",
        Ecosystem::Go => "go.sum",
        // No canonical lockfile path.
        Ecosystem::Pip | Ecosystem::Gradle | Ecosystem::Maven | Ecosystem::Dotnet => return None,
    })
}

fn snapshot_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn build_argv(
    eco: Ecosystem,
    op: Operation,
    packages: &[String],
    dev: bool,
) -> Option<Vec<String>> {
    use Ecosystem::*;
    use Operation::*;
    let pkgs = packages.to_vec();
    let pkgs_some = !pkgs.is_empty();
    Some(match (eco, op) {
        (Cargo, Add) if pkgs_some => prepend("cargo", &["add"], &pkgs),
        (Cargo, Remove) if pkgs_some => prepend("cargo", &["remove"], &pkgs),
        (Cargo, Install | Refresh) => vec!["cargo".into(), "fetch".into()],
        (Cargo, Update) => {
            let mut argv = vec!["cargo".into(), "update".into()];
            for p in &pkgs {
                argv.push("-p".into());
                argv.push(p.clone());
            }
            argv
        }
        (Npm, Add | Install) if pkgs_some => {
            let mut argv = vec!["npm".into(), "install".into()];
            if dev {
                argv.push("--save-dev".into());
            }
            argv.extend(pkgs);
            argv
        }
        (Npm, Install) => vec!["npm".into(), "install".into()],
        (Npm, Remove) if pkgs_some => prepend("npm", &["uninstall"], &pkgs),
        (Npm, Update) => prepend_or_default("npm", &["update"], &pkgs),
        (Npm, Refresh) => vec!["npm".into(), "ci".into()],
        (Pnpm, Add | Install) if pkgs_some => {
            let mut argv = vec!["pnpm".into(), "add".into()];
            if dev {
                argv.push("--save-dev".into());
            }
            argv.extend(pkgs);
            argv
        }
        (Pnpm, Install) => vec!["pnpm".into(), "install".into()],
        (Pnpm, Remove) if pkgs_some => prepend("pnpm", &["remove"], &pkgs),
        (Pnpm, Update) => prepend_or_default("pnpm", &["update"], &pkgs),
        (Pnpm, Refresh) => vec!["pnpm".into(), "install".into(), "--frozen-lockfile".into()],
        (Yarn, Add | Install) if pkgs_some => {
            let mut argv = vec!["yarn".into(), "add".into()];
            if dev {
                argv.push("--dev".into());
            }
            argv.extend(pkgs);
            argv
        }
        (Yarn, Install) => vec!["yarn".into(), "install".into()],
        (Yarn, Remove) if pkgs_some => prepend("yarn", &["remove"], &pkgs),
        (Yarn, Update) => prepend_or_default("yarn", &["upgrade"], &pkgs),
        (Yarn, Refresh) => vec!["yarn".into(), "install".into(), "--frozen-lockfile".into()],
        (Pip, Add | Install) if pkgs_some => prepend("python", &["-m", "pip", "install"], &pkgs),
        (Pip, Install) => vec![
            "python".into(),
            "-m".into(),
            "pip".into(),
            "install".into(),
            "-e".into(),
            ".".into(),
        ],
        (Pip, Remove) if pkgs_some => prepend("python", &["-m", "pip", "uninstall", "-y"], &pkgs),
        (Pip, Update) => {
            prepend_or_default("python", &["-m", "pip", "install", "--upgrade"], &pkgs)
        }
        (Pip, Refresh) => vec![
            "python".into(),
            "-m".into(),
            "pip".into(),
            "install".into(),
            "-r".into(),
            "requirements.txt".into(),
        ],
        (Uv, Add) if pkgs_some => prepend("uv", &["add"], &pkgs),
        (Uv, Install | Refresh) => vec!["uv".into(), "sync".into()],
        (Uv, Remove) if pkgs_some => prepend("uv", &["remove"], &pkgs),
        (Uv, Update) => prepend_or_default("uv", &["sync", "--upgrade"], &pkgs),
        (Poetry, Add) if pkgs_some => {
            let mut argv = vec!["poetry".into(), "add".into()];
            if dev {
                argv.push("--group".into());
                argv.push("dev".into());
            }
            argv.extend(pkgs);
            argv
        }
        (Poetry, Install | Refresh) => vec!["poetry".into(), "install".into()],
        (Poetry, Remove) if pkgs_some => prepend("poetry", &["remove"], &pkgs),
        (Poetry, Update) => prepend_or_default("poetry", &["update"], &pkgs),
        (Go, Add) if pkgs_some => prepend("go", &["get"], &pkgs),
        (Go, Install | Refresh) => vec!["go".into(), "mod".into(), "download".into()],
        (Go, Update) => prepend_or_default("go", &["get", "-u"], &pkgs),
        (Go, Remove) if pkgs_some => prepend("go", &["mod", "tidy"], &[]),
        (Swift, Install | Add | Refresh) => {
            vec!["swift".into(), "package".into(), "resolve".into()]
        }
        (Swift, Update) => vec!["swift".into(), "package".into(), "update".into()],
        (Bundler, Install | Refresh) => vec!["bundle".into(), "install".into()],
        (Bundler, Add) if pkgs_some => prepend("bundle", &["add"], &pkgs),
        (Bundler, Remove) if pkgs_some => prepend("bundle", &["remove"], &pkgs),
        (Bundler, Update) => prepend_or_default("bundle", &["update"], &pkgs),
        (Composer, Install | Refresh) => vec!["composer".into(), "install".into()],
        (Composer, Add) if pkgs_some => prepend("composer", &["require"], &pkgs),
        (Composer, Remove) if pkgs_some => prepend("composer", &["remove"], &pkgs),
        (Composer, Update) => prepend_or_default("composer", &["update"], &pkgs),
        (Gradle, Install | Refresh) => vec![
            "./gradlew".into(),
            "build".into(),
            "--refresh-dependencies".into(),
        ],
        (Maven, Install | Refresh) => vec!["mvn".into(), "install".into()],
        (Dotnet, Install | Refresh) => vec!["dotnet".into(), "restore".into()],
        (Dotnet, Add) if pkgs_some => prepend("dotnet", &["add", "package"], &pkgs),
        (Dotnet, Remove) if pkgs_some => prepend("dotnet", &["remove", "package"], &pkgs),
        _ => return None,
    })
}

fn prepend(program: &str, flags: &[&str], packages: &[String]) -> Vec<String> {
    let mut argv = vec![program.to_string()];
    argv.extend(flags.iter().map(|s| s.to_string()));
    argv.extend(packages.iter().cloned());
    argv
}

fn prepend_or_default(program: &str, flags: &[&str], packages: &[String]) -> Vec<String> {
    if packages.is_empty() {
        let mut argv = vec![program.to_string()];
        argv.extend(flags.iter().map(|s| s.to_string()));
        argv
    } else {
        prepend(program, flags, packages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_install_dev_emits_save_dev_flag() {
        let argv = build_argv(
            Ecosystem::Npm,
            Operation::Install,
            &["lodash".to_string()],
            true,
        )
        .unwrap();
        assert_eq!(argv, vec!["npm", "install", "--save-dev", "lodash"]);
    }

    #[test]
    fn cargo_refresh_uses_fetch() {
        let argv = build_argv(Ecosystem::Cargo, Operation::Refresh, &[], false).unwrap();
        assert_eq!(argv, vec!["cargo", "fetch"]);
    }

    #[test]
    fn poetry_add_dev_uses_group_dev() {
        let argv = build_argv(
            Ecosystem::Poetry,
            Operation::Add,
            &["pytest".to_string()],
            true,
        )
        .unwrap();
        assert_eq!(argv, vec!["poetry", "add", "--group", "dev", "pytest"]);
    }

    #[test]
    fn unsupported_pair_returns_none() {
        // gradle has no defined `add` mapping today
        assert!(build_argv(
            Ecosystem::Gradle,
            Operation::Add,
            &["junit".to_string()],
            false
        )
        .is_none());
    }
}
