//! Portable resolution of the current user's home directory.
//!
//! Unix exposes the home directory through `$HOME`. Windows usually leaves
//! `HOME` unset and exposes `%USERPROFILE%` instead. Historically a dozen
//! call sites across the workspace read `$HOME` directly and silently
//! degraded on Windows — falling back to a cwd-relative cache, a missing
//! config overlay, or an unresolved AWS profile — while a handful of others
//! hand-rolled their own `HOME`-then-`USERPROFILE` cascade. This module is
//! the single source of truth.
//!
//! The pure [`home_dir_from`] core takes the environment values explicitly so
//! it can be unit-tested for every platform's env shape (including Windows)
//! on any host. [`home_dir`] is the thin wrapper over the live process
//! environment that production code calls.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

fn non_empty(value: Option<&OsStr>) -> Option<PathBuf> {
    value.filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Resolve a home directory from explicit `$HOME` / `%USERPROFILE%` values.
///
/// Prefers `home` (the Unix convention), falling back to `userprofile` (the
/// Windows convention). Empty values are treated as unset. Pure and
/// platform-agnostic so the resolution order can be unit-tested for every OS.
pub fn home_dir_from(home: Option<&OsStr>, userprofile: Option<&OsStr>) -> Option<PathBuf> {
    non_empty(home).or_else(|| non_empty(userprofile))
}

/// The current user's home directory, resolved portably from the environment
/// (`$HOME`, then `%USERPROFILE%`). Returns `None` only when neither is set to
/// a non-empty value (e.g. a stripped-down container with no home).
pub fn home_dir() -> Option<PathBuf> {
    home_dir_from(
        std::env::var_os("HOME").as_deref(),
        std::env::var_os("USERPROFILE").as_deref(),
    )
}

/// Where Harn keeps its package cache for a given home directory.
///
/// The pure core takes `home` and `$XDG_CACHE_HOME` explicitly, for the same
/// reason [`home_dir_from`] does: the resolution has to be assertable for a
/// home that is not the running process's own. A sandboxed run relocates
/// `HOME` and `XDG_CACHE_HOME` into the workspace so each toolchain writes its
/// caches there, which silently moved Harn's package cache too and left every
/// such run resolving an empty cache. Resolving against an explicit home lets
/// the sandbox hand the child the real cache root instead.
pub fn package_cache_dir_from(home: &Path, xdg_cache_home: Option<&OsStr>) -> PathBuf {
    if cfg!(target_os = "macos") {
        return home.join("Library/Caches/harn");
    }
    match non_empty(xdg_cache_home) {
        Some(xdg) => xdg.join("harn"),
        None => home.join(".cache/harn"),
    }
}

/// [`package_cache_dir_from`] against the current process environment.
///
/// This is the default only. `HARN_CACHE_DIR` overrides it, and resolving that
/// override stays with the package layer that owns the setting.
pub fn package_cache_dir() -> Option<PathBuf> {
    home_dir()
        .map(|home| package_cache_dir_from(&home, std::env::var_os("XDG_CACHE_HOME").as_deref()))
}

/// Prefixes that stand in for the home directory, longest first so `"$HOME/"`
/// is tried before the bare `"$HOME"`.
const HOME_PREFIXES: &[&str] = &["~/", "$HOME/", "~", "$HOME"];

/// Expand a leading home-directory reference in `raw`.
///
/// Recognises `~`, `~/…`, `$HOME`, and `$HOME/…`. Anything else — including a
/// `~user` reference, which needs a passwd lookup this module deliberately
/// does not do — is returned unchanged, as is any input when no home directory
/// resolves. Only a *leading* reference is expanded; this is not a shell.
///
/// The pure core takes `home` explicitly so every prefix and both platforms'
/// env shapes are unit-testable on any host. [`expand_home`] is the wrapper
/// over the live environment.
pub fn expand_home_from(raw: &str, home: Option<&Path>) -> PathBuf {
    let Some(home) = home else {
        return PathBuf::from(raw);
    };
    for prefix in HOME_PREFIXES {
        let Some(rest) = raw.strip_prefix(prefix) else {
            continue;
        };
        // A bare `~` / `$HOME` only matches when nothing follows; otherwise
        // `~alice` and `$HOMEBREW` would silently expand.
        if !prefix.ends_with('/') && !rest.is_empty() {
            continue;
        }
        return if rest.is_empty() {
            home.to_path_buf()
        } else {
            home.join(rest)
        };
    }
    PathBuf::from(raw)
}

/// Expand a leading `~` / `$HOME` reference against the current user's home
/// directory, resolved portably (see [`home_dir`]).
pub fn expand_home(raw: &str) -> PathBuf {
    expand_home_from(raw, home_dir().as_deref())
}

/// [`expand_home`] for an existing `Path`. Paths that are not valid UTF-8
/// cannot contain a `~`/`$HOME` prefix we could act on, so they pass through.
pub fn expand_home_path(path: &Path) -> PathBuf {
    match path.to_str() {
        Some(raw) => expand_home(raw),
        None => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(value: &str) -> Option<&OsStr> {
        Some(OsStr::new(value))
    }

    fn home() -> Option<&'static Path> {
        Some(Path::new("/home/ada"))
    }

    #[test]
    fn unix_shape_prefers_home() {
        // Typical Unix: HOME set, USERPROFILE unset.
        assert_eq!(
            home_dir_from(os("/home/ada"), None),
            Some(PathBuf::from("/home/ada"))
        );
    }

    #[test]
    fn windows_shape_falls_back_to_userprofile() {
        // Typical Windows: HOME unset, USERPROFILE set. This is the case the
        // bare `var("HOME")` sites used to get wrong.
        assert_eq!(
            home_dir_from(None, os(r"C:\Users\Ada")),
            Some(PathBuf::from(r"C:\Users\Ada"))
        );
    }

    #[test]
    fn empty_home_falls_back_to_userprofile() {
        // Some shells export HOME="" rather than leaving it unset.
        assert_eq!(
            home_dir_from(os(""), os(r"C:\Users\Ada")),
            Some(PathBuf::from(r"C:\Users\Ada"))
        );
    }

    #[test]
    fn home_wins_when_both_present() {
        assert_eq!(
            home_dir_from(os("/home/ada"), os(r"C:\Users\Ada")),
            Some(PathBuf::from("/home/ada"))
        );
    }

    #[test]
    fn none_when_both_unset_or_empty() {
        assert_eq!(home_dir_from(None, None), None);
        assert_eq!(home_dir_from(os(""), os("")), None);
    }

    #[test]
    fn expands_every_home_prefix() {
        // Before consolidation each call site supported a different subset of
        // these four; the owner supports all of them.
        for raw in ["~", "$HOME"] {
            assert_eq!(expand_home_from(raw, home()), PathBuf::from("/home/ada"));
        }
        for raw in ["~/.config/harn", "$HOME/.config/harn"] {
            assert_eq!(
                expand_home_from(raw, home()),
                PathBuf::from("/home/ada/.config/harn")
            );
        }
    }

    #[test]
    fn leaves_non_home_paths_alone() {
        for raw in ["/etc/harn", "relative/path", ""] {
            assert_eq!(expand_home_from(raw, home()), PathBuf::from(raw));
        }
    }

    #[test]
    fn bare_prefixes_do_not_swallow_longer_tokens() {
        // `~alice` needs a passwd lookup we do not do, and `$HOMEBREW` is an
        // unrelated variable. Neither may expand.
        for raw in ["~alice", "~alice/bin", "$HOMEBREW", "$HOMEBREW_PREFIX"] {
            assert_eq!(expand_home_from(raw, home()), PathBuf::from(raw));
        }
    }

    #[test]
    fn only_leading_references_expand() {
        assert_eq!(
            expand_home_from("/opt/~/cache", home()),
            PathBuf::from("/opt/~/cache")
        );
    }

    #[test]
    fn unresolvable_home_returns_input_unchanged() {
        assert_eq!(expand_home_from("~/cache", None), PathBuf::from("~/cache"));
        assert_eq!(expand_home_from("~", None), PathBuf::from("~"));
    }

    #[test]
    fn expand_home_path_passes_through_non_utf8() {
        let raw = PathBuf::from("/etc/harn");
        assert_eq!(expand_home_path(&raw), raw);
    }

    #[test]
    fn package_cache_follows_the_home_it_is_given() {
        let home = PathBuf::from("/somewhere/else");
        let resolved = package_cache_dir_from(&home, None);
        assert!(
            resolved.starts_with(&home),
            "an explicit home must decide the cache root: {resolved:?}"
        );
    }

    #[test]
    fn package_cache_honours_xdg_cache_home_off_macos() {
        let resolved = package_cache_dir_from(
            Path::new("/home/u"),
            Some(OsStr::new("/var/cache/somewhere")),
        );
        let expected = if cfg!(target_os = "macos") {
            // macOS has no XDG convention, so the home path wins there.
            PathBuf::from("/home/u/Library/Caches/harn")
        } else {
            PathBuf::from("/var/cache/somewhere/harn")
        };
        assert_eq!(resolved, expected);
    }

    #[test]
    fn empty_xdg_cache_home_falls_back_to_the_home_default() {
        let resolved = package_cache_dir_from(Path::new("/home/u"), Some(OsStr::new("")));
        assert!(resolved.starts_with("/home/u"), "got {resolved:?}");
    }
}
