//! Product-path coverage for sharing a custom package cache with a nested,
//! sandboxed Harn invocation.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use crate::test_util::process::{harn_e2e_binary, harn_e2e_command};
use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.email=harn-test@example.com",
            "-c",
            "user.name=Harn Tests",
            "-c",
            "commit.gpgSign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn run_harn(cwd: &Path, cache: &Path, args: &[&str]) -> std::process::Output {
    harn_e2e_command()
        .current_dir(cwd)
        .env("HARN_CACHE_DIR", cache)
        .args(args)
        .output()
        .expect("run harn")
}

fn assert_success(output: &std::process::Output, command: &str) {
    assert!(
        output.status.success(),
        "{command} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn tree_contains(root: &Path, needle: &[u8]) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            tree_contains(&path, needle)
        } else {
            std::fs::read(path)
                .map(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
                .unwrap_or(false)
        }
    })
}

/// This is the canonical failure from #6823: `harn install --locked` warms a
/// non-default HARN_CACHE_DIR, then a sandboxed parent starts a nested `harn
/// run` after the dependency source and installed generation are gone. The
/// nested run can succeed only if the process sandbox both forwards and grants
/// that exact cache root. A unique deleted file:// source is the negative
/// control that prevents an ambient cache or network fetch from satisfying it.
#[test]
fn nested_sandboxed_run_reuses_the_installers_explicit_package_cache() {
    let temp = tempfile::tempdir().expect("test root");
    let workspace = temp.path().join("workspace");
    let child = workspace.join("child");
    let source = temp.path().join("dependency-source");
    let cache = temp.path().join("explicit-cache");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&cache).unwrap();

    git(&source, &["init", "-q", "-b", "main"]);
    std::fs::write(
        source.join("harn.toml"),
        "[package]\nname = \"acme-lib\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    std::fs::write(
        source.join("lib.harn"),
        "pub fn value() -> string { return \"from-explicit-cache\" }\n",
    )
    .unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-q", "-m", "package"]);
    git(&source, &["tag", "v1.0.0"]);
    let source_url = url::Url::from_file_path(source.canonicalize().unwrap())
        .expect("dependency source is an absolute file URL");

    std::fs::write(
        child.join("harn.toml"),
        format!(
            concat!(
                "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n\n",
                "[dependencies]\n",
                "acme-lib = {{ git = {:?}, tag = \"v1.0.0\" }}\n"
            ),
            source_url.as_str()
        ),
    )
    .unwrap();
    std::fs::write(
        child.join("main.harn"),
        concat!(
            "import { value } from \"acme-lib/lib\"\n\n",
            "fn main(harness: Harness) { harness.stdio.println(value()) }\n",
        ),
    )
    .unwrap();

    assert_success(&run_harn(&child, &cache, &["lock"]), "harn lock");
    assert_success(
        &run_harn(&child, &cache, &["install", "--locked"]),
        "harn install --locked",
    );
    assert!(
        cache.join("git").is_dir(),
        "installer did not warm custom cache"
    );

    // Make the cache the only remaining source of package bytes.
    std::fs::remove_dir_all(&source).unwrap();
    std::fs::remove_dir_all(child.join(".harn")).unwrap();

    let bin = serde_json::to_string(&harn_e2e_binary().display().to_string()).unwrap();
    let child_dir = serde_json::to_string(&child.display().to_string()).unwrap();
    std::fs::write(
        workspace.join("parent.harn"),
        format!(
            concat!(
                "fn main(harness: Harness) {{\n",
                "  const result = harness.process.run({{program: {}, args: [\"run\", \"main.harn\"], cwd: {}}})\n",
                "  harness.stdio.println(\"nested-success=\" + to_string(result.success))\n",
                "  harness.stdio.println(result.stdout)\n",
                "  harness.stdio.println(result.stderr)\n",
                "}}\n"
            ),
            bin,
            child_dir,
        ),
    )
    .unwrap();

    // Negative control: reproduce the old decision by withholding the custom
    // root and pointing XDG at a fresh base. The child must miss, proving that
    // the deleted source plus an empty derived cache cannot satisfy the run.
    let empty_xdg = temp.path().join("empty-xdg");
    std::fs::create_dir_all(&empty_xdg).unwrap();
    let negative = harn_e2e_command()
        .current_dir(&workspace)
        .env_remove("HARN_CACHE_DIR")
        .env("XDG_CACHE_HOME", &empty_xdg)
        .args(["run", "parent.harn"])
        .output()
        .expect("run cache-miss negative control");
    assert_success(&negative, "cache-miss negative-control parent");
    let negative_stdout = String::from_utf8_lossy(&negative.stdout);
    assert!(
        negative_stdout.contains("nested-success=false"),
        "negative control unexpectedly reached the dependency: {negative_stdout}"
    );
    assert!(!negative_stdout.contains("from-explicit-cache"));
    if child.join(".harn").exists() {
        std::fs::remove_dir_all(child.join(".harn")).unwrap();
    }

    const SECRET_MARKER: &str = "harn-cache-test-secret-must-not-persist";
    let output = harn_e2e_command()
        .current_dir(&workspace)
        .env("HARN_CACHE_DIR", &cache)
        .env("HARN_PACKAGE_REGISTRY_TOKEN", SECRET_MARKER)
        .args(["run", "parent.harn"])
        .output()
        .expect("run sandboxed parent");
    assert_success(&output, "sandboxed parent harn run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("nested-success=true"),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("from-explicit-cache"),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        child.join(".harn/package-current.toml").is_file(),
        "nested run did not publish a package generation from the cache"
    );
    assert!(!stdout.contains(SECRET_MARKER));
    assert!(!stderr.contains(SECRET_MARKER));
    assert!(!tree_contains(&workspace, SECRET_MARKER.as_bytes()));
    assert!(!tree_contains(&cache, SECRET_MARKER.as_bytes()));
}
