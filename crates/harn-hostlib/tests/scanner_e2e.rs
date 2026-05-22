//! End-to-end tests for the scanner host capability.
//!
//! Builds a small fixture repository inside a temp dir and asserts every
//! piece of the `ScanResult` shape: file discovery, file roles
//! (`.gitignore` + excluded-dir filtering), symbol/import extraction,
//! dependency edges, test pairings, folder + project metadata,
//! sub-project boundaries, repo-map text, and the
//! `scan_project → scan_incremental` round-trip.
//!
//! The fixture below is hand-shaped to exercise common sub-tree patterns
//! (Cargo + nested package.json, `__tests__/` test files,
//! paginated-response helpers, prisma schema) without forcing a
//! heavyweight checkout.

use std::fs;
use std::path::Path;

use harn_hostlib::scanner::{
    scan_incremental, scan_project, scan_project_with_git, FileRecord, GitCapabilities,
    ScanProjectOptions, SymbolKind,
};
use tempfile::tempdir;

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, content).unwrap();
}

fn build_fixture(root: &Path) {
    write(
        root,
        "Cargo.toml",
        r#"[package]
name = "scanner-fixture"
version = "0.1.0"
edition = "2024"
"#,
    );
    write(
        root,
        "package.json",
        r#"{
  "name": "scanner-fixture",
  "scripts": {
    "test": "vitest run",
    "lint": "eslint .",
    "build": "tsc -b"
  }
}
"#,
    );
    write(
        root,
        "src/main.rs",
        r#"use crate::routes::accounts;

pub struct App;

impl App {
    pub fn run(&self) {}
}

pub fn entry() {}
"#,
    );
    write(
        root,
        "src/routes/accounts.rs",
        r#"use std::collections::HashMap;

pub struct AccountsService;

impl AccountsService {
    pub fn lookup(&self, id: u64) -> Option<()> { None }
}
"#,
    );
    write(
        root,
        "src/routes/accounts_test.rs",
        r#"use super::accounts::AccountsService;
"#,
    );
    write(
        root,
        "src/lib/helpers.ts",
        r#"export function paginatedResponse<T>(items: T[], total: number) {
  return { items, total, page: 1 };
}

export function asyncHandler(fn: any) { return fn; }
"#,
    );
    write(
        root,
        "src/lib/__tests__/helpers.test.ts",
        r#"import { paginatedResponse } from "../helpers";
"#,
    );
    write(root, "prisma/schema.prisma", "model User { id Int @id }\n");
    // Hidden + excluded dirs that must never appear in the result.
    write(root, ".env", "SECRET=1\n");
    write(root, "node_modules/foo/index.js", "module.exports = 1;\n");
    write(root, "target/debug/junk.txt", "ignore me\n");
    // Nested sub-project at a 2nd-level path.
    write(
        root,
        "services/api/Cargo.toml",
        r#"[package]
name = "api"
version = "0.1.0"
edition = "2024"
"#,
    );
    write(
        root,
        "services/api/src/lib.rs",
        "pub fn ping() -> bool { true }\n",
    );
}

fn touch_after_snapshot(path: &Path, snapshot_max_mtime_ms: i64) {
    let base_secs = snapshot_max_mtime_ms.max(0).div_euclid(1_000);
    let new_mtime = filetime::FileTime::from_unix_time(base_secs.saturating_add(60), 0);
    filetime::set_file_mtime(path, new_mtime).unwrap();
}

#[derive(Default)]
struct MockGit {
    files: Option<Vec<String>>,
    churn: std::collections::BTreeMap<String, f64>,
}

impl GitCapabilities for MockGit {
    fn list_files(&self, _root: &Path) -> Option<Vec<String>> {
        self.files.clone()
    }

    fn churn_scores(&self, _root: &Path) -> std::collections::BTreeMap<String, f64> {
        self.churn.clone()
    }
}

#[test]
fn scan_project_emits_full_result_shape() {
    let tmp = tempdir().unwrap();
    build_fixture(tmp.path());

    let result = scan_project(tmp.path(), ScanProjectOptions::default());

    // Discovery: source files present, excluded dirs gone.
    let paths: Vec<&str> = result
        .files
        .iter()
        .map(|f| f.relative_path.as_str())
        .collect();
    assert!(paths.contains(&"src/main.rs"));
    assert!(paths.contains(&"src/routes/accounts.rs"));
    assert!(paths.contains(&"src/lib/helpers.ts"));
    assert!(!paths.iter().any(|p| p.starts_with("node_modules")));
    assert!(!paths.iter().any(|p| p.starts_with("target")));

    // File records have language + line counts.
    let main = result
        .files
        .iter()
        .find(|f| f.relative_path == "src/main.rs")
        .expect("main.rs file record");
    assert_eq!(main.language, "rs");
    assert!(main.line_count > 0);
    assert!(main.size_bytes > 0);
    assert!(main.imports.iter().any(|i| i.contains("routes")));

    // Symbol records cover both Rust and TS source.
    let symbol_names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(symbol_names.contains(&"App"));
    assert!(symbol_names.contains(&"AccountsService"));
    assert!(symbol_names.contains(&"paginatedResponse"));

    // Dependency edges built from imports.
    assert!(result
        .dependencies
        .iter()
        .any(|d| d.from_file == "src/routes/accounts.rs" && d.to_module.contains("HashMap")));

    // Test-source pairing: helpers.ts ↔ helpers.test.ts.
    let helpers = result
        .files
        .iter()
        .find(|f| f.relative_path == "src/lib/helpers.ts")
        .unwrap();
    assert_eq!(
        helpers.corresponding_test_file.as_deref(),
        Some("src/lib/__tests__/helpers.test.ts")
    );

    // Folder records sorted by line count desc.
    assert!(!result.folders.is_empty());
    for window in result.folders.windows(2) {
        assert!(window[0].line_count >= window[1].line_count);
    }

    // Project metadata: language stats, test commands, code patterns.
    assert_eq!(result.project.name, project_name(tmp.path()));
    assert!(result.project.languages.iter().any(|l| l.name == "rs"));
    assert!(result.project.languages.iter().any(|l| l.name == "ts"));
    assert!(result.project.test_commands.contains_key("cargo test"));
    assert!(result.project.test_commands.contains_key("pnpm test"));
    let detected = result
        .project
        .detected_test_command
        .as_deref()
        .expect("a test command should be detected");
    assert!(!detected.is_empty());
    assert!(result
        .project
        .code_patterns
        .iter()
        .any(|p| p.contains("Prisma")));

    // Sub-project detection: outer fixture + nested api crate.
    let sub_marker_count = result
        .sub_projects
        .iter()
        .filter(|s| s.project_marker == "Cargo.toml" || s.project_marker == "package.json")
        .count();
    assert!(sub_marker_count >= 2);

    // Repo map renders symbols.
    assert!(result.repo_map.contains("src/main.rs"));
    assert!(result.repo_map.contains("App"));

    // Output is deterministic — files sorted alphabetically.
    let sorted: Vec<_> = result
        .files
        .iter()
        .map(|f| &f.relative_path)
        .collect::<Vec<_>>();
    let mut copy = sorted.clone();
    copy.sort();
    assert_eq!(sorted, copy);

    // Snapshot persisted at the canonical location.
    let snapshot_path = tmp.path().join(".harn/hostlib/scanner-snapshot.json");
    assert!(snapshot_path.exists());
}

#[test]
fn scan_project_uses_injected_git_capabilities() {
    let tmp = tempdir().unwrap();
    build_fixture(tmp.path());

    let git = MockGit {
        files: Some(vec![
            "Cargo.toml".to_string(),
            "src/main.rs".to_string(),
            "src/routes/accounts.rs".to_string(),
            "target/debug/junk.txt".to_string(),
        ]),
        churn: [("src/main.rs".to_string(), 0.75)].into_iter().collect(),
    };
    let result = scan_project_with_git(tmp.path(), ScanProjectOptions::default(), &git);
    let paths: Vec<&str> = result
        .files
        .iter()
        .map(|f| f.relative_path.as_str())
        .collect();

    assert_eq!(
        paths,
        vec!["Cargo.toml", "src/main.rs", "src/routes/accounts.rs"]
    );
    let main = find_file(&result.files, "src/main.rs").unwrap();
    assert_eq!(main.churn_score, 0.75);
}

fn project_name(root: &Path) -> String {
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    canonical
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

#[test]
fn scan_incremental_picks_up_added_and_removed_files() {
    let tmp = tempdir().unwrap();
    build_fixture(tmp.path());

    let initial = scan_project(tmp.path(), ScanProjectOptions::default());
    let token = initial.snapshot_token.clone();
    let snapshot_max_mtime_ms = initial
        .files
        .iter()
        .map(|file| file.last_modified_unix_ms)
        .max()
        .unwrap_or(0);

    // Add a new source file and remove an existing one.
    write(
        tmp.path(),
        "src/routes/orders.rs",
        "pub struct OrdersService;\n",
    );
    fs::remove_file(tmp.path().join("src/lib/helpers.ts")).unwrap();
    touch_after_snapshot(
        &tmp.path().join("src/routes/orders.rs"),
        snapshot_max_mtime_ms,
    );

    let scan = scan_incremental(&token, None, ScanProjectOptions::default());
    assert!(scan.delta.added.iter().any(|p| p == "src/routes/orders.rs"));
    assert!(scan.delta.removed.iter().any(|p| p == "src/lib/helpers.ts"));
    let result = &scan.result;
    let new_paths: Vec<&str> = result
        .files
        .iter()
        .map(|f| f.relative_path.as_str())
        .collect();
    assert!(new_paths.contains(&"src/routes/orders.rs"));
    assert!(!new_paths.contains(&"src/lib/helpers.ts"));
}

#[test]
fn scan_incremental_full_rescan_when_snapshot_missing() {
    let tmp = tempdir().unwrap();
    build_fixture(tmp.path());

    let token = tmp.path().to_string_lossy().into_owned();
    let scan = scan_incremental(&token, None, ScanProjectOptions::default());
    assert!(scan.delta.full_rescan);
    assert!(!scan.result.files.is_empty());
}

#[test]
fn scan_incremental_modifies_via_explicit_changed_paths() {
    let tmp = tempdir().unwrap();
    build_fixture(tmp.path());

    let initial = scan_project(tmp.path(), ScanProjectOptions::default());
    let token = initial.snapshot_token.clone();

    // Replace the contents of an existing file.
    write(
        tmp.path(),
        "src/main.rs",
        "pub fn entry() { println!(\"refreshed\"); }\n",
    );

    let scan = scan_incremental(
        &token,
        Some(&["src/main.rs".to_string()]),
        ScanProjectOptions::default(),
    );
    assert!(scan.delta.modified.iter().any(|p| p == "src/main.rs"));
    let main = find_file(&scan.result.files, "src/main.rs").unwrap();
    assert!(main.imports.is_empty());
}

fn find_file<'a>(files: &'a [FileRecord], path: &str) -> Option<&'a FileRecord> {
    files.iter().find(|f| f.relative_path == path)
}

#[test]
fn scan_project_truncates_to_max_files() {
    let tmp = tempdir().unwrap();
    build_fixture(tmp.path());

    let opts = ScanProjectOptions {
        max_files: 2,
        ..ScanProjectOptions::default()
    };
    let result = scan_project(tmp.path(), opts);
    assert!(result.truncated);
    assert!(result.files.len() <= 2);
}

#[test]
fn symbol_records_carry_canonical_kind() {
    let tmp = tempdir().unwrap();
    build_fixture(tmp.path());

    let result = scan_project(tmp.path(), ScanProjectOptions::default());
    let app = result.symbols.iter().find(|s| s.name == "App").unwrap();
    assert_eq!(app.kind, SymbolKind::StructDecl);
    let entry = result.symbols.iter().find(|s| s.name == "entry").unwrap();
    assert_eq!(entry.kind, SymbolKind::Function);
}

#[test]
fn scan_project_workspace_shape_smoke_test() {
    let tmp = tempdir().unwrap();
    build_fixture(tmp.path());
    write(
        tmp.path(),
        ".github/workflows/ci.yml",
        "name: ci\non: [push]\njobs:\n  test:\n    runs-on: ubuntu-latest\n",
    );
    write(
        tmp.path(),
        "docs/src/language-spec.md",
        "# Language Spec\n\n```harn\nfn main() { 1 }\n```\n",
    );
    write(
        tmp.path(),
        "crates/harn-hostlib/src/lib.rs",
        "pub mod scanner;\npub fn install() {}\n",
    );

    let result = scan_project_with_git(
        tmp.path(),
        ScanProjectOptions {
            include_git_history: false,
            ..ScanProjectOptions::default()
        },
        &MockGit::default(),
    );

    assert!(
        !result.files.is_empty(),
        "workspace fixture should have files"
    );
    assert!(
        !result.symbols.is_empty(),
        "workspace fixture should have symbols"
    );

    let names: Vec<&str> = result
        .files
        .iter()
        .map(|f| f.relative_path.as_str())
        .collect();
    assert!(names.iter().any(|n| n.ends_with("Cargo.toml")));
    assert!(names.iter().any(|n| n.contains("crates/harn-hostlib")));
}
