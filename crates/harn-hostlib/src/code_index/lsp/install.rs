//! Where a language server is installed when `PATH` does not name it.
//!
//! A GUI app opened from Finder, a launchd job, and a non-login shell start
//! with a `PATH` that omits the per-user directories rustup, npm, Go and
//! Homebrew install into, so a server the user's terminal runs looked
//! uninstalled. These directories are searched after `PATH`, resolved from the
//! user's home and the toolchain variables rather than from `PATH`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// What discovery reads from the process, so tests can supply their own.
pub(crate) trait Probe {
    fn var(&self, name: &str) -> Option<OsString>;
    /// A set but empty server override is distinct from an absent override.
    fn raw_var(&self, name: &str) -> Option<OsString> {
        self.var(name)
    }
    fn home(&self) -> Option<PathBuf>;
    /// `CARGO_HOME` and `RUSTUP_HOME` by the sandbox's own rule.
    fn rust_homes(&self) -> (Option<PathBuf>, Option<PathBuf>);
    /// Machine-wide package-manager directories, searched last.
    fn system_dirs(&self) -> Vec<PathBuf>;
}

/// The running process.
pub(crate) struct ProcessProbe;

impl Probe for ProcessProbe {
    fn var(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name).filter(|value| !value.is_empty())
    }

    fn raw_var(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name)
    }

    fn home(&self) -> Option<PathBuf> {
        harn_vm::user_dirs::home_dir().filter(|home| home.is_absolute())
    }

    fn rust_homes(&self) -> (Option<PathBuf>, Option<PathBuf>) {
        let homes = harn_vm::process_sandbox::rust_toolchain_homes();
        let find = |name: &str| {
            homes
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, path)| path.clone())
        };
        (find("CARGO_HOME"), find("RUSTUP_HOME"))
    }

    fn system_dirs(&self) -> Vec<PathBuf> {
        if !cfg!(unix) {
            return Vec::new();
        }
        // Homebrew on Apple silicon, Homebrew on Intel and most Linux
        // installers, then Linuxbrew.
        [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/home/linuxbrew/.linuxbrew/bin",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect()
    }
}

/// Install directories searched after `PATH`, most specific first. `root` is
/// the server's workspace root, which can pin a Rust toolchain.
pub(crate) fn install_dirs(probe: &dyn Probe, root: &Path) -> Vec<PathBuf> {
    let home = probe.home();
    let in_home = |rel: &str| home.as_ref().map(|home| home.join(rel));
    let var_dir = |name: &str| {
        probe
            .var(name)
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
    };
    // npm installs global shims in the prefix itself on Windows, in `bin` elsewhere.
    let npm_bin = |prefix: PathBuf| {
        if cfg!(windows) {
            prefix
        } else {
            prefix.join("bin")
        }
    };
    let (cargo_home, _) = probe.rust_homes();
    let mut dirs: Vec<Option<PathBuf>> = vec![
        cargo_home.map(|cargo| cargo.join("bin")),
        rustup_toolchain(probe, root).map(|toolchain| toolchain.bin),
        var_dir("GOBIN"),
        probe
            .var("GOPATH")
            .and_then(|gopath| std::env::split_paths(&gopath).next())
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join("bin")),
        in_home("go/bin"),
        var_dir("NPM_CONFIG_PREFIX").map(npm_bin),
        var_dir("npm_config_prefix").map(npm_bin),
        in_home(".npm-global").map(npm_bin),
        var_dir("PNPM_HOME"),
        in_home(".volta/bin"),
        in_home(".bun/bin"),
        // pipx and `uv tool install`.
        in_home(".local/bin"),
    ];
    if cfg!(windows) {
        // npm's default global prefix, then Scoop.
        dirs.push(var_dir("APPDATA").map(|appdata| appdata.join("npm")));
        dirs.push(in_home("scoop/shims"));
    }
    dirs.extend(probe.system_dirs().into_iter().map(Some));
    let mut unique: Vec<PathBuf> = Vec::new();
    for dir in dirs.into_iter().flatten() {
        if !unique.contains(&dir) {
            unique.push(dir);
        }
    }
    unique
}

/// The toolchain rustup would run in `root`.
pub(crate) struct RustupToolchain {
    pub name: String,
    pub bin: PathBuf,
}

/// The installed toolchain rustup selects for `root`, in rustup's order:
/// `RUSTUP_TOOLCHAIN`; then, from `root` upward, a `rustup override set`
/// directory override or a `rust-toolchain(.toml)` file; then the default.
/// `None` when rustup is absent, selects nothing, or the selection is not
/// installed.
pub(crate) fn rustup_toolchain(probe: &dyn Probe, root: &Path) -> Option<RustupToolchain> {
    let (_, rustup_home) = probe.rust_homes();
    let rustup_home = rustup_home?;
    let settings = rustup_settings(&rustup_home);
    let name = probe
        .var("RUSTUP_TOOLCHAIN")
        .map(|name| name.to_string_lossy().into_owned())
        .or_else(|| {
            root.ancestors().find_map(|dir| {
                directory_override(settings.as_ref(), dir).or_else(|| pinned_toolchain(dir))
            })
        })
        .or_else(|| {
            settings
                .as_ref()?
                .get("default_toolchain")?
                .as_str()
                .map(str::to_string)
        })?;
    let toolchains = rustup_home.join("toolchains");
    // `stable` and `1.95.0` name the host's `<name>-<target-triple>` directory.
    let dir = if toolchains.join(&name).is_dir() {
        toolchains.join(&name)
    } else {
        let prefix = format!("{name}-");
        let mut matches: Vec<PathBuf> = std::fs::read_dir(&toolchains)
            .ok()?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
            .map(|entry| entry.path())
            .collect();
        matches.sort();
        matches.into_iter().next()?
    };
    Some(RustupToolchain {
        name,
        bin: dir.join("bin"),
    })
}

fn rustup_settings(rustup_home: &Path) -> Option<toml::Table> {
    std::fs::read_to_string(rustup_home.join("settings.toml"))
        .ok()?
        .parse()
        .ok()
}

/// The toolchain `rustup override set` recorded for exactly `dir`.
fn directory_override(settings: Option<&toml::Table>, dir: &Path) -> Option<String> {
    let overrides = settings?.get("overrides")?.as_table()?;
    let canonical = std::fs::canonicalize(dir).ok();
    [Some(dir.to_path_buf()), canonical]
        .into_iter()
        .flatten()
        .find_map(|dir| overrides.get(dir.to_string_lossy().as_ref())?.as_str())
        .map(str::to_string)
}

/// The channel a `rust-toolchain(.toml)` file in `dir` pins.
fn pinned_toolchain(dir: &Path) -> Option<String> {
    let toml = std::fs::read_to_string(dir.join("rust-toolchain.toml")).ok();
    let legacy = || std::fs::read_to_string(dir.join("rust-toolchain")).ok();
    let text = toml.or_else(legacy)?;
    match text.parse::<toml::Table>() {
        Ok(table) => table
            .get("toolchain")
            .and_then(|toolchain| toolchain.get("channel"))
            .and_then(toml::Value::as_str)
            .map(str::to_string),
        // The legacy file may hold a bare channel name.
        Err(_) => Some(text.trim().to_string()).filter(|name| !name.is_empty()),
    }
}

/// Whether `candidate` is a rustup proxy: a link to, or a copy of the inode of,
/// the `rustup` binary beside it.
pub(crate) fn is_rustup_proxy(candidate: &Path) -> bool {
    let named_rustup = |path: &Path| {
        path.file_stem()
            .is_some_and(|stem| stem.to_string_lossy().starts_with("rustup"))
    };
    if std::fs::canonicalize(candidate).is_ok_and(|target| named_rustup(&target)) {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let sibling = candidate.with_file_name("rustup");
        if let (Ok(a), Ok(b)) = (std::fs::metadata(candidate), std::fs::metadata(&sibling)) {
            return a.dev() == b.dev() && a.ino() == b.ino();
        }
    }
    false
}

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
