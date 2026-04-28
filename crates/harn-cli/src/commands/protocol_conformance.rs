use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum FixtureExpectation {
    #[default]
    Valid,
    Invalid,
}

#[derive(Debug, Deserialize)]
struct ProtocolFixture {
    name: String,
    protocol: String,
    schema: String,
    #[serde(default)]
    expect: FixtureExpectation,
    documents: Vec<Value>,
}

#[derive(Debug, Default)]
struct ProtocolConformanceReport {
    passed: usize,
    failed: usize,
    skipped: usize,
    errors: Vec<String>,
}

pub(crate) fn run_protocol_conformance(
    selection: Option<&str>,
    filter: Option<&str>,
    verbose: bool,
) {
    let root = PathBuf::from("conformance/protocols");
    let report = run_protocol_conformance_inner(&root, selection, filter, verbose);
    if report.failed > 0 {
        eprintln!();
        for error in &report.errors {
            eprintln!("{error}");
        }
        eprintln!(
            "Protocol conformance failed: {} passed, {} failed, {} skipped",
            report.passed, report.failed, report.skipped
        );
        process::exit(1);
    }
    println!(
        "Protocol conformance passed: {} passed, {} skipped",
        report.passed, report.skipped
    );
}

fn run_protocol_conformance_inner(
    root: &Path,
    selection: Option<&str>,
    filter: Option<&str>,
    verbose: bool,
) -> ProtocolConformanceReport {
    let mut report = ProtocolConformanceReport::default();
    if !root.exists() {
        report.failed += 1;
        report.errors.push(format!(
            "Protocol conformance root not found: {}",
            root.display()
        ));
        return report;
    }

    let fixtures = match resolve_fixture_files(root, selection) {
        Ok(fixtures) => fixtures,
        Err(error) => {
            report.failed += 1;
            report.errors.push(error);
            return report;
        }
    };
    let display_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    for fixture_path in fixtures {
        let relative_path = fixture_path
            .strip_prefix(&display_root)
            .unwrap_or(&fixture_path)
            .display()
            .to_string();
        let fixture = match read_fixture(&fixture_path) {
            Ok(fixture) => fixture,
            Err(error) => {
                report.failed += 1;
                report.errors.push(format!("{relative_path}: {error}"));
                println!("  \x1b[31mFAIL\x1b[0m  {relative_path}");
                continue;
            }
        };

        if !matches_filter(filter, &fixture, &relative_path) {
            report.skipped += 1;
            continue;
        }

        match run_fixture(root, &fixture_path, &fixture) {
            Ok(()) => {
                report.passed += 1;
                if verbose {
                    println!(
                        "  \x1b[32mPASS\x1b[0m  {} ({})",
                        fixture.name, relative_path
                    );
                } else {
                    println!("  \x1b[32mPASS\x1b[0m  {}", fixture.name);
                }
            }
            Err(error) => {
                report.failed += 1;
                report.errors.push(format!("{relative_path}: {error}"));
                println!("  \x1b[31mFAIL\x1b[0m  {}", fixture.name);
            }
        }
    }

    report
}

fn resolve_fixture_files(root: &Path, selection: Option<&str>) -> Result<Vec<PathBuf>, String> {
    let fixture_root = root.join("fixtures");
    let selected = match selection {
        Some(selection) => {
            let raw = PathBuf::from(selection);
            if raw.is_absolute() || raw.starts_with(root) {
                raw
            } else {
                root.join(raw)
            }
        }
        None => fixture_root,
    };

    if !selected.exists() {
        return Err(format!(
            "Protocol conformance target not found: {}",
            selected.display()
        ));
    }
    let selected = canonicalize_under(root, &selected)?;
    let mut files = Vec::new();
    collect_json_files(&selected, &mut files);
    if files.is_empty() {
        return Err(format!(
            "No protocol fixture JSON files found under {}",
            selected.display()
        ));
    }
    Ok(files)
}

fn canonicalize_under(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("Failed to canonicalize {}: {error}", root.display()))?;
    let path = path
        .canonicalize()
        .map_err(|error| format!("Failed to canonicalize {}: {error}", path.display()))?;
    if !path.starts_with(&root) {
        return Err(format!(
            "Protocol conformance target must be inside {}: {}",
            root.display(),
            path.display()
        ));
    }
    Ok(path)
}

fn collect_json_files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        if path.extension().is_some_and(|ext| ext == "json") {
            out.push(path.to_path_buf());
        }
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        collect_json_files(&entry.path(), out);
    }
}

fn read_fixture(path: &Path) -> Result<ProtocolFixture, String> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("failed to read fixture: {error}"))?;
    let fixture: ProtocolFixture =
        serde_json::from_str(&text).map_err(|error| format!("invalid fixture JSON: {error}"))?;
    if fixture.documents.is_empty() {
        return Err("fixture must contain at least one document".to_string());
    }
    Ok(fixture)
}

fn matches_filter(filter: Option<&str>, fixture: &ProtocolFixture, relative_path: &str) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    if let Some(pattern) = filter.strip_prefix("re:") {
        return Regex::new(pattern).is_ok_and(|regex| {
            regex.is_match(&fixture.name)
                || regex.is_match(&fixture.protocol)
                || regex.is_match(relative_path)
        });
    }
    fixture.name.contains(filter)
        || fixture.protocol.contains(filter)
        || relative_path.contains(filter)
}

fn run_fixture(root: &Path, fixture_path: &Path, fixture: &ProtocolFixture) -> Result<(), String> {
    let schema_path = resolve_schema_path(root, fixture_path, &fixture.schema)?;
    let schema_text = fs::read_to_string(&schema_path)
        .map_err(|error| format!("failed to read schema {}: {error}", schema_path.display()))?;
    let schema: Value = serde_json::from_str(&schema_text)
        .map_err(|error| format!("invalid schema JSON {}: {error}", schema_path.display()))?;
    jsonschema::draft202012::meta::validate(&schema).map_err(|error| {
        format!(
            "schema {} is not valid JSON Schema 2020-12: {error}",
            schema_path.display()
        )
    })?;
    let validator = jsonschema::draft202012::new(&schema).map_err(|error| {
        format!(
            "failed to compile schema {}: {error}",
            schema_path.display()
        )
    })?;

    for (index, document) in fixture.documents.iter().enumerate() {
        let result = validator.validate(document);
        match (&fixture.expect, result) {
            (FixtureExpectation::Valid, Ok(())) => {}
            (FixtureExpectation::Valid, Err(error)) => {
                return Err(format!(
                    "document #{index} was expected to be valid against {}: {error}",
                    schema_path.display()
                ));
            }
            (FixtureExpectation::Invalid, Ok(())) => {
                return Err(format!(
                    "document #{index} was expected to be rejected by {}",
                    schema_path.display()
                ));
            }
            (FixtureExpectation::Invalid, Err(_)) => {}
        }
    }

    Ok(())
}

fn resolve_schema_path(root: &Path, fixture_path: &Path, schema: &str) -> Result<PathBuf, String> {
    let raw = PathBuf::from(schema);
    let path = if raw.is_absolute() || raw.starts_with(root) {
        raw
    } else if schema.starts_with("schemas/") {
        root.join(raw)
    } else {
        fixture_path.parent().unwrap_or(root).join(raw)
    };
    canonicalize_under(root, &path)
}

#[cfg(test)]
mod tests {
    use super::{run_protocol_conformance_inner, FixtureExpectation, ProtocolFixture};

    #[test]
    fn expectation_defaults_to_valid() {
        let fixture: ProtocolFixture = serde_json::from_str(
            r#"{
              "name": "sample",
              "protocol": "mcp",
              "schema": "schemas/mcp-2025-11-25.schema.json",
              "documents": [{}]
            }"#,
        )
        .unwrap();
        assert_eq!(fixture.expect, FixtureExpectation::Valid);
    }

    #[test]
    fn checked_in_protocol_fixtures_pass() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("conformance/protocols");
        let report = run_protocol_conformance_inner(&root, None, None, false);
        assert_eq!(report.failed, 0, "{:#?}", report.errors);
        assert!(report.passed >= 6);
    }
}
