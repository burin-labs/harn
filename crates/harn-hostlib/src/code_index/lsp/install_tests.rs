use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::super::servers;
use super::Probe;
use crate::ast::Language;

/// A home directory and environment that are not the test process's own.
struct FakeProbe {
    home: PathBuf,
    vars: HashMap<&'static str, OsString>,
}

impl FakeProbe {
    fn new(home: &Path) -> Self {
        // A Finder-launched app's PATH: none of the per-user directories.
        let vars = HashMap::from([("PATH", OsString::from("/usr/bin:/bin"))]);
        Self {
            home: home.to_path_buf(),
            vars,
        }
    }
}

impl Probe for FakeProbe {
    fn var(&self, name: &str) -> Option<OsString> {
        self.vars.get(name).cloned()
    }

    fn home(&self) -> Option<PathBuf> {
        Some(self.home.clone())
    }

    fn rust_homes(&self) -> (Option<PathBuf>, Option<PathBuf>) {
        (
            Some(self.home.join(".cargo")),
            Some(self.home.join(".rustup")),
        )
    }

    fn system_dirs(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}

fn executable(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "").unwrap();
}

fn rust_launch() -> &'static servers::Launch {
    &servers::for_language(Language::Rust).unwrap().launches[0]
}

/// rustup's layout: proxies in `~/.cargo/bin`, toolchains under `~/.rustup`.
#[cfg(unix)]
fn rustup_home(home: &Path, toolchain_has_server: bool) {
    let bin = home.join(".cargo/bin");
    executable(&bin.join("rustup"));
    std::os::unix::fs::symlink("rustup", bin.join("rust-analyzer")).unwrap();
    std::fs::create_dir_all(home.join(".rustup")).unwrap();
    std::fs::write(
        home.join(".rustup/settings.toml"),
        "default_toolchain = \"1.95.0-aarch64-apple-darwin\"\n",
    )
    .unwrap();
    let toolchain_bin = home.join(".rustup/toolchains/1.95.0-aarch64-apple-darwin/bin");
    executable(&toolchain_bin.join("cargo"));
    if toolchain_has_server {
        executable(&toolchain_bin.join("rust-analyzer"));
    }
}

#[cfg(unix)]
#[test]
fn a_rustup_server_resolves_without_cargo_bin_on_path() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    rustup_home(home.path(), true);
    let program = rust_launch()
        .resolve_with(&FakeProbe::new(home.path()), workspace.path())
        .expect("rust-analyzer resolves from the home directory");
    assert_eq!(
        PathBuf::from(program),
        home.path().join(".cargo/bin/rust-analyzer")
    );
}

#[cfg(unix)]
#[test]
fn a_rustup_proxy_without_the_component_does_not_resolve() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    rustup_home(home.path(), false);
    let reason = rust_launch()
        .resolve_with(&FakeProbe::new(home.path()), workspace.path())
        .expect_err("the proxy would exit: its toolchain has no rust-analyzer");
    assert!(
        reason.contains("rustup component add rust-analyzer"),
        "{reason}"
    );

    // A workspace that pins a toolchain with the component resolves.
    std::fs::write(
        workspace.path().join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"nightly\"\n",
    )
    .unwrap();
    executable(
        &home
            .path()
            .join(".rustup/toolchains/nightly-aarch64-apple-darwin/bin/rust-analyzer"),
    );
    rust_launch()
        .resolve_with(&FakeProbe::new(home.path()), workspace.path())
        .expect("the pinned toolchain has rust-analyzer");
}

#[cfg(unix)]
#[test]
fn a_rustup_directory_override_decides_the_proxy() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    rustup_home(home.path(), true);
    executable(
        &home
            .path()
            .join(".rustup/toolchains/beta-aarch64-apple-darwin/bin/cargo"),
    );
    // `rustup override set beta` in the workspace: beta lacks the component.
    std::fs::write(
        home.path().join(".rustup/settings.toml"),
        format!(
            "default_toolchain = \"1.95.0-aarch64-apple-darwin\"\n[overrides]\n{:?} = \"beta\"\n",
            workspace.path().display().to_string()
        ),
    )
    .unwrap();
    let reason = rust_launch()
        .resolve_with(&FakeProbe::new(home.path()), workspace.path())
        .expect_err("the override selects beta, which has no rust-analyzer");
    assert!(reason.contains("beta toolchain"), "{reason}");

    // With no override and no default, rustup selects nothing to run.
    std::fs::write(home.path().join(".rustup/settings.toml"), "").unwrap();
    let reason = rust_launch()
        .resolve_with(&FakeProbe::new(home.path()), workspace.path())
        .expect_err("a proxy with no selected toolchain cannot start");
    assert!(
        reason.contains("selects no installed toolchain"),
        "{reason}"
    );
}

#[test]
fn an_override_naming_no_file_does_not_resolve() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    executable(&home.path().join(".cargo/bin/rust-analyzer"));
    let mut probe = FakeProbe::new(home.path());
    probe.vars.insert(
        "HARN_LSP_SERVER_RUST_ANALYZER",
        OsString::from("/nonexistent/rust-analyzer"),
    );
    let reason = rust_launch()
        .resolve_with(&probe, workspace.path())
        .expect_err("a set override wins over discovery, even when it names nothing");
    assert!(reason.contains("HARN_LSP_SERVER_RUST_ANALYZER"), "{reason}");
}

#[test]
fn an_empty_process_override_does_not_fall_back_to_a_project_server() {
    const CHILD: &str = "HARN_TEST_EMPTY_LSP_OVERRIDE_CHILD";
    const OVERRIDE: &str = "HARN_LSP_SERVER_TYPESCRIPT_LANGUAGE_SERVER";
    if std::env::var_os(CHILD).is_some() {
        let workspace = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) {
            "typescript-language-server.cmd"
        } else {
            "typescript-language-server"
        };
        executable(&workspace.path().join("node_modules/.bin").join(name));
        let launch = &servers::for_language(Language::TypeScript)
            .unwrap()
            .launches[0];
        let result = launch.resolve_with(&super::ProcessProbe, workspace.path());
        assert!(
            result.is_err(),
            "an empty set override must win over the project server: {result:?}"
        );
        return;
    }
    // Process-global environment is isolated in a child, so parallel tests do
    // not race on PATH or the server override.
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "code_index::lsp::install::tests::an_empty_process_override_does_not_fall_back_to_a_project_server", "--nocapture"])
        .env(CHILD, "1")
        .env(OVERRIDE, "")
        .env("PATH", "")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&child.stdout);
    let stderr = String::from_utf8_lossy(&child.stderr);
    assert!(
        child.status.success(),
        "isolated override test failed:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("1 passed; 0 failed"),
        "the isolated test must actually run: {stdout}"
    );
}

#[cfg(unix)]
#[test]
fn a_tsc_without_version_metadata_does_not_resolve() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let name = if cfg!(windows) { "tsc.cmd" } else { "tsc" };
    executable(&workspace.path().join("node_modules/.bin").join(name));
    let mut probe = FakeProbe::new(home.path());
    probe.vars.insert("PATH", OsString::new());
    let tsc = &servers::for_language(Language::TypeScript)
        .unwrap()
        .launches[1];
    let reason = tsc
        .resolve_with(&probe, workspace.path())
        .expect_err("an unversioned tsc does not prove TypeScript 7 LSP support");
    assert!(
        reason.contains("cannot determine the TypeScript version"),
        "{reason}"
    );
}

#[cfg(unix)]
#[test]
fn a_tsc_older_than_typescript_7_does_not_resolve() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let package = workspace.path().join("node_modules/typescript");
    executable(&package.join("bin/tsc"));
    std::fs::write(
        package.join("package.json"),
        r#"{"name": "typescript", "version": "5.9.3"}"#,
    )
    .unwrap();
    std::fs::create_dir_all(workspace.path().join("node_modules/.bin")).unwrap();
    std::os::unix::fs::symlink(
        "../typescript/bin/tsc",
        workspace.path().join("node_modules/.bin/tsc"),
    )
    .unwrap();
    let tsc = &servers::for_language(Language::TypeScript)
        .unwrap()
        .launches[1];
    let reason = tsc
        .resolve_with(&FakeProbe::new(home.path()), workspace.path())
        .expect_err("TypeScript 5 has no `tsc --lsp`");
    assert!(reason.contains("TypeScript 5"), "{reason}");

    std::fs::write(
        package.join("package.json"),
        r#"{"name": "typescript", "version": "7.0.1"}"#,
    )
    .unwrap();
    tsc.resolve_with(&FakeProbe::new(home.path()), workspace.path())
        .expect("TypeScript 7 serves LSP from tsc");
}
