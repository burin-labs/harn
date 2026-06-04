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
use std::path::PathBuf;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn os(value: &str) -> Option<&OsStr> {
        Some(OsStr::new(value))
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
}
