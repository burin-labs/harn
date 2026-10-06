//! Project fingerprint detection: languages, frameworks, package managers, test runners, and CI.

use super::*;

pub(in super::super) fn detect_project_fingerprint(dir: &Path) -> ProjectFingerprint {
    let mut signals = FingerprintSignals::default();
    walk_project_fingerprint(dir, dir, 0, &mut signals);

    if signals.has_next_dep && signals.has_next_config {
        signals.frameworks.insert("next".to_string());
        signals.add_language_signal("typescript");
        signals.build_tools.insert("next".to_string());
    }
    if signals.node_project
        && !signals.package_managers.contains("npm")
        && !signals.package_managers.contains("pnpm")
        && !signals.package_managers.contains("yarn")
    {
        signals.package_managers.insert("npm".to_string());
    }
    if signals.python_project
        && !signals.package_managers.contains("poetry")
        && !signals.package_managers.contains("uv")
        && signals.python_needs_pip
    {
        signals.package_managers.insert("pip".to_string());
    }
    if signals.languages.contains("rust") {
        signals.build_tools.insert("cargo".to_string());
        signals.test_runners.insert("cargo-test".to_string());
    }
    if signals.languages.contains("go") {
        signals.package_managers.insert("go-mod".to_string());
        signals.build_tools.insert("go".to_string());
        signals.test_runners.insert("go-test".to_string());
    }
    if signals.languages.contains("swift") {
        signals.package_managers.insert("spm".to_string());
        signals.build_tools.insert("spm".to_string());
        signals.test_runners.insert("xctest".to_string());
    }
    if signals.python_project && signals.test_runners.is_empty() {
        signals.test_runners.insert("pytest".to_string());
    }
    if signals.ruby_project {
        signals.package_managers.insert("bundler".to_string());
        signals.build_tools.insert("bundler".to_string());
        if signals.test_runners.is_empty() {
            if signals.has_spec_dir {
                signals.test_runners.insert("rspec".to_string());
            } else if signals.has_test_dir {
                signals.test_runners.insert("minitest".to_string());
            }
        }
    }
    if signals.has_vite_dep || signals.has_vite_config {
        signals.build_tools.insert("vite".to_string());
        if signals.has_tests && signals.test_runners.is_empty() {
            signals.test_runners.insert("vitest".to_string());
        }
    }
    if signals.node_project && signals.build_tools.is_empty() {
        if signals.package_managers.contains("pnpm") {
            signals.build_tools.insert("pnpm".to_string());
        } else if signals.package_managers.contains("yarn") {
            signals.build_tools.insert("yarn".to_string());
        } else if signals.package_managers.contains("npm") {
            signals.build_tools.insert("npm".to_string());
        }
    }
    if signals.python_project && signals.build_tools.is_empty() {
        if signals.package_managers.contains("uv") {
            signals.build_tools.insert("uv".to_string());
        } else if signals.package_managers.contains("poetry") {
            signals.build_tools.insert("poetry".to_string());
        } else if signals.package_managers.contains("pip") {
            signals.build_tools.insert("pip".to_string());
        }
    }
    if signals.php_project {
        signals.package_managers.insert("composer".to_string());
        signals.build_tools.insert("composer".to_string());
        if signals.has_tests && signals.test_runners.is_empty() {
            signals.test_runners.insert("phpunit".to_string());
        }
    }

    let languages = ranked_project_languages(&signals);
    let frameworks = ordered_values(&signals.frameworks, PROJECT_FRAMEWORK_ORDER);
    let package_managers = ordered_values(&signals.package_managers, PROJECT_PACKAGE_MANAGER_ORDER);
    let test_runners = ordered_values(&signals.test_runners, PROJECT_TEST_RUNNER_ORDER);
    let build_tools = ordered_values(&signals.build_tools, PROJECT_BUILD_TOOL_ORDER);
    let ci = ordered_values(&signals.ci, PROJECT_CI_ORDER);
    let primary_language = primary_project_language(&languages, &signals);

    ProjectFingerprint {
        primary_language,
        languages,
        frameworks,
        package_manager: first_ordered_value(&package_managers),
        package_managers,
        test_runner: first_ordered_value(&test_runners),
        build_tool: first_ordered_value(&build_tools),
        vcs: detect_vcs(dir),
        ci: ci.clone(),
        has_tests: signals.has_tests,
        has_ci: !ci.is_empty(),
        lockfile_paths: signals.lockfile_paths.into_iter().collect(),
    }
}

pub(super) fn walk_project_fingerprint(
    base: &Path,
    dir: &Path,
    depth: usize,
    signals: &mut FingerprintSignals,
) {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries = read_dir.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let rel = relative_posix(base, &path);

        if file_type.is_dir() {
            inspect_fingerprint_dir(&rel, &name, signals);
            if depth < PROJECT_FINGERPRINT_MAX_DEPTH
                && !BUILTIN_IGNORED_DIRS.contains(&name.as_str())
            {
                walk_project_fingerprint(base, &path, depth + 1, signals);
            }
            continue;
        }

        if file_type.is_file() {
            inspect_fingerprint_file(&path, &rel, &name, signals);
        }
    }
}

pub(super) fn inspect_fingerprint_dir(rel: &str, name: &str, signals: &mut FingerprintSignals) {
    let lower_name = name.to_ascii_lowercase();
    if TEST_DIR_NAMES.contains(&lower_name.as_str()) {
        signals.has_tests = true;
        if lower_name == "spec" {
            signals.has_spec_dir = true;
        }
        if lower_name == "test" || lower_name == "tests" {
            signals.has_test_dir = true;
        }
    }
    if rel == ".github/workflows" || rel.ends_with("/.github/workflows") {
        signals.ci.insert("github-actions".to_string());
    }
    if name == ".circleci" {
        signals.ci.insert("circleci".to_string());
    }
    if name == ".buildkite" {
        signals.ci.insert("buildkite".to_string());
    }
    match name {
        "crates" => {
            signals.add_language_signal("rust");
        }
        "cmd" | "pkg" => {
            signals.add_language_signal("go");
        }
        ".git" => {}
        ".hg" => {}
        _ => {}
    }
}

pub(super) fn inspect_fingerprint_file(
    path: &Path,
    rel: &str,
    name: &str,
    signals: &mut FingerprintSignals,
) {
    if let Some((_lockfile, manager)) = PROJECT_LOCKFILES
        .iter()
        .find(|(lockfile, _manager)| *lockfile == name)
    {
        signals.lockfile_paths.insert(rel.to_string());
        if let Some(manager) = manager {
            signals.package_managers.insert((*manager).to_string());
        }
    }
    if rel.starts_with(".github/workflows/") || rel == ".github/workflows" {
        signals.ci.insert("github-actions".to_string());
    }
    if CI_FILE_NAMES.contains(&name) {
        match name {
            ".gitlab-ci.yml" => {
                signals.ci.insert("gitlab-ci".to_string());
            }
            "azure-pipelines.yml" => {
                signals.ci.insert("azure-pipelines".to_string());
            }
            "bitrise.yml" => {
                signals.ci.insert("bitrise".to_string());
            }
            "circle.yml" => {
                signals.ci.insert("circleci".to_string());
            }
            _ => {}
        }
    }

    match name {
        "Cargo.toml" => inspect_cargo_manifest(path, signals),
        "package.json" => inspect_package_json(path, signals),
        "pyproject.toml" => inspect_pyproject(path, signals),
        "requirements.txt" | "requirements-dev.txt" | "requirements-test.txt" => {
            inspect_python_requirements(path, signals);
        }
        "setup.py" => {
            signals.add_language_signal("python");
            signals.python_project = true;
            signals.python_needs_pip = true;
            inspect_python_text(read_text_if_exists(path.to_path_buf()).as_deref(), signals);
        }
        "go.mod" => {
            signals.add_language_signal("go");
            signals.package_managers.insert("go-mod".to_string());
            signals.build_tools.insert("go".to_string());
            signals.test_runners.insert("go-test".to_string());
        }
        "Package.swift" => {
            signals.add_language_signal("swift");
            signals.package_managers.insert("spm".to_string());
            signals.build_tools.insert("spm".to_string());
            signals.test_runners.insert("xctest".to_string());
        }
        "Gemfile" => inspect_gemfile(path, signals),
        "build.sbt" => {
            signals.add_language_signal("scala");
            signals.build_tools.insert("sbt".to_string());
        }
        "build.gradle.kts" => {
            signals.add_language_signal("kotlin");
            signals.build_tools.insert("gradle".to_string());
        }
        "mix.exs" => {
            signals.add_language_signal("elixir");
            signals.package_managers.insert("mix".to_string());
            signals.build_tools.insert("mix".to_string());
        }
        "pom.xml" => {
            signals.add_language_signal("java");
            signals.build_tools.insert("maven".to_string());
        }
        "composer.json" => inspect_composer_json(path, signals),
        "build.zig" | "build.zig.zon" => {
            signals.add_language_signal("zig");
            signals.build_tools.insert("zig".to_string());
        }
        _ => {}
    }

    if NEXT_CONFIG_NAMES.contains(&name) {
        signals.has_next_config = true;
        signals.node_project = true;
        signals.add_language_signal("typescript");
        signals.build_tools.insert("next".to_string());
    }
    if VITEST_CONFIG_NAMES.contains(&name) {
        signals.node_project = true;
        signals.add_language_signal("typescript");
        signals.test_runners.insert("vitest".to_string());
        signals.has_tests = true;
    }
    if JEST_CONFIG_NAMES.contains(&name) {
        signals.node_project = true;
        signals.add_language_signal("typescript");
        signals.test_runners.insert("jest".to_string());
        signals.has_tests = true;
    }
    if VITE_CONFIG_NAMES.contains(&name) {
        signals.node_project = true;
        signals.add_language_signal("typescript");
        signals.has_vite_config = true;
        signals.build_tools.insert("vite".to_string());
    }
    if MOCHA_CONFIG_NAMES.contains(&name) {
        signals.node_project = true;
        signals.add_language_signal("typescript");
        signals.test_runners.insert("mocha".to_string());
        signals.has_tests = true;
    }
    if PYTEST_CONFIG_NAMES.contains(&name) {
        signals.has_pytest_signal = true;
        signals.test_runners.insert("pytest".to_string());
        signals.has_tests = true;
    }
    if NEXTEST_CONFIG_NAMES.contains(&name) || rel.ends_with("/.config/nextest.toml") {
        signals.test_runners.insert("nextest".to_string());
    }

    if let Some(language) =
        source_language_for_file(name, path.extension().and_then(|ext| ext.to_str()))
    {
        signals.add_source_language(language);
        if language == "typescript" {
            signals.node_project = true;
        }
        if language == "python" {
            signals.python_project = true;
        }
        if language == "ruby" {
            signals.ruby_project = true;
        }
    } else if path.extension().and_then(|ext| ext.to_str()) == Some("h") {
        signals.add_language_signal("c");
    }
}

pub(super) fn inspect_cargo_manifest(path: &Path, signals: &mut FingerprintSignals) {
    signals.add_language_signal("rust");
    signals.package_managers.insert("cargo".to_string());
    signals.build_tools.insert("cargo".to_string());
    signals.test_runners.insert("cargo-test".to_string());
    let Some(text) = read_text_if_exists(path.to_path_buf()) else {
        return;
    };
    let Ok(parsed) = toml::from_str::<toml::Value>(&text) else {
        return;
    };
    let deps = collect_toml_keys(
        &parsed,
        &[
            &["dependencies"],
            &["dev-dependencies"],
            &["build-dependencies"],
            &["workspace", "dependencies"],
        ],
    );
    if deps.contains("axum") {
        signals.frameworks.insert("axum".to_string());
    }
}

pub(super) fn inspect_package_json(path: &Path, signals: &mut FingerprintSignals) {
    let Some(parsed) = read_json_object(path.to_path_buf()) else {
        return;
    };
    signals.node_project = true;

    let deps = collect_json_dependency_names(&parsed);
    if deps.contains("next") {
        signals.has_next_dep = true;
        signals.build_tools.insert("next".to_string());
    }
    if deps.contains("react") {
        signals.frameworks.insert("react".to_string());
        signals.add_language_signal("typescript");
    }
    if deps.contains("typescript") {
        signals.add_language_signal("typescript");
    }
    if deps.contains("vite") {
        signals.has_vite_dep = true;
        signals.build_tools.insert("vite".to_string());
    }
    if deps.contains("vitest") {
        signals.test_runners.insert("vitest".to_string());
        signals.has_tests = true;
    }
    if deps.contains("jest") {
        signals.test_runners.insert("jest".to_string());
        signals.has_tests = true;
    }
    if deps.contains("mocha") {
        signals.test_runners.insert("mocha".to_string());
        signals.has_tests = true;
    }

    if let Some(package_manager) = parsed
        .get("packageManager")
        .and_then(|value| value.as_str())
    {
        if package_manager.starts_with("pnpm@") {
            signals.package_managers.insert("pnpm".to_string());
        } else if package_manager.starts_with("yarn@") {
            signals.package_managers.insert("yarn".to_string());
        } else if package_manager.starts_with("npm@") {
            signals.package_managers.insert("npm".to_string());
        }
    }

    if let Some(scripts) = parsed.get("scripts").and_then(|value| value.as_object()) {
        for command in scripts.values().filter_map(serde_json::Value::as_str) {
            record_command_signal(command, signals);
        }
    }
}

pub(super) fn inspect_pyproject(path: &Path, signals: &mut FingerprintSignals) {
    let Some(text) = read_text_if_exists(path.to_path_buf()) else {
        return;
    };
    signals.add_language_signal("python");
    signals.python_project = true;
    inspect_python_text(Some(&text), signals);

    let Ok(parsed) = toml::from_str::<toml::Value>(&text) else {
        signals.python_needs_pip = true;
        return;
    };
    let has_poetry = table_path_exists(&parsed, &["tool", "poetry"]);
    let has_uv = table_path_exists(&parsed, &["tool", "uv"]);
    if has_poetry {
        signals.package_managers.insert("poetry".to_string());
        signals.build_tools.insert("poetry".to_string());
    }
    if has_uv {
        signals.package_managers.insert("uv".to_string());
        signals.build_tools.insert("uv".to_string());
    }
    if !has_poetry && !has_uv {
        signals.python_needs_pip = true;
    }
}

pub(super) fn inspect_python_requirements(path: &Path, signals: &mut FingerprintSignals) {
    signals.add_language_signal("python");
    signals.python_project = true;
    signals.python_needs_pip = true;
    signals.build_tools.insert("pip".to_string());
    inspect_python_text(read_text_if_exists(path.to_path_buf()).as_deref(), signals);
}

pub(super) fn inspect_python_text(text: Option<&str>, signals: &mut FingerprintSignals) {
    let Some(text) = text else {
        return;
    };
    let lower = text.to_ascii_lowercase();
    if lower.contains("fastapi") {
        signals.frameworks.insert("fastapi".to_string());
    }
    if lower.contains("django") {
        signals.frameworks.insert("django".to_string());
    }
    if lower.contains("pytest") || lower.contains("[tool.pytest") {
        signals.has_pytest_signal = true;
        signals.test_runners.insert("pytest".to_string());
        signals.has_tests = true;
    }
    if lower.contains("unittest") {
        signals.has_unittest_signal = true;
        signals.test_runners.insert("unittest".to_string());
        signals.has_tests = true;
    }
}

pub(super) fn inspect_composer_json(path: &Path, signals: &mut FingerprintSignals) {
    signals.add_language_signal("php");
    signals.php_project = true;
    signals.package_managers.insert("composer".to_string());
    signals.build_tools.insert("composer".to_string());

    let Some(parsed) = read_json_object(path.to_path_buf()) else {
        return;
    };
    let deps = collect_composer_dependency_names(&parsed);
    if deps.contains("laravel/framework") {
        signals.frameworks.insert("laravel".to_string());
    }
    if deps.contains("pestphp/pest") {
        signals.test_runners.insert("pest".to_string());
        signals.has_tests = true;
    }
    if deps.contains("phpunit/phpunit") {
        signals.test_runners.insert("phpunit".to_string());
        signals.has_tests = true;
    }
    if let Some(scripts) = parsed.get("scripts").and_then(|value| value.as_object()) {
        for value in scripts.values() {
            for command in composer_script_commands(value) {
                record_command_signal(command, signals);
            }
        }
    }
}

pub(super) fn inspect_gemfile(path: &Path, signals: &mut FingerprintSignals) {
    signals.add_language_signal("ruby");
    signals.ruby_project = true;
    signals.package_managers.insert("bundler".to_string());
    signals.build_tools.insert("bundler".to_string());
    let Some(text) = read_text_if_exists(path.to_path_buf()) else {
        return;
    };
    if text.contains("gem \"rails\"") || text.contains("gem 'rails'") {
        signals.frameworks.insert("rails".to_string());
    }
    if text.contains("gem \"rspec\"") || text.contains("gem 'rspec'") {
        signals.test_runners.insert("rspec".to_string());
    }
    if text.contains("gem \"minitest\"") || text.contains("gem 'minitest'") {
        signals.test_runners.insert("minitest".to_string());
    }
}

pub(super) fn record_command_signal(command: &str, signals: &mut FingerprintSignals) {
    let normalized = command.to_ascii_lowercase();
    if normalized.contains("next build") || normalized.contains("next dev") {
        signals.build_tools.insert("next".to_string());
    }
    if normalized.contains("vite build")
        || normalized.contains("vite dev")
        || normalized.contains("npx vite")
    {
        signals.has_vite_dep = true;
        signals.build_tools.insert("vite".to_string());
    }
    if normalized.contains("vitest") {
        signals.test_runners.insert("vitest".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("jest") {
        signals.test_runners.insert("jest".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("mocha") {
        signals.test_runners.insert("mocha".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("cargo nextest") {
        signals.test_runners.insert("nextest".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("cargo test") {
        signals.test_runners.insert("cargo-test".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("pytest") {
        signals.test_runners.insert("pytest".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("pest") {
        signals.test_runners.insert("pest".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("php artisan test") {
        signals.has_tests = true;
    }
    if normalized.contains("phpunit") {
        signals.test_runners.insert("phpunit".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("composer install")
        || normalized.contains("composer update")
        || normalized.contains("composer test")
    {
        signals.package_managers.insert("composer".to_string());
        signals.build_tools.insert("composer".to_string());
    }
    if normalized.contains("python -m unittest") || normalized.contains("unittest") {
        signals.test_runners.insert("unittest".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("go test") {
        signals.test_runners.insert("go-test".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("swift test") {
        signals.test_runners.insert("xctest".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("swift build") {
        signals.build_tools.insert("spm".to_string());
    }
}

pub(super) fn collect_json_dependency_names(
    parsed: &serde_json::Map<String, serde_json::Value>,
) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for key in [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ] {
        let Some(entries) = parsed.get(key).and_then(|value| value.as_object()) else {
            continue;
        };
        names.extend(entries.keys().cloned());
    }
    names
}

pub(super) fn collect_composer_dependency_names(
    parsed: &serde_json::Map<String, serde_json::Value>,
) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for key in ["require", "require-dev"] {
        let Some(entries) = parsed.get(key).and_then(|value| value.as_object()) else {
            continue;
        };
        names.extend(entries.keys().cloned());
    }
    names
}

pub(super) fn composer_script_commands(value: &serde_json::Value) -> Vec<&str> {
    match value {
        serde_json::Value::String(command) => vec![command.as_str()],
        serde_json::Value::Array(commands) => commands
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect(),
        _ => Vec::new(),
    }
}

pub(super) fn source_language_for_file(
    name: &str,
    extension: Option<&str>,
) -> Option<&'static str> {
    if is_tooling_source_file(name) {
        return None;
    }
    match extension {
        Some("rs") => Some("rust"),
        Some("ts") | Some("tsx") | Some("js") | Some("jsx") | Some("mjs") | Some("cjs") => {
            Some("typescript")
        }
        Some("py") => Some("python"),
        Some("go") => Some("go"),
        Some("swift") => Some("swift"),
        Some("rb") => Some("ruby"),
        Some("scala") | Some("sc") => Some("scala"),
        Some("kt") | Some("kts") => Some("kotlin"),
        Some("java") => Some("java"),
        Some("ex") | Some("exs") => Some("elixir"),
        Some("zig") | Some("zon") => Some("zig"),
        Some("php") => Some("php"),
        Some("cs") => Some("csharp"),
        Some("c") => Some("c"),
        Some("cpp") | Some("cc") | Some("cxx") | Some("hpp") | Some("hh") | Some("hxx")
        | Some("mm") => Some("cpp"),
        Some("h") => None,
        _ => None,
    }
}

pub(super) fn is_tooling_source_file(name: &str) -> bool {
    matches!(
        name,
        "Package.swift"
            | "build.gradle.kts"
            | "build.zig"
            | "build.zig.zon"
            | "mix.exs"
            | "setup.py"
    ) || NEXT_CONFIG_NAMES.contains(&name)
        || VITEST_CONFIG_NAMES.contains(&name)
        || JEST_CONFIG_NAMES.contains(&name)
        || VITE_CONFIG_NAMES.contains(&name)
        || MOCHA_CONFIG_NAMES.contains(&name)
}

pub(super) fn ranked_project_languages(signals: &FingerprintSignals) -> Vec<String> {
    let ordered_signals = ordered_values(&signals.languages, PROJECT_LANGUAGE_ORDER);
    if signals.source_language_counts.is_empty() {
        return ordered_signals;
    }

    let mut source_languages = signals
        .source_language_counts
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    source_languages.sort_by(|left, right| {
        let right_count = signals
            .source_language_counts
            .get(right)
            .copied()
            .unwrap_or_default();
        let left_count = signals
            .source_language_counts
            .get(left)
            .copied()
            .unwrap_or_default();
        right_count
            .cmp(&left_count)
            .then_with(|| language_order_rank(left).cmp(&language_order_rank(right)))
            .then_with(|| left.cmp(right))
    });

    let mut ranked = source_languages;
    for language in ordered_signals {
        if !ranked.contains(&language) {
            ranked.push(language);
        }
    }
    ranked
}

pub(super) fn primary_project_language(
    languages: &[String],
    signals: &FingerprintSignals,
) -> String {
    if languages.is_empty() {
        return "unknown".to_string();
    }
    if signals.source_language_counts.is_empty() {
        return if languages.len() == 1 {
            languages[0].clone()
        } else {
            "mixed".to_string()
        };
    }

    let source_languages = languages
        .iter()
        .filter(|language| {
            signals
                .source_language_counts
                .contains_key(language.as_str())
        })
        .collect::<Vec<_>>();
    let Some(first) = source_languages.first() else {
        return if languages.len() == 1 {
            languages[0].clone()
        } else {
            "mixed".to_string()
        };
    };

    let top_count = signals
        .source_language_counts
        .get(first.as_str())
        .copied()
        .unwrap_or_default();
    let total_count = signals.source_language_counts.values().sum::<usize>();
    if top_count * 2 > total_count {
        return (*first).clone();
    }
    "mixed".to_string()
}

pub(super) fn language_order_rank(language: &str) -> usize {
    PROJECT_LANGUAGE_ORDER
        .iter()
        .position(|candidate| *candidate == language)
        .unwrap_or(usize::MAX)
}

pub(super) fn collect_toml_keys(parsed: &toml::Value, paths: &[&[&str]]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for path in paths {
        let Some(table) = lookup_toml_path(parsed, path).and_then(toml::Value::as_table) else {
            continue;
        };
        names.extend(table.keys().cloned());
    }
    names
}

pub(super) fn table_path_exists(parsed: &toml::Value, path: &[&str]) -> bool {
    lookup_toml_path(parsed, path).is_some()
}

pub(super) fn lookup_toml_path<'a>(
    value: &'a toml::Value,
    path: &[&str],
) -> Option<&'a toml::Value> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    Some(current)
}

pub(super) fn ordered_values(values: &BTreeSet<String>, order: &[&str]) -> Vec<String> {
    let mut ordered = Vec::new();
    for wanted in order {
        if values.contains(*wanted) {
            ordered.push((*wanted).to_string());
        }
    }
    for value in values {
        if !order.iter().any(|candidate| candidate == &value.as_str()) {
            ordered.push(value.clone());
        }
    }
    ordered
}

pub(super) fn first_ordered_value(values: &[String]) -> Option<String> {
    values.first().cloned()
}
