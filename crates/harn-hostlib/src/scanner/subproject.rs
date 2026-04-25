//! Sub-project detection. Ports `SubProjectDetector.swift`: walk up to
//! `max_depth=2` levels from the root, looking for project markers (Cargo,
//! package.json, go.mod, …). Skip hidden / known-non-project directories.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::scanner::result::SubProject;

/// Directories that are never sub-projects (build artifacts, deps, caches).
const EXCLUDED_DIRS: &[&str] = &[
    "node_modules",
    "dist",
    "build",
    "__pycache__",
    "venv",
    "target",
    "Pods",
    "DerivedData",
    "vendor",
    "coverage",
    "egg-info",
];

const MARKERS: &[(&str, &str)] = &[
    ("package.json", "typescript"),
    ("Cargo.toml", "rust"),
    ("go.mod", "go"),
    ("Package.swift", "swift"),
    ("pyproject.toml", "python"),
    ("setup.py", "python"),
    ("build.gradle.kts", "java"),
    ("pom.xml", "java"),
    ("CMakeLists.txt", "c"),
    ("Gemfile", "ruby"),
    ("composer.json", "php"),
    ("pubspec.yaml", "dart"),
    ("build.sbt", "scala"),
];

/// Detect sub-projects beneath `root`, sorted asc by path.
pub fn detect_subprojects(root: &Path, max_depth: usize) -> Vec<SubProject> {
    let mut results: Vec<SubProject> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    scan_dir(root, 0, max_depth, &mut results, &mut seen);
    results.sort_by(|a, b| a.path.cmp(&b.path));
    results
}

fn scan_dir(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<SubProject>,
    seen: &mut HashSet<String>,
) {
    let dir_str = dir.to_string_lossy().into_owned();
    for (marker, language) in MARKERS {
        let marker_path = dir.join(marker);
        if marker_path.exists() && seen.insert(dir_str.clone()) {
            let name = extract_name(dir, &marker_path, marker).unwrap_or_else(|| {
                dir.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string()
            });
            out.push(SubProject {
                path: dir_str.clone(),
                name,
                language: (*language).to_string(),
                project_marker: (*marker).to_string(),
            });
        }
    }
    if depth >= max_depth {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let name = match entry.file_name().to_str() {
            Some(n) => n.to_string(),
            None => continue,
        };
        if name.starts_with('.') || EXCLUDED_DIRS.contains(&name.as_str()) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, depth + 1, max_depth, out, seen);
        }
    }
}

fn extract_name(dir: &Path, marker_path: &Path, marker: &str) -> Option<String> {
    if marker == "package.json" || marker == "composer.json" {
        if let Some(name) = read_json_name(marker_path) {
            return Some(name);
        }
    }
    match marker {
        "Cargo.toml" => extract_toml_value(marker_path, "name"),
        "pubspec.yaml" => extract_yaml_value(marker_path, "name"),
        "build.sbt" => extract_sbt_name(marker_path),
        "go.mod" => extract_go_module(marker_path),
        _ => dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string()),
    }
}

fn read_json_name(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    json.get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn extract_toml_value(path: &Path, key: &str) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    let mut in_package = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if in_package
            && (trimmed.starts_with(&format!("{key} ")) || trimmed.starts_with(&format!("{key}=")))
        {
            if let Some(eq_idx) = trimmed.find('=') {
                let mut value = trimmed[eq_idx + 1..].trim().to_string();
                if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
                    value = value[1..value.len() - 1].to_string();
                }
                return Some(value);
            }
        }
    }
    None
}

fn extract_yaml_value(path: &Path, key: &str) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&format!("{key}:")) {
            let value = rest.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn extract_sbt_name(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if !(trimmed.starts_with("name :=") || trimmed.starts_with("name:=")) {
            continue;
        }
        let after_eq = trimmed.split_once(":=")?.1.trim();
        let value = after_eq.trim_matches('"').trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn extract_go_module(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("module ") {
            let value = rest.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn detects_cargo_root_and_subdir() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"outer\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("services/api")).unwrap();
        fs::write(
            root.join("services/api/Cargo.toml"),
            "[package]\nname = \"api\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let subs = detect_subprojects(root, 2);
        let names: Vec<_> = subs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"outer"));
        assert!(names.contains(&"api"));
    }

    #[test]
    fn skips_excluded_dirs() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("node_modules/something")).unwrap();
        fs::write(
            root.join("node_modules/something/package.json"),
            r#"{"name":"nope"}"#,
        )
        .unwrap();
        let subs = detect_subprojects(root, 2);
        assert!(subs.is_empty());
    }
}
