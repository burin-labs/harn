use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::value::VmValue;
use crate::VmError;

fn temp_dir(label: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("harn-project-{label}-"))
        .tempdir()
        .expect("tempdir")
}

#[test]
fn scan_detects_rust_workspace_root_without_nested_source_walk() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate dir")
        .parent()
        .expect("workspace root")
        .to_path_buf();
    let evidence = scan_exact_directory(&repo_root, &ProjectScanOptions::default());
    assert!(sorted_confident_labels(&evidence.language_scores).contains(&"rust".to_string()));
    assert!(
        evidence
            .language_scores
            .get("rust")
            .copied()
            .unwrap_or_default()
            >= 0.95
    );
}

#[test]
fn package_name_detection_matches_manifest_priority() {
    let dir = temp_dir("package-name");
    std::fs::write(
        dir.path().join("pyproject.toml"),
        "[project]\nname = \"python-name\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("package.json"), "{\"name\":\"node-name\"}").unwrap();
    std::fs::write(
        dir.path().join("go.mod"),
        "module github.com/acme/go-name\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"cargo-name\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    assert_eq!(
        detect_package_name(dir.path()).as_deref(),
        Some("python-name")
    );
}

fn context_options_without_env() -> ContextProfileOptions {
    ContextProfileOptions {
        include_env_credentials: false,
        ..ContextProfileOptions::default()
    }
}

fn context_profile_ids(resolution: &ContextProfileResolution) -> Vec<String> {
    resolution
        .profiles
        .iter()
        .map(|profile| profile.id.clone())
        .collect()
}

fn flatten_profile_field(
    resolution: &ContextProfileResolution,
    field: fn(&ContextProfileActivation) -> Vec<String>,
) -> Vec<String> {
    unique_flatten(resolution.profiles.iter().flat_map(field))
}

#[test]
fn context_profile_resolves_github_remote_with_credentials() {
    let dir = temp_dir("context-github");
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::write(
        dir.path().join(".git/config"),
        "[remote \"origin\"]\n\turl = https://github.com/burin-labs/harn.git\n",
    )
    .unwrap();

    let mut options = context_options_without_env();
    options.credentials = BTreeSet::from(["github".to_string()]);
    let resolution = resolve_context_profile(dir.path(), options);

    assert_eq!(
        context_profile_ids(&resolution),
        vec!["git".to_string(), "github".to_string()]
    );
    assert_eq!(
        flatten_profile_field(&resolution, |profile| profile.skills.clone()),
        vec!["git".to_string(), "github".to_string()]
    );
    assert_eq!(
        flatten_profile_field(&resolution, |profile| profile.mcp_presets.clone()),
        vec!["github".to_string()]
    );
    assert_eq!(
        resolution
            .signals
            .remote
            .as_ref()
            .and_then(|remote| remote.slug.as_ref()),
        Some(&"burin-labs/harn".to_string())
    );
}

#[test]
fn context_profile_resolves_git_remote_from_subdirectory() {
    let dir = temp_dir("context-github-subdir");
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::create_dir_all(dir.path().join("crates/app/src")).unwrap();
    std::fs::write(
        dir.path().join(".git/config"),
        "[remote \"origin\"]\n\turl = https://github.com/burin-labs/harn.git\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("crates/app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let mut options = context_options_without_env();
    options.credentials = BTreeSet::from(["github".to_string()]);
    let resolution = resolve_context_profile(&dir.path().join("crates/app"), options);

    let profile_ids = context_profile_ids(&resolution);
    assert!(profile_ids.contains(&"git".to_string()));
    assert!(profile_ids.contains(&"github".to_string()));
    assert!(profile_ids.contains(&"rust".to_string()));
    assert_eq!(
        resolution
            .signals
            .remote
            .as_ref()
            .and_then(|remote| remote.slug.as_ref()),
        Some(&"burin-labs/harn".to_string())
    );
}

#[test]
fn context_profile_marks_github_preset_as_needing_credentials() {
    let dir = temp_dir("context-github-no-credentials");
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::write(
        dir.path().join(".git/config"),
        "[remote \"origin\"]\n\turl = git@github.com:burin-labs/harn.git\n",
    )
    .unwrap();

    let resolution = resolve_context_profile(dir.path(), context_options_without_env());
    let github = resolution
        .profiles
        .iter()
        .find(|profile| profile.id == "github")
        .expect("github profile");
    assert!(github.mcp_presets.is_empty());
    assert_eq!(github.mcp_preset_candidates[0].status, "needs_credentials");
    assert_eq!(
        github.mcp_preset_candidates[0].missing_credentials,
        vec!["GITHUB_PERSONAL_ACCESS_TOKEN".to_string()]
    );
}

#[test]
fn context_profile_does_not_treat_non_github_slug_as_github() {
    let dir = temp_dir("context-non-github-slug");
    let mut options = context_options_without_env();
    options.remote = Some(GitRemoteSignal {
        name: "origin".to_string(),
        host: "gitlab.com".to_string(),
        slug: Some("burin-labs/harn".to_string()),
        redacted_url: "https://gitlab.com/burin-labs/harn.git".to_string(),
    });

    let resolution = resolve_context_profile(dir.path(), options);

    assert_eq!(context_profile_ids(&resolution), vec!["git".to_string()]);
}

#[test]
fn context_profile_resolves_rust_crate_without_extra_profiles() {
    let dir = temp_dir("context-rust");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("Cargo.lock"), "# lock\n").unwrap();

    let resolution = resolve_context_profile(dir.path(), context_options_without_env());

    assert_eq!(context_profile_ids(&resolution), vec!["rust".to_string()]);
    assert_eq!(
        flatten_profile_field(&resolution, |profile| profile.tool_groups.clone()),
        vec!["cargo".to_string()]
    );
    assert_eq!(resolution.profiles[0].prompt_fragment.id, "profile:rust");
}

#[test]
fn context_profile_resolves_package_json_workspace() {
    let dir = temp_dir("context-node");
    std::fs::create_dir_all(dir.path().join("packages/web/src")).unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        "{\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"],\n  \"packageManager\": \"pnpm@9.0.0\"\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("packages/web/src/app.ts"),
        "export const app = 1;\n",
    )
    .unwrap();

    let resolution = resolve_context_profile(dir.path(), context_options_without_env());

    assert_eq!(context_profile_ids(&resolution), vec!["node".to_string()]);
    assert_eq!(
        flatten_profile_field(&resolution, |profile| profile.tool_groups.clone()),
        vec!["node".to_string()]
    );
    assert!(resolution.profiles[0].mcp_presets.is_empty());
}

#[test]
fn context_profile_bare_directory_has_no_active_profiles() {
    let dir = temp_dir("context-bare");
    let resolution = resolve_context_profile(dir.path(), context_options_without_env());

    assert!(resolution.profiles.is_empty());
    assert_eq!(resolution.activated_prompt_tokens, 0);
    assert!(resolution.always_on_prompt_tokens > 0);
}

#[test]
fn context_profile_consumes_supplied_code_librarian_signals_without_scanning() {
    let dir = temp_dir("context-supplied");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"local-rust\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let mut options = context_options_without_env();
    options.signal_source = Some("code_librarian".to_string());
    options.credentials = BTreeSet::from(["github".to_string()]);
    options.fingerprint = Some(ProjectFingerprint {
        primary_language: "python".to_string(),
        languages: vec!["python".to_string()],
        frameworks: Vec::new(),
        package_manager: Some("uv".to_string()),
        package_managers: vec!["uv".to_string()],
        test_runner: Some("pytest".to_string()),
        build_tool: None,
        vcs: Some("git".to_string()),
        ci: Vec::new(),
        has_tests: false,
        has_ci: false,
        lockfile_paths: Vec::new(),
    });
    options.remote = remote_signal_from_value(&VmValue::String(arcstr::ArcStr::from(
        "https://github.com/burin-labs/harn.git",
    )));

    let resolution = resolve_context_profile(dir.path(), options);

    assert_eq!(resolution.signals.source, "code_librarian");
    assert_eq!(
        context_profile_ids(&resolution),
        vec![
            "git".to_string(),
            "github".to_string(),
            "python".to_string()
        ]
    );
    assert_eq!(
        flatten_profile_field(&resolution, |profile| profile.mcp_presets.clone()),
        vec!["github".to_string()]
    );
}

#[test]
fn context_profile_scans_fingerprint_when_supplied_signals_only_include_remote() {
    let dir = temp_dir("context-remote-only");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let mut signals = BTreeMap::new();
    signals.insert(
        "remote".to_string(),
        VmValue::string("https://github.com/burin-labs/harn.git"),
    );
    let mut raw_options = BTreeMap::new();
    raw_options.insert("include_env_credentials".to_string(), VmValue::Bool(false));
    raw_options.insert("signals".to_string(), VmValue::dict(signals));
    let options = parse_context_profile_options(Some(&VmValue::dict(raw_options)));

    assert!(options.fingerprint.is_none());
    let resolution = resolve_context_profile(dir.path(), options);

    assert_eq!(
        context_profile_ids(&resolution),
        vec!["git".to_string(), "github".to_string(), "rust".to_string()]
    );
}

#[test]
fn context_profile_redacts_remote_userinfo_and_query() {
    assert_eq!(
        redact_remote_url("https://user:secret@github.com/burin-labs/harn.git?token=secret#frag"),
        "https://<redacted>@github.com/burin-labs/harn.git"
    );
    assert_eq!(
        github_slug_from_remote(
            "https://user:secret@github.com/burin-labs/harn.git?token=secret#frag"
        )
        .as_deref(),
        Some("burin-labs/harn")
    );
}

#[test]
fn scan_tree_respects_gitignore_and_vendor_dirs_by_default() {
    let dir = temp_dir("tree-ignore");
    std::fs::create_dir_all(dir.path().join("frontend/src")).unwrap();
    std::fs::create_dir_all(dir.path().join("ignored/src")).unwrap();
    std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
    std::fs::write(dir.path().join(".gitignore"), "ignored/\n").unwrap();
    std::fs::write(
        dir.path().join("frontend/package.json"),
        "{\"name\":\"frontend\"}",
    )
    .unwrap();
    std::fs::write(dir.path().join("frontend/package-lock.json"), "{}").unwrap();
    std::fs::write(
        dir.path().join("frontend/src/app.ts"),
        "export const x = 1;\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("ignored/go.mod"), "module ignored\n").unwrap();
    std::fs::write(
        dir.path().join("node_modules/pkg/package.json"),
        "{\"name\":\"pkg\"}",
    )
    .unwrap();

    let tree = scan_project_tree(dir.path(), &ProjectScanOptions::default()).unwrap();
    assert!(tree.contains_key("."));
    assert!(tree.contains_key("frontend"));
    assert!(!tree.contains_key("ignored"));
    assert!(!tree.contains_key("node_modules"));
}

#[test]
fn walk_tree_includes_all_directories_and_hashes_local_content_only() {
    let dir = temp_dir("walk");
    std::fs::create_dir_all(dir.path().join("src/auth")).unwrap();
    std::fs::create_dir_all(dir.path().join("src/api")).unwrap();
    std::fs::write(
        dir.path().join("src/auth/lib.rs"),
        "pub fn login() -> bool { true }\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("src/api/lib.rs"), "pub fn handle() {}\n").unwrap();

    let first = walk_project_tree(dir.path(), &ProjectScanOptions::default()).unwrap();
    let src = first
        .iter()
        .find(|entry| entry.relative_path == "src")
        .expect("src entry");
    let auth = first
        .iter()
        .find(|entry| entry.relative_path == "src/auth")
        .expect("auth entry");
    assert_eq!(
        first
            .iter()
            .map(|entry| entry.relative_path.as_str())
            .collect::<Vec<_>>(),
        vec![".", "src", "src/api", "src/auth"]
    );

    std::fs::write(
        dir.path().join("src/auth/lib.rs"),
        "pub fn login() -> bool { false }\n",
    )
    .unwrap();

    let second = walk_project_tree(dir.path(), &ProjectScanOptions::default()).unwrap();
    let src_after = second
        .iter()
        .find(|entry| entry.relative_path == "src")
        .expect("src entry");
    let auth_after = second
        .iter()
        .find(|entry| entry.relative_path == "src/auth")
        .expect("auth entry");

    assert_eq!(src.content_hash, src_after.content_hash);
    assert_ne!(auth.content_hash, auth_after.content_hash);
}

#[test]
fn project_fingerprint_detects_polyglot_repo_shape() {
    let dir = temp_dir("fingerprint-polyglot");
    std::fs::create_dir_all(dir.path().join("backend")).unwrap();
    std::fs::create_dir_all(dir.path().join("portal/tests")).unwrap();
    std::fs::create_dir_all(dir.path().join(".github/workflows")).unwrap();
    std::fs::write(
        dir.path().join("backend/Cargo.toml"),
        "[package]\nname = \"backend\"\nversion = \"0.1.0\"\n[dependencies]\naxum = \"0.8\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("backend/Cargo.lock"), "# lock\n").unwrap();
    std::fs::write(
        dir.path().join("portal/package.json"),
        "{\n  \"name\": \"portal\",\n  \"packageManager\": \"pnpm@9.0.0\",\n  \"dependencies\": {\n    \"next\": \"15.0.0\",\n    \"react\": \"19.0.0\"\n  }\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("portal/next.config.ts"),
        "export default {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("portal/pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join(".github/workflows/ci.yml"),
        "name: ci\non: push\n",
    )
    .unwrap();

    let fingerprint = detect_project_fingerprint(dir.path());
    assert_eq!(fingerprint.primary_language, "mixed");
    assert_eq!(
        fingerprint.languages,
        vec!["rust".to_string(), "typescript".to_string()]
    );
    assert_eq!(
        fingerprint.frameworks,
        vec!["axum".to_string(), "next".to_string(), "react".to_string()]
    );
    assert_eq!(
        fingerprint.package_managers,
        vec!["cargo".to_string(), "pnpm".to_string()]
    );
    assert_eq!(fingerprint.package_manager.as_deref(), Some("cargo"));
    assert_eq!(fingerprint.test_runner.as_deref(), Some("cargo-test"));
    assert_eq!(fingerprint.build_tool.as_deref(), Some("cargo"));
    assert_eq!(fingerprint.vcs, None);
    assert_eq!(fingerprint.ci, vec!["github-actions".to_string()]);
    assert!(fingerprint.has_tests);
    assert!(fingerprint.has_ci);
    assert_eq!(
        fingerprint.lockfile_paths,
        vec![
            "backend/Cargo.lock".to_string(),
            "portal/pnpm-lock.yaml".to_string()
        ]
    );
}

#[test]
fn project_fingerprint_detects_python_package_managers() {
    let dir = temp_dir("fingerprint-python");
    std::fs::write(
        dir.path().join("pyproject.toml"),
        "[project]\nname = \"api\"\ndependencies = [\"fastapi>=0.110\"]\n[tool.uv]\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("uv.lock"), "# lock\n").unwrap();
    std::fs::create_dir_all(dir.path().join("tests")).unwrap();

    let fingerprint = detect_project_fingerprint(dir.path());
    assert_eq!(fingerprint.primary_language, "python");
    assert_eq!(fingerprint.languages, vec!["python".to_string()]);
    assert!(fingerprint.frameworks.contains(&"fastapi".to_string()));
    assert_eq!(fingerprint.package_managers, vec!["uv".to_string()]);
    assert_eq!(fingerprint.package_manager.as_deref(), Some("uv"));
    assert_eq!(fingerprint.test_runner.as_deref(), Some("pytest"));
    assert_eq!(fingerprint.build_tool.as_deref(), Some("uv"));
    assert!(fingerprint.has_tests);
    assert!(!fingerprint.has_ci);
    assert_eq!(fingerprint.lockfile_paths, vec!["uv.lock".to_string()]);
}

#[test]
fn project_fingerprint_detects_rust_nextest_profile() {
    let dir = temp_dir("fingerprint-rust-nextest");
    std::fs::create_dir_all(dir.path().join(".config")).unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"runner\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join(".config/nextest.toml"),
        "[profile.default]\n",
    )
    .unwrap();

    let fingerprint = detect_project_fingerprint(dir.path());
    assert_eq!(fingerprint.primary_language, "rust");
    assert_eq!(fingerprint.package_manager.as_deref(), Some("cargo"));
    assert_eq!(fingerprint.test_runner.as_deref(), Some("nextest"));
    assert_eq!(fingerprint.build_tool.as_deref(), Some("cargo"));
    assert_eq!(fingerprint.vcs.as_deref(), Some("git"));
}

#[test]
fn project_fingerprint_detects_parent_vcs_from_subdirectory() {
    let dir = temp_dir("fingerprint-subdir-vcs");
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::create_dir_all(dir.path().join("crates/app/src")).unwrap();
    std::fs::write(
        dir.path().join("crates/app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let fingerprint = detect_project_fingerprint(&dir.path().join("crates/app"));
    assert_eq!(fingerprint.primary_language, "rust");
    assert_eq!(fingerprint.vcs.as_deref(), Some("git"));
}

#[test]
fn project_fingerprint_detects_scala_sbt_profile() {
    let dir = temp_dir("fingerprint-scala");
    std::fs::create_dir_all(dir.path().join("src/main/scala")).unwrap();
    std::fs::write(
        dir.path().join("build.sbt"),
        "name := \"app\"\nscalaVersion := \"3.3.1\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/main/scala/Main.scala"),
        "object Main { def main(args: Array[String]): Unit = () }\n",
    )
    .unwrap();

    let fingerprint = detect_project_fingerprint(dir.path());
    assert_eq!(fingerprint.primary_language, "scala");
    assert!(fingerprint.languages.contains(&"scala".to_string()));
    assert!(fingerprint.build_tool.as_deref() == Some("sbt"));
}

#[test]
fn project_fingerprint_detects_kotlin_gradle_kts_profile() {
    let dir = temp_dir("fingerprint-kotlin");
    std::fs::create_dir_all(dir.path().join("src/main/kotlin")).unwrap();
    std::fs::write(
        dir.path().join("build.gradle.kts"),
        "plugins { kotlin(\"jvm\") version \"2.0.0\" }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/main/kotlin/Main.kt"),
        "fun main() {}\n",
    )
    .unwrap();

    let fingerprint = detect_project_fingerprint(dir.path());
    assert_eq!(fingerprint.primary_language, "kotlin");
    assert!(fingerprint.languages.contains(&"kotlin".to_string()));
    assert!(fingerprint.build_tool.as_deref() == Some("gradle"));
}

#[test]
fn project_fingerprint_detects_elixir_mix_profile() {
    let dir = temp_dir("fingerprint-elixir");
    std::fs::create_dir_all(dir.path().join("lib")).unwrap();
    std::fs::write(
        dir.path().join("mix.exs"),
        "defmodule App.MixProject do\n  use Mix.Project\nend\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("lib/app.ex"), "defmodule App do\nend\n").unwrap();

    let fingerprint = detect_project_fingerprint(dir.path());
    assert_eq!(fingerprint.primary_language, "elixir");
    assert_eq!(fingerprint.package_manager.as_deref(), Some("mix"));
    assert_eq!(fingerprint.build_tool.as_deref(), Some("mix"));
}

#[test]
fn project_fingerprint_detects_zig_profile() {
    let dir = temp_dir("fingerprint-zig");
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("build.zig"),
        "const std = @import(\"std\");\npub fn build(b: *std.Build) void { _ = b; }\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("src/main.zig"), "pub fn main() void {}\n").unwrap();

    let fingerprint = detect_project_fingerprint(dir.path());
    assert_eq!(fingerprint.primary_language, "zig");
    assert_eq!(fingerprint.build_tool.as_deref(), Some("zig"));
}

#[test]
fn project_fingerprint_detects_java_maven_profile() {
    let dir = temp_dir("fingerprint-java");
    std::fs::create_dir_all(dir.path().join("src/main/java/app")).unwrap();
    std::fs::write(
        dir.path().join("pom.xml"),
        "<project><modelVersion>4.0.0</modelVersion></project>\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/main/java/app/App.java"),
        "package app;\npublic class App {}\n",
    )
    .unwrap();

    let fingerprint = detect_project_fingerprint(dir.path());
    assert_eq!(fingerprint.primary_language, "java");
    assert_eq!(fingerprint.build_tool.as_deref(), Some("maven"));
}

#[test]
fn project_fingerprint_detects_php_composer_profile() {
    let dir = temp_dir("fingerprint-php");
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("composer.json"),
        r#"{
  "name": "acme/app",
  "require": {
    "laravel/framework": "^12.0"
  },
  "require-dev": {
    "pestphp/pest": "^3.0",
    "phpunit/phpunit": "^11.0"
  },
  "scripts": {
    "test": "php artisan test"
  }
}
"#,
    )
    .unwrap();
    std::fs::write(dir.path().join("src/index.php"), "<?php\necho \"hi\";\n").unwrap();

    let fingerprint = detect_project_fingerprint(dir.path());
    assert_eq!(fingerprint.primary_language, "php");
    assert!(fingerprint.frameworks.contains(&"laravel".to_string()));
    assert_eq!(fingerprint.package_manager.as_deref(), Some("composer"));
    assert_eq!(fingerprint.test_runner.as_deref(), Some("pest"));
    assert_eq!(fingerprint.build_tool.as_deref(), Some("composer"));
    assert!(fingerprint.has_tests);
}

#[test]
fn project_fingerprint_detects_csharp_profile() {
    let dir = temp_dir("fingerprint-csharp");
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/Program.cs"),
        "class Program { static void Main() {} }\n",
    )
    .unwrap();

    let fingerprint = detect_project_fingerprint(dir.path());
    assert_eq!(fingerprint.primary_language, "csharp");
}

#[test]
fn project_fingerprint_detects_cpp_profile() {
    let dir = temp_dir("fingerprint-cpp");
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/main.cpp"),
        "int main() { return 0; }\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("src/util.hpp"), "#pragma once\n").unwrap();

    let fingerprint = detect_project_fingerprint(dir.path());
    assert_eq!(fingerprint.primary_language, "cpp");
}

#[test]
fn project_fingerprint_ranks_source_languages_before_tooling_hints() {
    let php_dir = temp_dir("fingerprint-php-with-frontend-tooling");
    std::fs::create_dir_all(php_dir.path().join("app")).unwrap();
    std::fs::write(
        php_dir.path().join("composer.json"),
        r#"{"require":{"laravel/framework":"^12.0"}}"#,
    )
    .unwrap();
    std::fs::write(
        php_dir.path().join("package.json"),
        r#"{"dependencies":{"react":"19.0.0","typescript":"^5.0.0","vite":"^6.0.0"}}"#,
    )
    .unwrap();
    std::fs::write(php_dir.path().join("vite.config.js"), "export default {}\n").unwrap();
    std::fs::write(php_dir.path().join("app/Controller.php"), "<?php\n").unwrap();

    let php = detect_project_fingerprint(php_dir.path());
    assert_eq!(php.primary_language, "php");
    assert_eq!(php.languages.first().map(String::as_str), Some("php"));
    assert!(php.languages.contains(&"typescript".to_string()));

    let java_dir = temp_dir("fingerprint-java-with-kotlin-build");
    std::fs::create_dir_all(java_dir.path().join("src/main/java/app")).unwrap();
    std::fs::write(
        java_dir.path().join("build.gradle.kts"),
        "plugins { java }\n",
    )
    .unwrap();
    std::fs::write(
        java_dir.path().join("src/main/java/app/App.java"),
        "package app;\npublic class App {}\n",
    )
    .unwrap();

    let java = detect_project_fingerprint(java_dir.path());
    assert_eq!(java.primary_language, "java");
    assert_eq!(java.languages.first().map(String::as_str), Some("java"));
    assert!(java.languages.contains(&"kotlin".to_string()));

    let cpp_dir = temp_dir("fingerprint-cpp-with-c-header");
    std::fs::create_dir_all(cpp_dir.path().join("src")).unwrap();
    std::fs::write(
        cpp_dir.path().join("src/lib.cpp"),
        "int lib() { return 1; }\n",
    )
    .unwrap();
    std::fs::write(cpp_dir.path().join("src/lib.h"), "int lib();\n").unwrap();

    let cpp = detect_project_fingerprint(cpp_dir.path());
    assert_eq!(cpp.primary_language, "cpp");
    assert_eq!(cpp.languages.first().map(String::as_str), Some("cpp"));
    assert!(cpp.languages.contains(&"c".to_string()));

    let ruby_dir = temp_dir("fingerprint-ruby-with-frontend-tooling");
    std::fs::create_dir_all(ruby_dir.path().join("app/models")).unwrap();
    std::fs::write(ruby_dir.path().join("Gemfile"), "gem \"rails\"\n").unwrap();
    std::fs::write(
        ruby_dir.path().join("package.json"),
        r#"{"dependencies":{"react":"19.0.0","typescript":"^5.0.0"}}"#,
    )
    .unwrap();
    std::fs::write(
        ruby_dir.path().join("app/models/user.rb"),
        "class User\nend\n",
    )
    .unwrap();

    let ruby = detect_project_fingerprint(ruby_dir.path());
    assert_eq!(ruby.primary_language, "ruby");
    assert_eq!(ruby.languages.first().map(String::as_str), Some("ruby"));
    assert!(ruby.languages.contains(&"typescript".to_string()));
}

#[test]
fn project_fingerprint_detects_swift_spm_profile() {
    let dir = temp_dir("fingerprint-swift");
    std::fs::create_dir_all(dir.path().join("Sources/App")).unwrap();
    std::fs::create_dir_all(dir.path().join("Tests/AppTests")).unwrap();
    std::fs::write(
        dir.path().join("Package.swift"),
        "import PackageDescription\nlet package = Package(name: \"App\")\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("Sources/App/main.swift"),
        "__io_print(\"hi\")\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("Tests/AppTests/AppTests.swift"),
        "import XCTest\n",
    )
    .unwrap();

    let fingerprint = detect_project_fingerprint(dir.path());
    assert_eq!(fingerprint.primary_language, "swift");
    assert_eq!(fingerprint.package_manager.as_deref(), Some("spm"));
    assert_eq!(fingerprint.test_runner.as_deref(), Some("xctest"));
    assert_eq!(fingerprint.build_tool.as_deref(), Some("spm"));
    assert!(fingerprint.has_tests);
}

#[test]
fn project_fingerprint_detects_npm_workspace_profile() {
    let dir = temp_dir("fingerprint-npm");
    std::fs::create_dir_all(dir.path().join("packages/web/src")).unwrap();
    std::fs::create_dir_all(dir.path().join("packages/web/tests")).unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        "{\n  \"name\": \"workspace\",\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"],\n  \"packageManager\": \"npm@10.8.0\",\n  \"devDependencies\": {\n    \"vite\": \"5.0.0\",\n    \"vitest\": \"2.0.0\"\n  },\n  \"scripts\": {\n    \"build\": \"vite build\",\n    \"test\": \"vitest run\"\n  }\n}\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("package-lock.json"), "{}\n").unwrap();
    std::fs::write(
        dir.path().join("packages/web/src/app.ts"),
        "export const app = 1;\n",
    )
    .unwrap();

    let fingerprint = detect_project_fingerprint(dir.path());
    assert_eq!(fingerprint.primary_language, "typescript");
    assert_eq!(fingerprint.package_manager.as_deref(), Some("npm"));
    assert_eq!(fingerprint.test_runner.as_deref(), Some("vitest"));
    assert_eq!(fingerprint.build_tool.as_deref(), Some("vite"));
    assert!(fingerprint.has_tests);
}

#[test]
fn project_fingerprint_detects_poetry_profile() {
    let dir = temp_dir("fingerprint-poetry");
    std::fs::create_dir_all(dir.path().join("tests")).unwrap();
    std::fs::write(
        dir.path().join("pyproject.toml"),
        "[tool.poetry]\nname = \"svc\"\nversion = \"0.1.0\"\n[tool.poetry.dependencies]\npython = \"^3.12\"\nfastapi = \"^0.110\"\n[tool.poetry.group.dev.dependencies]\npytest = \"^8.0\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("poetry.lock"), "# lock\n").unwrap();

    let fingerprint = detect_project_fingerprint(dir.path());
    assert_eq!(fingerprint.package_manager.as_deref(), Some("poetry"));
    assert_eq!(fingerprint.test_runner.as_deref(), Some("pytest"));
    assert_eq!(fingerprint.build_tool.as_deref(), Some("poetry"));
    assert!(fingerprint.frameworks.contains(&"fastapi".to_string()));
}

#[test]
fn project_fingerprint_detects_go_module_profile() {
    let dir = temp_dir("fingerprint-go");
    std::fs::create_dir_all(dir.path().join("pkg")).unwrap();
    std::fs::write(
        dir.path().join("go.mod"),
        "module github.com/acme/service\n\ngo 1.23\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("go.sum"), "example v0.0.0 h1:abc\n").unwrap();
    std::fs::write(dir.path().join("pkg/service.go"), "package pkg\n").unwrap();

    let fingerprint = detect_project_fingerprint(dir.path());
    assert_eq!(fingerprint.primary_language, "go");
    assert_eq!(fingerprint.package_manager.as_deref(), Some("go-mod"));
    assert_eq!(fingerprint.test_runner.as_deref(), Some("go-test"));
    assert_eq!(fingerprint.build_tool.as_deref(), Some("go"));
}

#[test]
fn project_fingerprint_handles_empty_directory() {
    let dir = temp_dir("fingerprint-empty");
    let fingerprint = detect_project_fingerprint(dir.path());
    assert_eq!(fingerprint.primary_language, "unknown");
    assert!(fingerprint.languages.is_empty());
    assert!(fingerprint.frameworks.is_empty());
    assert!(fingerprint.package_managers.is_empty());
    assert_eq!(fingerprint.package_manager, None);
    assert_eq!(fingerprint.test_runner, None);
    assert_eq!(fingerprint.build_tool, None);
    assert_eq!(fingerprint.vcs, None);
    assert!(fingerprint.ci.is_empty());
    assert!(!fingerprint.has_tests);
    assert!(!fingerprint.has_ci);
    assert!(fingerprint.lockfile_paths.is_empty());
}

#[test]
fn scan_path_resolution_rejects_a_missing_path_instead_of_scanning_its_parent() {
    let dir = temp_dir("missing-path");
    let missing = dir.path().join("no-such-subdir");

    let error = super::builtins::resolve_existing_directory(&missing.display().to_string())
        .expect_err("a path that does not exist names no directory to scan");

    // Falling back to the parent silently answers a question the caller never
    // asked: `project_fingerprint("/tmp/nonexistent")` reported all of `/tmp`.
    match error {
        VmError::Thrown(VmValue::String(message)) => {
            assert!(
                message.contains("does not exist"),
                "the error must name the missing path, got: {message}"
            );
            assert!(
                message.contains("no-such-subdir"),
                "the error must name the path the caller passed, not its parent, got: {message}"
            );
        }
        other => panic!("expected a thrown path error, got: {other:?}"),
    }
}

#[test]
fn scan_path_resolution_maps_a_file_to_its_directory() {
    let dir = temp_dir("file-path");
    let file = dir.path().join("main.rs");
    std::fs::write(&file, "fn main() {}\n").expect("write");

    let resolved = super::builtins::resolve_existing_directory(&file.display().to_string())
        .expect("a file names the directory that contains it");

    assert_eq!(
        resolved,
        dir.path().canonicalize().expect("canonicalize"),
        "a caller holding a file path scans the directory it lives in"
    );
}

#[test]
fn scan_path_resolution_accepts_a_directory_unchanged() {
    let dir = temp_dir("dir-path");

    let resolved = super::builtins::resolve_existing_directory(&dir.path().display().to_string())
        .expect("an existing directory resolves to itself");

    assert_eq!(resolved, dir.path().canonicalize().expect("canonicalize"));
}
