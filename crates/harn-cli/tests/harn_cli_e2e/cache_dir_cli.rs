//! Canonical CLI path for the bytecode-cache directory contract (#7066, #7067).
//!
//! `initialize_runtime` runs before clap, so `harn --version` is enough to
//! prove the startup gate. `run_harn_e2e` treats an empty env value as
//! unset, which is the opposite of #7066 — those cases spawn directly.

use std::process::Command;

use tempfile::TempDir;

use crate::test_util::process::{harn_e2e_binary, harn_e2e_command, run_harn_e2e};

#[test]
fn empty_harn_cache_dir_is_a_startup_error() {
    let output = Command::new(harn_e2e_binary())
        .arg("--version")
        .env("HARN_CACHE_DIR", "")
        .output()
        .expect("spawn harn");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr={stderr}");
    assert!(
        output.stdout.is_empty(),
        "stdout leaked: {:?}",
        output.stdout
    );
    assert!(
        stderr.contains("HARN_CACHE_DIR") && stderr.contains("empty"),
        "stderr={stderr}"
    );
}

#[test]
fn relative_harn_cache_dir_is_a_startup_error() {
    let cwd = TempDir::new().unwrap();
    let output = harn_e2e_command()
        .arg("--version")
        .current_dir(cwd.path())
        .env("HARN_CACHE_DIR", "relative/cache")
        .output()
        .expect("spawn harn");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr={stderr}");
    assert!(
        stderr.contains("HARN_CACHE_DIR") && stderr.contains("relative/cache"),
        "stderr={stderr}"
    );
    assert!(
        !cwd.path().join(".harn-cache").exists(),
        "a rejected relative override must not create a working-directory cache"
    );
}

#[test]
fn absolute_harn_cache_dir_is_accepted() {
    let cache = TempDir::new().unwrap();
    let output = run_harn_e2e(
        &["--version"],
        &[("HARN_CACHE_DIR", cache.path().to_str().unwrap())],
    );
    assert_eq!(output.exit_code, 0, "stderr={}", output.stderr);
    assert!(
        !output.stderr.contains("HARN_CACHE_DIR"),
        "a valid override must not warn; stderr={}",
        output.stderr
    );
}

/// #7067: with no override, no XDG cache home, and no home directory, the
/// process must not invent `./.harn-cache` beside whatever directory it
/// happened to start in.
#[test]
fn missing_home_disables_caching_instead_of_writing_beside_cwd() {
    let cwd = TempDir::new().unwrap();
    let output = harn_e2e_command()
        .arg("--version")
        .current_dir(cwd.path())
        .env_remove("HARN_CACHE_DIR")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("HOME")
        .env_remove("USERPROFILE")
        .output()
        .expect("spawn harn");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !cwd.path().join(".harn-cache").exists(),
        "no resolvable cache root must not fall back to a cwd-relative directory; stderr={stderr}"
    );
    assert!(
        stderr.contains("no cache directory resolves"),
        "the operator should see why caching is off; stderr={stderr}"
    );
}
