//! Test-command + code-pattern detection so scanner metadata can power
//! downstream "run tests" affordances without asking every host to
//! rediscover package manifests.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::scanner::extensions::should_include;
use crate::scanner::result::FileRecord;

/// Walk the project root looking for test/build manifest files and return a
/// `command -> human label` map.
pub fn detect_test_commands(root: &Path) -> BTreeMap<String, String> {
    let mut commands: BTreeMap<String, String> = BTreeMap::new();

    scan_package_json(root, None, &mut commands);
    scan_monorepo_packages(root, &mut commands);
    detect_language_test_commands(root, &mut commands);
    detect_makefile_targets(root, &mut commands);

    commands
}

fn scan_monorepo_packages(root: &Path, commands: &mut BTreeMap<String, String>) {
    for subdir in ["packages", "apps", "services"] {
        let dir = root.join(subdir);
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue,
            };
            if path.is_dir() {
                scan_package_json(&path, Some(format!("{subdir}/{name}")), commands);
            }
        }
    }
}

fn detect_language_test_commands(root: &Path, commands: &mut BTreeMap<String, String>) {
    const MARKERS: &[(&str, &str, &str)] = &[
        ("Cargo.toml", "cargo test", "Rust test runner"),
        ("go.mod", "go test ./...", "Go test runner"),
        ("Package.swift", "swift test", "Swift test runner"),
        ("build.sbt", "sbt test", "Scala test runner"),
        ("composer.json", "./vendor/bin/phpunit", "PHP test runner"),
        (
            "build.gradle.kts",
            "./gradlew test --info",
            "Gradle test runner",
        ),
        (
            "build.gradle",
            "./gradlew test --info",
            "Gradle test runner",
        ),
        (
            "settings.gradle.kts",
            "./gradlew test --info",
            "Gradle test runner",
        ),
        (
            "settings.gradle",
            "./gradlew test --info",
            "Gradle test runner",
        ),
        ("pom.xml", "mvn test", "Maven test runner"),
        ("CMakeLists.txt", "make test", "CMake test runner"),
    ];
    for (marker, command, label) in MARKERS {
        if root.join(marker).exists() {
            commands.insert((*command).to_string(), (*label).to_string());
        }
    }
    detect_python_test_command(root, commands);
    detect_dart_test_command(root, commands);
    detect_dotnet_test_command(root, commands);
    detect_ruby_test_command(root, commands);
    detect_php_test_command(root, commands);
}

fn detect_python_test_command(root: &Path, commands: &mut BTreeMap<String, String>) {
    let pyproject = root.join("pyproject.toml");
    let uv_lock = root.join("uv.lock");
    let poetry_lock = root.join("poetry.lock");
    let (preferred_command, preferred_label) = if pyproject.exists() && uv_lock.exists() {
        ("uv run --with pytest pytest", "Python test runner (uv)")
    } else if pyproject.exists() && poetry_lock.exists() {
        ("poetry run pytest", "Python test runner (poetry)")
    } else {
        ("python -m pytest", "Python test runner")
    };

    let walker = walkdir::WalkDir::new(root).follow_links(false);
    for entry in walker.into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = match entry.path().strip_prefix(root) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let candidate = match rel.to_str() {
            Some(s) => s,
            None => continue,
        };
        if !should_include(candidate) || !candidate.ends_with(".py") {
            continue;
        }
        let lower = candidate.to_ascii_lowercase();
        if !(lower.contains("/tests/")
            || lower.starts_with("tests/")
            || lower.contains("/test_")
            || lower.ends_with("_test.py")
            || lower.contains("conftest.py"))
        {
            continue;
        }
        if pyproject.exists() {
            commands.insert(preferred_command.to_string(), preferred_label.to_string());
            return;
        }
        if let Ok(content) = fs::read_to_string(entry.path()) {
            if content.contains("import pytest") || content.contains("from pytest") {
                commands.insert(preferred_command.to_string(), preferred_label.to_string());
                return;
            }
        }
    }
}

fn detect_dart_test_command(root: &Path, commands: &mut BTreeMap<String, String>) {
    let pubspec = root.join("pubspec.yaml");
    let content = match fs::read_to_string(&pubspec) {
        Ok(c) => c,
        Err(_) => return,
    };
    if content.contains("flutter") {
        commands.insert(
            "flutter test".to_string(),
            "Flutter test runner".to_string(),
        );
    } else {
        commands.insert("dart test".to_string(), "Dart test runner".to_string());
    }
}

fn detect_dotnet_test_command(root: &Path, commands: &mut BTreeMap<String, String>) {
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            if name.ends_with(".sln") || name.ends_with(".slnx") || name.ends_with(".csproj") {
                commands.insert("dotnet test".to_string(), ".NET test runner".to_string());
                return;
            }
        }
    }
}

fn detect_ruby_test_command(root: &Path, commands: &mut BTreeMap<String, String>) {
    let gemfile = root.join("Gemfile");
    if let Ok(content) = fs::read_to_string(gemfile) {
        if content.contains("rspec") {
            commands.insert(
                "bundle exec rspec".to_string(),
                "Ruby RSpec test runner".to_string(),
            );
        }
    }
}

fn detect_php_test_command(root: &Path, commands: &mut BTreeMap<String, String>) {
    let composer = root.join("composer.json");
    if !composer.exists() {
        return;
    }
    let pest = root.join("vendor/bin/pest");
    if pest.exists() {
        commands.insert(
            "./vendor/bin/pest".to_string(),
            "PHP Pest test runner".to_string(),
        );
        commands.remove("./vendor/bin/phpunit");
    }
}

fn detect_makefile_targets(root: &Path, commands: &mut BTreeMap<String, String>) {
    let makefile = root.join("Makefile");
    let content = match fs::read_to_string(makefile) {
        Ok(c) => c,
        Err(_) => return,
    };
    for line in content.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("test") {
            continue;
        }
        let trimmed = trimmed.trim();
        if let Some(colon_idx) = trimmed.find(':') {
            let target = trimmed[..colon_idx].trim();
            if !target.is_empty() && !target.contains(' ') {
                commands.insert(format!("make {target}"), "Makefile target".to_string());
            }
        }
    }
}

fn scan_package_json(dir: &Path, prefix: Option<String>, commands: &mut BTreeMap<String, String>) {
    let path = dir.join("package.json");
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let json: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return,
    };
    let scripts = match json.get("scripts").and_then(|v| v.as_object()) {
        Some(m) => m,
        None => return,
    };

    let pkg_name = prefix
        .clone()
        .or_else(|| {
            json.get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default();
    let label_suffix = if pkg_name.is_empty() {
        String::new()
    } else {
        format!(" ({pkg_name})")
    };

    for (name, value) in scripts {
        if !(name.contains("test")
            || name.contains("lint")
            || name.contains("typecheck")
            || name == "check")
        {
            continue;
        }
        let runner = match prefix.as_deref() {
            Some(p) => format!("cd {p} && pnpm {name}"),
            None => format!("pnpm {name}"),
        };
        let cmd_text = value.as_str().unwrap_or_default();
        commands.insert(runner, format!("{cmd_text}{label_suffix}"));
    }
}

// MARK: - Code pattern hints

/// Detect ORM/test/auth/middleware patterns that help the agent write
/// correct code. Best-effort.
pub fn detect_code_patterns(files: &[FileRecord], root: &Path) -> Vec<String> {
    let mut patterns: Vec<String> = Vec::new();
    let helper_candidates = collect_helper_candidates(files);

    for file in helper_candidates {
        let lower = file.relative_path.to_ascii_lowercase();
        if lower.contains(".test.")
            || lower.contains(".spec.")
            || lower.contains("__tests__")
            || lower.contains("_test.")
        {
            continue;
        }
        let abs = root.join(&file.relative_path);
        let content = match fs::read_to_string(&abs) {
            Ok(c) => c,
            Err(_) => continue,
        };

        if content.contains("paginatedResponse")
            || content.contains("paginated_response")
            || content.contains("paginateResponse")
            || content.contains("buildPaginatedResponse")
        {
            if let Some(snippet) = extract_paginated_response_snippet(&content) {
                patterns.push(format!(
                    "API response format ({}):\n{snippet}",
                    file.relative_path
                ));
            }
        }

        if content.contains("z.object")
            && (content.contains("Schema") || content.contains("schema"))
        {
            patterns.push(format!(
                "Uses Zod validation schemas in {}",
                file.relative_path
            ));
        }

        if content.contains("asyncHandler") || content.contains("async_handler") {
            patterns.push(format!(
                "Routes use asyncHandler wrapper in {}",
                file.relative_path
            ));
        }

        if content.contains("isAuthenticated")
            || content.contains("is_authenticated")
            || content.contains("requireAuth")
        {
            patterns.push(format!("Auth middleware: {}", file.relative_path));
        }
    }

    let prisma = root.join("prisma/schema.prisma");
    if prisma.exists()
        || files
            .iter()
            .any(|f| f.relative_path.contains("prisma/schema"))
    {
        patterns.push("Database: Prisma ORM with schema at prisma/schema.prisma".to_string());
    }
    if files.iter().any(|f| f.relative_path.contains("drizzle")) {
        patterns.push("Database: Drizzle ORM".to_string());
    }
    if files
        .iter()
        .any(|f| f.relative_path.ends_with("models.py") || f.relative_path.contains("sqlalchemy"))
    {
        patterns.push("Database: SQLAlchemy / Django ORM".to_string());
    }
    if files.iter().any(|f| {
        f.relative_path.contains("test/integration-helpers")
            || f.relative_path.contains("test/helpers")
    }) {
        patterns.push(
            "Test helpers available — check test/integration-helpers or test/helpers before \
             writing tests"
                .to_string(),
        );
    }

    patterns
}

fn collect_helper_candidates(files: &[FileRecord]) -> Vec<&FileRecord> {
    let mut candidates: Vec<&FileRecord> = files
        .iter()
        .filter(|f| {
            let lower = f.relative_path.to_ascii_lowercase();
            let ext = f.language.as_str();
            let is_source = matches!(
                ext,
                "ts" | "js" | "py" | "go" | "rs" | "swift" | "java" | "kt"
            );
            if !is_source {
                return false;
            }
            lower.contains("helper")
                || lower.contains("util")
                || lower.contains("middleware")
                || lower.contains("route-helper")
                || lower.contains("/lib/")
                || lower.contains("/utils/")
                || lower.contains("/common/")
                || lower.contains("/shared/")
        })
        .collect();
    candidates.sort_by(|a, b| {
        let a_server = a.relative_path.contains("server")
            || a.relative_path.contains("api")
            || a.relative_path.contains("backend");
        let b_server = b.relative_path.contains("server")
            || b.relative_path.contains("api")
            || b.relative_path.contains("backend");
        if a_server != b_server {
            return b_server.cmp(&a_server);
        }
        a.relative_path.len().cmp(&b.relative_path.len())
    });
    candidates.into_iter().take(40).collect()
}

fn extract_paginated_response_snippet(content: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let is_def = trimmed.contains("function paginatedResponse")
            || trimmed.contains("function paginated_response")
            || trimmed.contains("def paginatedResponse")
            || trimmed.contains("def paginated_response")
            || trimmed.contains("func paginatedResponse");
        if !is_def {
            continue;
        }
        let end = (i + 15).min(lines.len().saturating_sub(1));
        let snippet = lines[i..=end].join("\n").trim().to_string();
        if snippet.len() < 600 {
            return Some(snippet);
        }
    }
    None
}

// MARK: - Preferred test command selection (matches `TestPatternResolver`).

/// Pick the most-actionable test command from a project's
/// `(command -> label)` map. Returns `None` if no shell-like command was
/// found.
pub fn select_preferred_test_command(commands: &BTreeMap<String, String>) -> Option<String> {
    preferred_test_commands(commands).into_iter().next()
}

fn preferred_test_commands(commands: &BTreeMap<String, String>) -> Vec<String> {
    let mut normalized = normalized_test_commands(commands);
    let mut keys: Vec<String> = normalized.keys().cloned().collect();
    keys.sort_by(|a, b| {
        let la = preference_score(a);
        let lb = preference_score(b);
        if la != lb {
            return lb.cmp(&la);
        }
        a.cmp(b)
    });
    normalized.clear();
    keys
}

fn normalized_test_commands(commands: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    if commands.is_empty() {
        return BTreeMap::new();
    }
    let mut out = BTreeMap::new();
    for (key, value) in commands {
        if looks_like_shell_command(key) {
            out.insert(key.clone(), value.clone());
            continue;
        }
        if looks_like_shell_command(value) {
            out.insert(value.clone(), key.clone());
        }
    }
    out
}

fn preference_score(command: &str) -> i32 {
    let lower = command.to_ascii_lowercase();
    let mut score = 0;
    if lower.contains("integration") || lower.contains("e2e") {
        score -= 100;
    }
    if lower.starts_with("uv run ") {
        score += 30;
    }
    if lower.contains("&&") || lower.contains("||") || lower.contains(';') {
        score -= 10;
    }
    if lower.contains(" test ") || lower.ends_with(" test") || lower.starts_with("test ") {
        score += 5;
    }
    if lower.contains("./") || lower.contains('/') || lower.contains('*') {
        score += 2;
    }
    let words = lower.split_whitespace().count() as i32;
    score - words
}

fn looks_like_shell_command(candidate: &str) -> bool {
    let trimmed = candidate.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.starts_with("./") || trimmed.starts_with("cd ") {
        return true;
    }
    if trimmed.contains(' ') || trimmed.contains('/') {
        return true;
    }
    let first = trimmed.split_whitespace().next().unwrap_or(trimmed);
    matches!(
        first,
        "make"
            | "cargo"
            | "go"
            | "swift"
            | "sbt"
            | "mvn"
            | "gradle"
            | "pnpm"
            | "npm"
            | "yarn"
            | "bun"
            | "uv"
            | "poetry"
            | "pytest"
            | "python"
            | "ruby"
            | "rspec"
            | "bundle"
            | "dotnet"
            | "dart"
            | "flutter"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn detects_cargo_via_marker() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let cmds = detect_test_commands(tmp.path());
        assert!(cmds.contains_key("cargo test"));
    }

    #[test]
    fn prefers_uv_over_plain_test() {
        let mut cmds = BTreeMap::new();
        cmds.insert("cargo test".to_string(), "label".to_string());
        cmds.insert(
            "uv run --with pytest pytest".to_string(),
            "label".to_string(),
        );
        let preferred = select_preferred_test_command(&cmds).unwrap();
        assert_eq!(preferred, "uv run --with pytest pytest");
    }

    #[test]
    fn pnpm_scripts_are_normalized() {
        let tmp = tempdir().unwrap();
        let pkg = serde_json::json!({
            "name": "x",
            "scripts": {
                "test": "vitest run",
                "lint": "eslint .",
                "build": "tsc -b"
            }
        });
        fs::write(
            tmp.path().join("package.json"),
            serde_json::to_string_pretty(&pkg).unwrap(),
        )
        .unwrap();
        let mut cmds = BTreeMap::new();
        scan_package_json(tmp.path(), None, &mut cmds);
        assert!(cmds.contains_key("pnpm test"));
        assert!(cmds.contains_key("pnpm lint"));
        assert!(!cmds.contains_key("pnpm build"));
    }
}
