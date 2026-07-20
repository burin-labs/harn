//! Working-directory policy for hardened git invocations.
//!
//! Remote operations (`ls-remote`, `clone <url> <absolute-dest>`) act on a URL
//! and need no working tree at all — but a spawned `git` still calls `getcwd()`
//! at startup and aborts with `fatal: Unable to read current working directory`
//! if the *inherited process CWD* has been deleted. Under a threaded test
//! runner that happens routinely: any concurrent test whose `TempDir` (or an
//! external tmp sweep) removes the directory the process happens to be sitting
//! in kills an unrelated remote git call. That is not hypothetical — it is the
//! failure this module exists to prevent.
//!
//! [`Cwd`] makes the safe choice the only choice: there is deliberately no
//! "inherit the process CWD" variant, so a caller *cannot* express the
//! dependency that caused the flake. [`Cwd::Detached`] pins a call to a
//! neutral, guaranteed-to-exist directory; [`Cwd::In`] is for operations that
//! genuinely act on a working tree.

use std::path::Path;

/// Working directory for a hardened git invocation.
pub(crate) enum Cwd<'a> {
    /// Run git with its working directory set to this path.
    In(&'a Path),
    /// Run git detached from the process CWD. Resolves to a caller-supplied
    /// neutral directory that is known to exist for the call's lifetime.
    Detached,
}

impl<'a> Cwd<'a> {
    /// Resolve to the concrete working directory to hand `git`. `neutral` is
    /// the fallback for [`Cwd::Detached`] — a directory the caller guarantees
    /// exists (in practice the hardened-env `HOME` tempdir). The process CWD is
    /// never consulted, which is the entire point.
    pub(crate) fn resolve(&self, neutral: &'a Path) -> &'a Path {
        match self {
            Cwd::In(dir) => dir,
            Cwd::Detached => neutral,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn detached_resolves_to_the_neutral_dir_never_the_process_cwd() {
        // The regression contract: a detached invocation is pinned to the
        // neutral directory the caller vouches for, so it can never inherit —
        // and die on — a process CWD that some concurrent test deleted. This
        // asserts the invariant without ever touching the real process CWD.
        let neutral = PathBuf::from("/neutral/existing/dir");
        assert_eq!(Cwd::Detached.resolve(&neutral), neutral.as_path());
    }

    #[test]
    fn in_resolves_to_its_own_path_ignoring_the_neutral_dir() {
        let neutral = PathBuf::from("/neutral/existing/dir");
        let work = PathBuf::from("/some/worktree");
        assert_eq!(Cwd::In(&work).resolve(&neutral), work.as_path());
    }
}
