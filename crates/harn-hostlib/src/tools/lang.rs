//! Workspace ecosystem detection.
//!
//! `run_test`, `run_build_command`, and `manage_packages` need to map a
//! `cwd` to a runner (`cargo`, `npm`, `pytest`, `go`, `swift`, …) when the
//! caller doesn't pass an explicit argv / ecosystem.
//!
//! Detection is intentionally manifest-only — we never read user shell
//! profiles or scan outside `cwd`. The caller can always force a specific
//! runner by passing `argv` (for `run_*`) or `ecosystem` (for
//! `manage_packages`), bypassing detection entirely.
//!
//! Divergence vs. Swift `BurinCore`: the Swift implementation also
//! consulted `LanguageConfigRegistry` JSON files shipped inside the IDE
//! bundle. We do *not* port that here — the IDE shim can keep doing its
//! own override before invoking `run_test`. The behaviors here are the
//! sane defaults for the manifests every supported ecosystem ships.

use std::path::Path;

/// One of the package ecosystems hostlib knows how to drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ecosystem {
    Cargo,
    Npm,
    Pnpm,
    Yarn,
    Pip,
    Uv,
    Poetry,
    Go,
    Swift,
    Gradle,
    Maven,
    Bundler,
    Composer,
    Dotnet,
}

impl Ecosystem {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Ecosystem::Cargo => "cargo",
            Ecosystem::Npm => "npm",
            Ecosystem::Pnpm => "pnpm",
            Ecosystem::Yarn => "yarn",
            Ecosystem::Pip => "pip",
            Ecosystem::Uv => "uv",
            Ecosystem::Poetry => "poetry",
            Ecosystem::Go => "go",
            Ecosystem::Swift => "swift",
            Ecosystem::Gradle => "gradle",
            Ecosystem::Maven => "maven",
            Ecosystem::Bundler => "bundler",
            Ecosystem::Composer => "composer",
            Ecosystem::Dotnet => "dotnet",
        }
    }

    /// Map the user-facing ecosystem name (from the request payload) to a
    /// detected variant. Aliases follow Swift's `packageManager(from:)`.
    pub(crate) fn parse(name: &str) -> Option<Ecosystem> {
        match name.trim().to_ascii_lowercase().as_str() {
            "cargo" | "rust" => Some(Ecosystem::Cargo),
            "npm" => Some(Ecosystem::Npm),
            "pnpm" => Some(Ecosystem::Pnpm),
            "yarn" => Some(Ecosystem::Yarn),
            "pip" | "python" => Some(Ecosystem::Pip),
            "uv" => Some(Ecosystem::Uv),
            "poetry" => Some(Ecosystem::Poetry),
            "go" | "golang" => Some(Ecosystem::Go),
            "swift" | "swiftpm" => Some(Ecosystem::Swift),
            "gradle" => Some(Ecosystem::Gradle),
            "maven" | "mvn" => Some(Ecosystem::Maven),
            "bundle" | "bundler" | "ruby" => Some(Ecosystem::Bundler),
            "composer" | "php" => Some(Ecosystem::Composer),
            "dotnet" | ".net" | "csharp" => Some(Ecosystem::Dotnet),
            _ => None,
        }
    }
}

/// Detect the most likely ecosystem for a workspace by inspecting manifests.
///
/// Returns `None` if no recognized manifest is present. Detection order
/// matches the strictest-first heuristic Swift used: lockfile-bearing JS
/// managers beat plain `npm`; `uv.lock` / `poetry.lock` beat plain `pip`.
pub(crate) fn detect(cwd: &Path) -> Option<Ecosystem> {
    if cwd.join("Cargo.toml").is_file() {
        return Some(Ecosystem::Cargo);
    }
    if cwd.join("Package.swift").is_file() {
        return Some(Ecosystem::Swift);
    }
    if cwd.join("go.mod").is_file() {
        return Some(Ecosystem::Go);
    }
    if cwd.join("pnpm-lock.yaml").is_file() {
        return Some(Ecosystem::Pnpm);
    }
    if cwd.join("yarn.lock").is_file() {
        return Some(Ecosystem::Yarn);
    }
    if cwd.join("package.json").is_file() {
        return Some(Ecosystem::Npm);
    }
    if cwd.join("uv.lock").is_file() {
        return Some(Ecosystem::Uv);
    }
    if cwd.join("poetry.lock").is_file() {
        return Some(Ecosystem::Poetry);
    }
    if cwd.join("pyproject.toml").is_file() || cwd.join("setup.py").is_file() {
        return Some(Ecosystem::Pip);
    }
    if cwd.join("Gemfile").is_file() {
        return Some(Ecosystem::Bundler);
    }
    if cwd.join("composer.json").is_file() {
        return Some(Ecosystem::Composer);
    }
    if cwd.join("build.gradle").is_file() || cwd.join("build.gradle.kts").is_file() {
        return Some(Ecosystem::Gradle);
    }
    if cwd.join("pom.xml").is_file() {
        return Some(Ecosystem::Maven);
    }
    if has_dotnet_project(cwd) {
        return Some(Ecosystem::Dotnet);
    }
    None
}

fn has_dotnet_project(cwd: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(cwd) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .file_name()
            .to_str()
            .map(|name| {
                name.ends_with(".csproj") || name.ends_with(".sln") || name.ends_with(".slnx")
            })
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"").unwrap();
    }

    #[test]
    fn detects_cargo_workspace() {
        let dir = tempdir().unwrap();
        touch(dir.path(), "Cargo.toml");
        assert_eq!(detect(dir.path()), Some(Ecosystem::Cargo));
    }

    #[test]
    fn pnpm_lockfile_beats_package_json() {
        let dir = tempdir().unwrap();
        touch(dir.path(), "package.json");
        touch(dir.path(), "pnpm-lock.yaml");
        assert_eq!(detect(dir.path()), Some(Ecosystem::Pnpm));
    }

    #[test]
    fn returns_none_for_empty_directory() {
        let dir = tempdir().unwrap();
        assert!(detect(dir.path()).is_none());
    }

    #[test]
    fn parses_ecosystem_aliases() {
        assert_eq!(Ecosystem::parse("rust"), Some(Ecosystem::Cargo));
        assert_eq!(Ecosystem::parse("Python"), Some(Ecosystem::Pip));
        assert_eq!(Ecosystem::parse("swiftpm"), Some(Ecosystem::Swift));
        assert_eq!(Ecosystem::parse("nope"), None);
    }
}
