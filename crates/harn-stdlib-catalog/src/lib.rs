//! Registration point for the embedded Harn standard library.
//!
//! The compiler and runtime read stdlib sources through this crate instead of
//! depending on `harn-stdlib`, which embeds every `.harn` source file. A crate
//! that embeds the sources changes whenever one of them does, and so does
//! everything compiled against it; keeping the catalog behind this small,
//! dependency-free crate means editing a stdlib module rebuilds only
//! `harn-stdlib` and the binaries that link it.
//!
//! A host links `harn-stdlib` and calls `harn_stdlib::install()` once at
//! startup. Reading the catalog before that is a typed
//! [`CatalogNotInstalled`] error, never an empty standard library.

use std::sync::{LazyLock, OnceLock};

/// One embedded stdlib module, imported as `std/<module>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StdlibSource {
    pub module: &'static str,
    pub source: &'static str,
}

/// One embedded prompt template, addressed by its path under `std/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StdlibPromptAsset {
    pub path: &'static str,
    pub source: &'static str,
}

/// Embedded `.harn` script that backs a CLI subcommand. Looked up by
/// the `harn-cli` dispatch wedge (see harn#2293 epic and harn#2294 G1)
/// so subcommands can ship in Harn instead of Rust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StdlibCliScript {
    /// Lookup name. For nested scripts this is the path under
    /// `stdlib/cli/` without the `.harn` extension (e.g. `"eval/prompt"`
    /// for `stdlib/cli/eval/prompt.harn`).
    pub name: &'static str,
    /// Embedded source. Run via the existing `harn run` codepath by the
    /// dispatch wedge.
    pub source: &'static str,
}

/// Every embedded stdlib source a host provides.
#[derive(Debug)]
pub struct StdlibCatalog {
    pub sources: &'static [StdlibSource],
    pub prompt_assets: &'static [StdlibPromptAsset],
    pub cli_scripts: &'static [StdlibCliScript],
}

impl StdlibCatalog {
    /// The source of `std/<module>`, or `None` when no such module exists.
    pub fn source(&self, module: &str) -> Option<&'static str> {
        self.sources
            .iter()
            .find_map(|entry| (entry.module == module).then_some(entry.source))
    }

    /// The prompt template at `path`, with or without its `std/` prefix.
    pub fn prompt_asset(&self, path: &str) -> Option<&'static str> {
        let path = path.strip_prefix("std/").unwrap_or(path);
        self.prompt_assets
            .iter()
            .find_map(|entry| (entry.path == path).then_some(entry.source))
    }

    /// The CLI subcommand script registered under `name`.
    pub fn cli_script(&self, name: &str) -> Option<&'static str> {
        self.cli_scripts
            .iter()
            .find_map(|entry| (entry.name == name).then_some(entry.source))
    }
}

/// A catalog built on its first read, so a host that installs one at startup
/// pays for it only once Harn code needs it.
pub type LazyCatalog = LazyLock<StdlibCatalog>;

static INSTALLED: OnceLock<&'static LazyCatalog> = OnceLock::new();

/// No stdlib catalog was installed before Harn code needed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogNotInstalled;

impl std::fmt::Display for CatalogNotInstalled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "no Harn standard library is installed: the host must link harn-stdlib and call \
             harn_stdlib::install() before running Harn code",
        )
    }
}

impl std::error::Error for CatalogNotInstalled {}

/// A different catalog is already installed in this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogAlreadyInstalled;

impl std::fmt::Display for CatalogAlreadyInstalled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a different Harn standard library is already installed")
    }
}

impl std::error::Error for CatalogAlreadyInstalled {}

/// Install the process's stdlib catalog. Installing the same catalog again
/// is a no-op; a second, different catalog is refused.
pub fn install(catalog: &'static LazyCatalog) -> Result<(), CatalogAlreadyInstalled> {
    let installed = INSTALLED.get_or_init(|| catalog);
    if std::ptr::eq(*installed, catalog) {
        Ok(())
    } else {
        Err(CatalogAlreadyInstalled)
    }
}

/// Whether a catalog is installed, without building it. Startup checks use
/// this so a command that never runs Harn code never pays for the catalog.
pub fn ensure_installed() -> Result<(), CatalogNotInstalled> {
    INSTALLED.get().map(|_| ()).ok_or(CatalogNotInstalled)
}

/// The installed catalog.
pub fn installed() -> Result<&'static StdlibCatalog, CatalogNotInstalled> {
    INSTALLED
        .get()
        .map(|catalog| &***catalog)
        .ok_or(CatalogNotInstalled)
}

/// Builtins a stdlib module re-exports under its own name, so
/// `import { assert_eq } from "std/testing"` resolves.
///
/// Some of the standard library is implemented in Rust rather than Harn — the
/// value differ behind `assert_eq` needs to walk the runtime representation of
/// a value, which Harn source cannot do. Those builtins are callable without an
/// import, but a reader who has just typed `import { assert_throws } from
/// "std/testing"` has every reason to expect its sibling `assert_eq` to come
/// from the same place, and no reason to know which side of the Rust/Harn line
/// a given assertion happens to fall on. This table erases that seam: the
/// module's export surface is its `pub fn`s plus the names listed here.
///
/// The list is explicit rather than derived from the builtins' `category`
/// metadata: a category is a free-form label for docs and observability, and an
/// export surface is an API contract. Deriving one from the other would let an
/// unrelated metadata edit silently add or remove a public export.
///
/// `harn_vm::stdlib::tests` pins every name here to a registered builtin, so an
/// entry cannot rot into a name that no longer exists.
pub fn builtin_reexports(module: &str) -> &'static [&'static str] {
    match module {
        "testing" => &[
            "assert",
            "assert_approx",
            "assert_eq",
            "assert_matches",
            "assert_ne",
            "skip",
            "value_diff",
        ],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static ONE: LazyCatalog = LazyCatalog::new(|| StdlibCatalog {
        sources: &[StdlibSource {
            module: "text",
            source: "pub fn shout() {}",
        }],
        prompt_assets: &[StdlibPromptAsset {
            path: "agent/prompts/x.harn.prompt",
            source: "hello",
        }],
        cli_scripts: &[],
    });
    static OTHER: LazyCatalog = LazyCatalog::new(|| StdlibCatalog {
        sources: &[],
        prompt_assets: &[],
        cli_scripts: &[],
    });

    #[test]
    fn lookups_answer_by_module_and_by_prefixed_path() {
        assert_eq!(ONE.source("text"), Some("pub fn shout() {}"));
        assert_eq!(ONE.source("missing"), None);
        assert_eq!(
            ONE.prompt_asset("std/agent/prompts/x.harn.prompt"),
            Some("hello")
        );
    }

    static BUILDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static COUNTED: LazyCatalog = LazyCatalog::new(|| {
        BUILDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        StdlibCatalog {
            sources: &[],
            prompt_assets: &[],
            cli_scripts: &[],
        }
    });

    #[test]
    fn a_reader_before_install_gets_a_typed_error_then_the_one_catalog() {
        assert_eq!(installed().err(), Some(CatalogNotInstalled));
        assert_eq!(ensure_installed(), Err(CatalogNotInstalled));
        install(&COUNTED).expect("first install");
        install(&COUNTED).expect("same catalog again");
        assert_eq!(install(&OTHER), Err(CatalogAlreadyInstalled));
        // Startup checks must not decode the catalog: `harn --version` does one.
        ensure_installed().expect("installed");
        assert_eq!(BUILDS.load(std::sync::atomic::Ordering::SeqCst), 0);
        let counted: &StdlibCatalog = &COUNTED;
        assert!(std::ptr::eq(installed().expect("installed"), counted));
        assert_eq!(BUILDS.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
