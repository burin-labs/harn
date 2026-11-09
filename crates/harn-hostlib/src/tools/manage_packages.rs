//! `tools/manage_packages` — install / add / remove / update / refresh deps
//! across the package ecosystems hostlib can drive.
//!
//! Schema: `schemas/tools/manage_packages.{request,response}.json`.
//!
//! Behavior:
//! - `ecosystem` is required when `cwd` doesn't contain a recognized
//!   manifest; callers must opt in to a specific manager.
//! - `lockfile_changed` is computed by snapshotting the relevant lockfile
//!   path before/after the spawn and comparing mtimes. We don't read
//!   contents to keep the cost predictable on large lockfiles.
//! - Approval/UX is the embedder's responsibility — by the time this
//!   builtin is called, the host has already obtained user consent.

use std::path::Path;
use std::time::SystemTime;

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::lang::{detect, Ecosystem};
use crate::tools::payload::{
    optional_bool, optional_string, optional_string_list, parse_argv_program, require_dict_arg,
    require_string,
};
use crate::tools::proc;
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

    let (cwd_path, cwd_for_detect) = proc::parse_project_cwd(NAME, &map)?;

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

    let (program, args_tail) = parse_argv_program(NAME, argv)?;

    let outcome = proc::run_project_command(NAME, program, args_tail, cwd_path, None)?;

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
        (Cargo, Add) if pkgs_some => add("cargo", &["add"], &["--dev"], dev, &pkgs),
        (Cargo, Remove) if pkgs_some => prepend("cargo", &["remove"], &pkgs),
        (Cargo, Install | Refresh) => vec!["cargo".into(), "fetch".into()],
        (Cargo, Update) => each_package("cargo", &["update"], "-p", &pkgs),
        (Npm, Add | Install) if pkgs_some => add("npm", &["install"], &["--save-dev"], dev, &pkgs),
        (Npm, Install) => vec!["npm".into(), "install".into()],
        (Npm, Remove) if pkgs_some => prepend("npm", &["uninstall"], &pkgs),
        (Npm, Update) => prepend("npm", &["update"], &pkgs),
        (Npm, Refresh) => vec!["npm".into(), "ci".into()],
        (Pnpm, Add | Install) if pkgs_some => add("pnpm", &["add"], &["--save-dev"], dev, &pkgs),
        (Pnpm, Install) => vec!["pnpm".into(), "install".into()],
        (Pnpm, Remove) if pkgs_some => prepend("pnpm", &["remove"], &pkgs),
        (Pnpm, Update) => prepend("pnpm", &["update"], &pkgs),
        (Pnpm, Refresh) => vec!["pnpm".into(), "install".into(), "--frozen-lockfile".into()],
        (Yarn, Add | Install) if pkgs_some => add("yarn", &["add"], &["--dev"], dev, &pkgs),
        (Yarn, Install) => vec!["yarn".into(), "install".into()],
        (Yarn, Remove) if pkgs_some => prepend("yarn", &["remove"], &pkgs),
        (Yarn, Update) => prepend("yarn", &["upgrade"], &pkgs),
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
        (Pip, Update) => prepend("python", &["-m", "pip", "install", "--upgrade"], &pkgs),
        (Pip, Refresh) => vec![
            "python".into(),
            "-m".into(),
            "pip".into(),
            "install".into(),
            "-r".into(),
            "requirements.txt".into(),
        ],
        (Uv, Add) if pkgs_some => add("uv", &["add"], &["--dev"], dev, &pkgs),
        (Uv, Install | Refresh) => vec!["uv".into(), "sync".into()],
        (Uv, Remove) if pkgs_some => prepend("uv", &["remove"], &pkgs),
        // `uv sync` takes no package positionals; it upgrades named ones by flag.
        (Uv, Update) if pkgs_some => each_package("uv", &["sync"], "--upgrade-package", &pkgs),
        (Uv, Update) => vec!["uv".into(), "sync".into(), "--upgrade".into()],
        (Poetry, Add) if pkgs_some => add("poetry", &["add"], &["--group", "dev"], dev, &pkgs),
        (Poetry, Install | Refresh) => vec!["poetry".into(), "install".into()],
        (Poetry, Remove) if pkgs_some => prepend("poetry", &["remove"], &pkgs),
        (Poetry, Update) => prepend("poetry", &["update"], &pkgs),
        (Go, Add) if pkgs_some => prepend("go", &["get"], &pkgs),
        (Go, Install | Refresh) => vec!["go".into(), "mod".into(), "download".into()],
        (Go, Update) => prepend("go", &["get", "-u"], &pkgs),
        // `go mod tidy` keeps a module the code still imports; `@none` removes it.
        (Go, Remove) if pkgs_some => {
            let pkgs: Vec<_> = pkgs.iter().map(|p| format!("{p}@none")).collect();
            prepend("go", &["get"], &pkgs)
        }
        (Swift, Install | Refresh) => {
            vec!["swift".into(), "package".into(), "resolve".into()]
        }
        (Swift, Update) => vec!["swift".into(), "package".into(), "update".into()],
        (Bundler, Install | Refresh) => vec!["bundle".into(), "install".into()],
        (Bundler, Add) if pkgs_some => {
            add("bundle", &["add"], &["--group", "development"], dev, &pkgs)
        }
        (Bundler, Remove) if pkgs_some => prepend("bundle", &["remove"], &pkgs),
        (Bundler, Update) => prepend("bundle", &["update"], &pkgs),
        (Composer, Install | Refresh) => vec!["composer".into(), "install".into()],
        (Composer, Add) if pkgs_some => add("composer", &["require"], &["--dev"], dev, &pkgs),
        (Composer, Remove) if pkgs_some => prepend("composer", &["remove"], &pkgs),
        (Composer, Update) => prepend("composer", &["update"], &pkgs),
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

/// `program flags [dev_flags] packages`, with `dev_flags` only for a dev dependency.
fn add(
    program: &str,
    flags: &[&str],
    dev_flags: &[&str],
    dev: bool,
    packages: &[String],
) -> Vec<String> {
    let flags: Vec<&str> = flags
        .iter()
        .chain(dev_flags.iter().filter(|_| dev))
        .copied()
        .collect();
    prepend(program, &flags, packages)
}

/// `program flags` followed by `flag package` for each package.
fn each_package(program: &str, flags: &[&str], flag: &str, packages: &[String]) -> Vec<String> {
    let mut argv = prepend(program, flags, &[]);
    for package in packages {
        argv.extend([flag.to_string(), package.clone()]);
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_operations_map_to_each_managers_own_spelling() {
        use Ecosystem::*;
        use Operation::*;
        let pkg = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        for (eco, op, packages, dev, expected) in [
            (
                Npm,
                Install,
                pkg(&["lodash"]),
                true,
                "npm install --save-dev lodash",
            ),
            (Cargo, Refresh, pkg(&[]), false, "cargo fetch"),
            (Cargo, Add, pkg(&["serde"]), true, "cargo add --dev serde"),
            (
                Cargo,
                Update,
                pkg(&["a", "b"]),
                false,
                "cargo update -p a -p b",
            ),
            (
                Poetry,
                Add,
                pkg(&["pytest"]),
                true,
                "poetry add --group dev pytest",
            ),
            (Uv, Add, pkg(&["pytest"]), true, "uv add --dev pytest"),
            (
                Uv,
                Update,
                pkg(&["httpx"]),
                false,
                "uv sync --upgrade-package httpx",
            ),
            (Uv, Update, pkg(&[]), false, "uv sync --upgrade"),
            (
                Go,
                Remove,
                pkg(&["example.com/m"]),
                false,
                "go get example.com/m@none",
            ),
            (
                Composer,
                Add,
                pkg(&["phpunit/phpunit"]),
                true,
                "composer require --dev phpunit/phpunit",
            ),
            (
                Bundler,
                Add,
                pkg(&["rspec"]),
                true,
                "bundle add --group development rspec",
            ),
        ] {
            let argv = build_argv(eco, op, &packages, dev).unwrap();
            assert_eq!(argv.join(" "), expected);
        }
        // No portable CLI adds these dependencies, so the tool refuses instead
        // of running an unrelated command that exits 0.
        assert!(build_argv(Gradle, Add, &pkg(&["junit"]), false).is_none());
        assert!(build_argv(Swift, Add, &pkg(&["swift-log"]), false).is_none());
    }
}
