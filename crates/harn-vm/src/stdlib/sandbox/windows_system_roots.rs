//! Windows system read roots: the directories a confined child needs in
//! order to run the toolchains the parent process can already run.
//!
//! The product contract is reads-open, writes-confined. Every other backend
//! meets it by denying writes and leaving reads alone. The Windows backend
//! cannot: an AppContainer child reads a file only when that file's DACL
//! admits the container SID, a capability SID the token carries, or
//! `ALL APPLICATION PACKAGES` (`S-1-15-2-1`), which every AppContainer token
//! carries. Anything the host installed with an ACL that omits all three is
//! invisible to the child, and `cmd.exe` reports an unreadable executable as
//! "'x' is not recognized as an internal or external command" — a message
//! that reads as a PATH defect and is not one.
//!
//! This module owns the *set* of roots that gap covers: every existing
//! directory on the parent's `PATH`, plus the standard system prefixes and
//! the hosted tool cache when the environment names one. The Windows backend
//! is the only consumer.
//!
//! ## Why this set is not simply granted
//!
//! Granting read means `icacls /T` over the root, because Windows
//! inheritance is not dynamic: an inheritable entry placed on a directory
//! does not reach the files already inside it. Measured on a Windows 11 host,
//! that rewrite costs about a second over a Node install of 2449 files, while
//! reading the directory's permissions to find out whether it is needed at
//! all costs five milliseconds.
//!
//! Three things keep the cost bounded, and all three are load-bearing:
//!
//! * [`broad_system_root`] names the prefixes that are never granted under
//!   any circumstance, because rewriting `C:\Windows` or `C:\Program Files`
//!   takes minutes and mutates system state wholesale.
//! * [`hosts_an_executable`] drops the directories that cannot answer a
//!   command name, which is what keeps the budget available for the ones that
//!   can.
//! * The backend probes before it grants, and grants to the group every
//!   AppContainer already carries rather than to one spawn's own container,
//!   so a grant is made once on a host and every later spawn's probe finds it
//!   and skips.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::super::normalize_for_policy;

/// Environment variables that name the standard system prefixes. Read from
/// the environment rather than hard-coded so a non-`C:` system volume, a
/// relocated `ProgramData`, or a hosted runner's tool cache is covered.
const SYSTEM_ROOT_ENV_VARS: &[&str] = &[
    "SystemRoot",
    "windir",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "ProgramData",
    // Hosted CI images place their managed toolchains (node, python, go, …)
    // under a tool cache named by one of these; on a developer machine
    // neither is set and the entry is simply absent.
    "AGENT_TOOLSDIRECTORY",
    "RUNNER_TOOL_CACHE",
];

/// Every existing directory on the parent's `PATH`, plus the standard system
/// prefixes, normalized and deduplicated in `PATH` order (system prefixes
/// last). Non-existent and non-directory entries are dropped: a `PATH` entry
/// that does not resolve grants nothing and would only make a backend's
/// not-found handling ambiguous.
///
/// The result is cached for the life of the process. `PATH` is captured once
/// at startup by the OS and the system prefixes do not move, so re-reading
/// them per spawn would buy nothing and cost a syscall per entry on a `PATH`
/// that routinely runs past a hundred directories.
pub(crate) fn system_read_roots() -> Vec<PathBuf> {
    static ROOTS: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();
    ROOTS.get_or_init(compute_system_read_roots).clone()
}

fn compute_system_read_roots() -> Vec<PathBuf> {
    let path_entries = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    let system_entries = SYSTEM_ROOT_ENV_VARS
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from);
    let mut seen = BTreeSet::new();
    let mut roots = Vec::new();
    for entry in path_entries.into_iter().chain(system_entries) {
        if entry.as_os_str().is_empty() {
            continue;
        }
        let normalized = normalize_for_policy(&entry);
        if !normalized.is_absolute() || !normalized.is_dir() {
            continue;
        }
        // Case-insensitive identity: `C:\Windows\System32` and
        // `C:\WINDOWS\system32` are one root, and a `PATH` that names both
        // must not produce two grants.
        let key = normalized.to_string_lossy().to_ascii_lowercase();
        if seen.insert(key) {
            roots.push(normalized);
        }
    }
    roots
}

/// Whether `dir` directly holds a file Windows would run by name, i.e. one
/// whose extension appears in `PATHEXT`.
///
/// This is the test that decides whether a `PATH` directory is worth a
/// recursive ACL rewrite at all, and it follows from why the mechanism
/// exists: a child fails when it cannot read the interpreter its command
/// names, and a directory holding no executable cannot be where that
/// interpreter lives. Skipping those is not an optimization bolted onto a
/// correct set — it is what keeps the budget available for the directories
/// that can actually answer a command.
///
/// It is also the difference between working and not working on a Windows
/// build host. Cargo puts every native library search directory on `PATH`
/// when it runs a test binary, so the child's `PATH` opened with 39
/// `target\debug\build\*\out` directories, none of which hold an executable
/// and none of which carry an all-application-packages entry. Selected in
/// `PATH` order they filled the grant budget, the loop stopped, and the Node
/// install at position 109 was never reached (harn#7993, harn#8004).
///
/// Only the directory itself is read, never its subdirectories, and the
/// answer is cached: this runs once per root per process.
pub(crate) fn hosts_an_executable(dir: &Path) -> bool {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, bool>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Ok(map) = cache.lock() {
        if let Some(known) = map.get(dir) {
            return *known;
        }
    }
    let answer = read_hosts_an_executable(dir);
    if let Ok(mut map) = cache.lock() {
        map.insert(dir.to_path_buf(), answer);
    }
    answer
}

fn read_hosts_an_executable(dir: &Path) -> bool {
    let extensions = executable_extensions();
    let Ok(entries) = std::fs::read_dir(dir) else {
        // A directory we cannot enumerate is one we also cannot usefully
        // grant, and guessing "yes" would spend the budget this test exists
        // to protect.
        return false;
    };
    for entry in entries.flatten() {
        if !entry
            .file_type()
            .map(|kind| kind.is_file())
            .unwrap_or(false)
        {
            continue;
        }
        let name = entry.file_name();
        let Some(extension) = Path::new(&name).extension() else {
            continue;
        };
        let extension = format!(".{}", extension.to_string_lossy().to_ascii_uppercase());
        if extensions.iter().any(|candidate| *candidate == extension) {
            return true;
        }
    }
    false
}

/// The extensions `PATHEXT` names, upper-cased and dot-prefixed. Read from
/// the environment because a host may add to it; the fallback is the set
/// Windows ships with, so an unset `PATHEXT` narrows nothing.
fn executable_extensions() -> Vec<String> {
    parse_executable_extensions(&std::env::var("PATHEXT").unwrap_or_default())
}

/// Split on the environment value rather than the environment, so the empty
/// and malformed cases are provable without mutating process state.
fn parse_executable_extensions(raw: &str) -> Vec<String> {
    let parsed: Vec<String> = raw
        .split(';')
        .map(|entry| entry.trim().to_ascii_uppercase())
        .filter(|entry| entry.starts_with('.') && entry.len() > 1)
        .collect();
    if parsed.is_empty() {
        return [".COM", ".EXE", ".BAT", ".CMD"]
            .iter()
            .map(|entry| (*entry).to_string())
            .collect();
    }
    parsed
}

/// How many entries `dir` contains, counted recursively but abandoned as soon
/// as the count passes `ceiling`. `None` means "larger than `ceiling`", which
/// is the only fact the caller needs from a tree too big to grant.
///
/// The cost of opening a directory is proportional to how many files are
/// inside it, so the size of the tree is the honest way to decide whether the
/// rewrite is affordable — not the directory's name, and not where it sits.
/// The walk is bounded by `ceiling`, so asking the question is cheap even
/// when the answer is "enormous".
///
/// This is what stops a build tree that survives every other test. A cargo
/// target directory is on `PATH` when tests run, it holds test executables so
/// it can answer a command name, and nothing already grants it, so it reaches
/// this point looking exactly like a toolchain. It is also tens of thousands
/// of files, and rewriting it takes minutes: on a Windows 11 host every
/// sandboxed command timed out at two minutes with `target\debug` and
/// `target\debug\deps` selected for granting (harn#8004).
/// [`tree_entry_count_within`] against a fixed ceiling, cached per directory.
///
/// The ceiling is fixed so the answer is a property of the directory rather
/// than of how much budget happened to be left when it was first asked, which
/// is what makes caching sound. Every root is then walked at most once per
/// process, however many times it is reconsidered.
pub(crate) fn cached_tree_entry_count(dir: &Path, ceiling: usize) -> Option<usize> {
    type Cache = std::sync::Mutex<std::collections::HashMap<(PathBuf, usize), Option<usize>>>;
    static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let key = (dir.to_path_buf(), ceiling);
    if let Ok(map) = cache.lock() {
        if let Some(known) = map.get(&key) {
            return *known;
        }
    }
    let answer = tree_entry_count_within(dir, ceiling);
    if let Ok(mut map) = cache.lock() {
        map.insert(key, answer);
    }
    answer
}

pub(crate) fn tree_entry_count_within(dir: &Path, ceiling: usize) -> Option<usize> {
    let mut count = 0usize;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            // Unreadable subtrees are not counted. They also cannot be
            // granted, so treating them as empty neither hides cost nor
            // invents it.
            continue;
        };
        for entry in entries.flatten() {
            count += 1;
            if count > ceiling {
                return None;
            }
            if entry
                .file_type()
                .map(|kind| kind.is_dir() && !kind.is_symlink())
                .unwrap_or(false)
            {
                pending.push(entry.path());
            }
        }
    }
    Some(count)
}

/// Roots that are never handed to a recursive ACL grant, whatever a probe
/// says about them: a drive root, the Windows directory, either Program
/// Files prefix, `ProgramData`, and the user's home. Rewriting the ACLs
/// under any of these takes minutes and mutates system state that outlives
/// the spawn if the process dies before its grants are removed.
///
/// A leaf under one of them (`C:\Program Files\nodejs`) is not broad; the
/// prefix itself is.
pub(crate) fn broad_system_root(path: &Path) -> bool {
    // A path with no parent, or whose only components are a drive prefix and
    // a root, is a volume root.
    let depth = path
        .components()
        .filter(|component| {
            matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::ParentDir
            )
        })
        .count();
    if depth == 0 {
        return true;
    }
    let normalized = path.to_string_lossy().to_ascii_lowercase();
    let normalized = normalized.trim_end_matches(['\\', '/']).to_string();
    SYSTEM_ROOT_ENV_VARS
        .iter()
        .filter_map(std::env::var_os)
        .map(|value| {
            normalize_for_policy(Path::new(&value))
                .to_string_lossy()
                .to_ascii_lowercase()
                .trim_end_matches(['\\', '/'])
                .to_string()
        })
        .chain(crate::user_dirs::home_dir().map(|home| {
            normalize_for_policy(&home)
                .to_string_lossy()
                .to_ascii_lowercase()
                .trim_end_matches(['\\', '/'])
                .to_string()
        }))
        .any(|root| !root.is_empty() && root == normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_volume_root_is_broad() {
        assert!(broad_system_root(Path::new("C:\\")));
        assert!(broad_system_root(Path::new("\\")));
    }

    #[test]
    fn a_leaf_under_a_system_prefix_is_not_broad() {
        // The prefix itself is broad only when the environment names it, so
        // this asserts the shape that never depends on the host: a two-deep
        // path under a prefix is a leaf, and leaves are grantable.
        assert!(!broad_system_root(Path::new("C:\\Program Files\\nodejs")));
    }

    #[test]
    fn a_directory_holding_an_executable_is_grant_worthy() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("tool.exe"), b"").expect("write tool");
        assert!(hosts_an_executable(dir.path()));
    }

    #[test]
    fn a_directory_holding_no_executable_is_not_grant_worthy() {
        // The shape that broke the mechanism on a Windows build host: cargo
        // puts native library search directories on PATH, and they carry
        // object files and headers but nothing runnable. Selected in PATH
        // order they consumed the whole grant budget before the Node install
        // was reached.
        let dir = tempfile::tempdir().expect("temp dir");
        for name in ["libfoo.lib", "foo.o", "foo.h", "output"] {
            std::fs::write(dir.path().join(name), b"").expect("write artifact");
        }
        assert!(!hosts_an_executable(dir.path()));
    }

    #[test]
    fn an_executable_in_a_subdirectory_does_not_make_the_parent_grant_worthy() {
        // Only a directory ON `PATH` resolves a command name, so a nested
        // executable is not evidence that this directory answers one.
        let dir = tempfile::tempdir().expect("temp dir");
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).expect("create nested");
        std::fs::write(nested.join("tool.exe"), b"").expect("write tool");
        assert!(!hosts_an_executable(dir.path()));
    }

    #[test]
    fn a_missing_directory_is_not_grant_worthy() {
        let dir = tempfile::tempdir().expect("temp dir");
        let absent = dir.path().join("does-not-exist");
        assert!(!hosts_an_executable(&absent));
    }

    #[test]
    fn an_absent_or_malformed_pathext_falls_back_to_the_windows_default_set() {
        // An unset PATHEXT must never narrow the set to nothing, because that
        // would make every PATH directory look executable-free and silently
        // disable the whole mechanism.
        for raw in ["", "   ", ";;;", "bogus"] {
            let extensions = parse_executable_extensions(raw);
            assert!(
                extensions.iter().any(|entry| entry == ".EXE"),
                "PATHEXT {raw:?} produced {extensions:?}, which cannot match an .exe"
            );
        }
    }

    #[test]
    fn pathext_is_honoured_and_normalized_when_the_host_sets_one() {
        let extensions = parse_executable_extensions(".com;.Exe; .Ps1 ;notanext");
        assert_eq!(extensions, vec![".COM", ".EXE", ".PS1"]);
    }

    #[test]
    fn a_tree_under_the_ceiling_is_counted_exactly() {
        let dir = tempfile::tempdir().expect("temp dir");
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).expect("create nested");
        std::fs::write(dir.path().join("a.txt"), b"").expect("write a");
        std::fs::write(nested.join("b.txt"), b"").expect("write b");
        // Two files plus the directory itself.
        assert_eq!(tree_entry_count_within(dir.path(), 64), Some(3));
    }

    #[test]
    fn a_tree_over_the_ceiling_is_abandoned_rather_than_counted() {
        // The build-tree case: the answer must be "too big", and getting it
        // must not require walking the whole thing.
        let dir = tempfile::tempdir().expect("temp dir");
        for index in 0..32 {
            std::fs::write(dir.path().join(format!("{index}.txt")), b"").expect("write");
        }
        assert_eq!(tree_entry_count_within(dir.path(), 8), None);
    }

    #[test]
    fn an_unreadable_or_missing_tree_counts_as_empty_rather_than_enormous() {
        let dir = tempfile::tempdir().expect("temp dir");
        let absent = dir.path().join("does-not-exist");
        assert_eq!(tree_entry_count_within(&absent, 64), Some(0));
    }

    #[test]
    fn system_read_roots_are_absolute_directories_without_duplicates() {
        let roots = system_read_roots();
        let mut seen = BTreeSet::new();
        for root in &roots {
            assert!(root.is_absolute(), "non-absolute read root {root:?}");
            assert!(root.is_dir(), "non-directory read root {root:?}");
            assert!(
                seen.insert(root.to_string_lossy().to_ascii_lowercase()),
                "duplicate read root {root:?}"
            );
        }
    }
}
