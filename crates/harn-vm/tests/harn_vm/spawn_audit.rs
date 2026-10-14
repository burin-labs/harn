//! Every child process the runtime starts goes through the session
//! environment funnel.
//!
//! A raw `Command::new` inherits the engine's whole environment, including
//! provider credentials and `in_process` grants that the session policy keeps
//! out of children. The funnel is `process_sandbox::session_std_command` /
//! `session_tokio_command` (environment only) and `std_command_for` /
//! `tokio_command_for` (environment plus sandbox confinement). This audit
//! scans the crates that spawn on a session's behalf and fails on a raw
//! constructor outside the sites listed below, each of which is either the
//! funnel itself or states why it may not use it.
//!
//! Test code is not audited: `*_tests.rs`, `tests.rs`, `tests/` trees, test
//! helper binaries, and everything from an inline `#[cfg(test)] mod` to the
//! end of its file.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Production sites allowed to construct a raw command, with the count each
/// may hold. A new raw spawn anywhere else, or an extra one here, fails.
const ALLOWED_RAW_SPAWNS: &[(&str, usize, &str)] = &[
    (
        "harn-vm/src/stdlib/sandbox/command_for.rs",
        7,
        "the funnel: builds the command and closes its environment",
    ),
    (
        "harn-vm/src/stdlib/sandbox/build_command.rs",
        8,
        "the funnel's confined builders; command_for.rs closes their environment",
    ),
    (
        "harn-vm/src/stdlib/sandbox/linux_bwrap_probe.rs",
        1,
        "setup-only availability probe; clears the environment and uses absolute programs \
         without payload grants",
    ),
    (
        "harn-vm/src/stdlib/sandbox/mod.rs",
        1,
        "command_output fallback; spawns from a config the session policy already closed",
    ),
    (
        "harn-vm/src/stdlib/process.rs",
        1,
        "run_captured_spawn; applies session_closed_env_for_command before spawning",
    ),
    (
        "harn-hostlib/src/process/owner_death.rs",
        3,
        "process-owner guardian: re-executes this binary with the sensitive environment \
         stripped, then rebuilds the child from the funnel's closed environment",
    ),
];

const AUDITED_CRATES: &[&str] = &["harn-vm", "harn-hostlib", "harn-serve"];

#[test]
fn every_runtime_spawn_goes_through_the_session_environment_funnel() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let crates_dir = manifest_dir.parent().expect("crate under crates/");
    let mut actual: BTreeMap<String, usize> = BTreeMap::new();
    for krate in AUDITED_CRATES {
        let src = crates_dir.join(krate).join("src");
        scan(&src, crates_dir, &mut actual);
    }
    // The scan must have found the funnel itself, or it measured nothing.
    assert!(
        actual
            .get("harn-vm/src/stdlib/sandbox/command_for.rs")
            .is_some_and(|count| *count > 0),
        "the audit did not find the funnel's own constructors; the scan is broken: {actual:?}"
    );
    let allowed: BTreeMap<&str, usize> = ALLOWED_RAW_SPAWNS
        .iter()
        .map(|(path, count, _)| (*path, *count))
        .collect();
    let mut failures = Vec::new();
    for (path, count) in &actual {
        match allowed.get(path.as_str()) {
            None => failures.push(format!(
                "{path}: {count} raw Command::new; build the child with \
                 process_sandbox::session_std_command / session_tokio_command \
                 (or std_command_for when it must be confined)"
            )),
            Some(expected) if count > expected => failures.push(format!(
                "{path}: {count} raw Command::new, {expected} allowed"
            )),
            Some(_) => {}
        }
    }
    for (path, expected, _) in ALLOWED_RAW_SPAWNS {
        let found = actual.get(*path).copied().unwrap_or(0);
        if found < *expected {
            failures.push(format!(
                "{path}: allowance of {expected} is stale, found {found}; lower it"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn scan(dir: &Path, crates_dir: &Path, out: &mut BTreeMap<String, usize>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if name == "tests" {
                continue;
            }
            scan(&path, crates_dir, out);
            continue;
        }
        if !name.ends_with(".rs")
            || name.ends_with("_tests.rs")
            || name == "tests.rs"
            || name.starts_with("harn_test_")
        {
            continue;
        }
        let source = fs::read_to_string(&path).expect("read source");
        let count = raw_spawns(&production_region(&source));
        if count > 0 {
            let relative = path
                .strip_prefix(crates_dir)
                .expect("under crates/")
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(relative, count);
        }
    }
}

/// The file up to its first inline test module.
fn production_region(source: &str) -> String {
    let mut region = String::new();
    let mut pending_test_cfg = false;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with("#[cfg(") && trimmed.contains("test") {
            pending_test_cfg = true;
        } else if pending_test_cfg && trimmed.starts_with("mod ") && trimmed.ends_with('{') {
            break;
        } else if !trimmed.starts_with("#[") {
            pending_test_cfg = false;
        }
        region.push_str(line);
    }
    region
}

/// Raw `Command::new(` constructors, not ones on a type that merely ends in
/// `Command` (for example a builder named `MutableCommand`).
fn raw_spawns(source: &str) -> usize {
    source
        .match_indices("Command::new(")
        .filter(|(index, _)| {
            source
                .get(..*index)
                .and_then(|before| before.chars().next_back())
                .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
        })
        .count()
}
