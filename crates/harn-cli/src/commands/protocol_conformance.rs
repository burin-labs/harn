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
    matrix: FixtureMatrix,
    #[serde(default)]
    expect: FixtureExpectation,
    documents: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct FixtureMatrix {
    version: String,
    family: String,
    case: String,
    source: FixtureSource,
}

#[derive(Debug, Deserialize)]
struct FixtureSource {
    kind: FixtureSourceKind,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    generator: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum FixtureSourceKind {
    OfficialExample,
    AdapterGenerated,
    HandAuthored,
}

struct SchemaProvenance {
    protocol: String,
    upstream_version: String,
    upstream_source_url: String,
    upstream_spec_url: String,
    retrieved_at: String,
    refresh_command: String,
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
                let candidate = root.join(&raw);
                if candidate.exists() {
                    candidate
                } else {
                    fixture_root.join(raw)
                }
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
    validate_fixture_metadata(&fixture)?;
    Ok(fixture)
}

fn validate_fixture_metadata(fixture: &ProtocolFixture) -> Result<(), String> {
    ensure_nonempty("name", &fixture.name)?;
    ensure_nonempty("protocol", &fixture.protocol)?;
    ensure_nonempty("schema", &fixture.schema)?;
    ensure_nonempty("matrix.version", &fixture.matrix.version)?;
    ensure_nonempty("matrix.family", &fixture.matrix.family)?;
    ensure_nonempty("matrix.case", &fixture.matrix.case)?;
    match fixture.protocol.as_str() {
        "mcp" | "acp" | "a2a" => {}
        other => {
            return Err(format!(
                "protocol must be one of mcp/acp/a2a, got {other:?}"
            ))
        }
    }
    match fixture.matrix.source.kind {
        FixtureSourceKind::OfficialExample => {
            let url = fixture.matrix.source.url.as_deref().unwrap_or_default();
            ensure_nonempty("matrix.source.url", url)?;
        }
        FixtureSourceKind::AdapterGenerated => {
            let generator = fixture
                .matrix
                .source
                .generator
                .as_deref()
                .unwrap_or_default();
            ensure_nonempty("matrix.source.generator", generator)?;
        }
        FixtureSourceKind::HandAuthored => {
            let description = fixture
                .matrix
                .source
                .description
                .as_deref()
                .unwrap_or_default();
            ensure_nonempty("matrix.source.description", description)?;
        }
    }
    Ok(())
}

fn ensure_nonempty(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{field} must be a non-empty string"));
    }
    Ok(())
}

fn matches_filter(filter: Option<&str>, fixture: &ProtocolFixture, relative_path: &str) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    if let Some(pattern) = filter.strip_prefix("re:") {
        return Regex::new(pattern).is_ok_and(|regex| {
            regex.is_match(&fixture.name)
                || regex.is_match(&fixture.protocol)
                || regex.is_match(&fixture.matrix.version)
                || regex.is_match(&fixture.matrix.family)
                || regex.is_match(&fixture.matrix.case)
                || regex.is_match(relative_path)
        });
    }
    fixture.name.contains(filter)
        || fixture.protocol.contains(filter)
        || fixture.matrix.version.contains(filter)
        || fixture.matrix.family.contains(filter)
        || fixture.matrix.case.contains(filter)
        || relative_path.contains(filter)
}

fn run_fixture(root: &Path, fixture_path: &Path, fixture: &ProtocolFixture) -> Result<(), String> {
    let schema_path = resolve_schema_path(root, fixture_path, &fixture.schema)?;
    let schema_text = fs::read_to_string(&schema_path)
        .map_err(|error| format!("failed to read schema {}: {error}", schema_path.display()))?;
    let schema: Value = serde_json::from_str(&schema_text)
        .map_err(|error| format!("invalid schema JSON {}: {error}", schema_path.display()))?;
    validate_schema_provenance(&schema, fixture, &schema_path)?;
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

fn validate_schema_provenance(
    schema: &Value,
    fixture: &ProtocolFixture,
    schema_path: &Path,
) -> Result<(), String> {
    let provenance = schema_provenance(schema).ok_or_else(|| {
        format!(
            "schema {} must define x-harn-provenance with upstream source, version, date, and refresh command",
            schema_path.display()
        )
    })?;
    if provenance.protocol != fixture.protocol {
        return Err(format!(
            "schema {} declares protocol {:?} but fixture {:?} uses {:?}",
            schema_path.display(),
            provenance.protocol,
            fixture.name,
            fixture.protocol
        ));
    }
    if provenance.upstream_version != fixture.matrix.version {
        return Err(format!(
            "schema {} declares upstream version {:?} but fixture {:?} is matrix version {:?}",
            schema_path.display(),
            provenance.upstream_version,
            fixture.name,
            fixture.matrix.version
        ));
    }
    ensure_nonempty(
        "x-harn-provenance.upstream_source_url",
        &provenance.upstream_source_url,
    )?;
    ensure_nonempty(
        "x-harn-provenance.upstream_spec_url",
        &provenance.upstream_spec_url,
    )?;
    ensure_nonempty("x-harn-provenance.retrieved_at", &provenance.retrieved_at)?;
    ensure_nonempty(
        "x-harn-provenance.refresh_command",
        &provenance.refresh_command,
    )?;
    Ok(())
}

fn schema_provenance(schema: &Value) -> Option<SchemaProvenance> {
    let object = schema.get("x-harn-provenance")?.as_object()?;
    Some(SchemaProvenance {
        protocol: object.get("protocol")?.as_str()?.to_string(),
        upstream_version: object.get("upstream_version")?.as_str()?.to_string(),
        upstream_source_url: object.get("upstream_source_url")?.as_str()?.to_string(),
        upstream_spec_url: object.get("upstream_spec_url")?.as_str()?.to_string(),
        retrieved_at: object.get("retrieved_at")?.as_str()?.to_string(),
        refresh_command: object.get("refresh_command")?.as_str()?.to_string(),
    })
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
    use super::{
        run_protocol_conformance_inner, validate_fixture_metadata, FixtureExpectation,
        ProtocolFixture,
    };

    #[test]
    fn expectation_defaults_to_valid() {
        let fixture: ProtocolFixture = serde_json::from_str(
            r#"{
              "name": "sample",
              "protocol": "mcp",
              "schema": "schemas/mcp-2025-11-25.schema.json",
              "matrix": {
                "version": "2025-11-25",
                "family": "initialize",
                "case": "success",
                "source": {
                  "kind": "hand_authored",
                  "description": "unit-test fixture metadata"
                }
              },
              "documents": [{}]
            }"#,
        )
        .unwrap();
        assert_eq!(fixture.expect, FixtureExpectation::Valid);
    }

    #[test]
    fn adapter_generated_fixtures_must_name_generator() {
        let fixture: ProtocolFixture = serde_json::from_str(
            r#"{
              "name": "sample",
              "protocol": "mcp",
              "schema": "schemas/mcp-2025-11-25.schema.json",
              "matrix": {
                "version": "2025-11-25",
                "family": "initialize",
                "case": "success",
                "source": {"kind": "adapter_generated"}
              },
              "documents": [{}]
            }"#,
        )
        .unwrap();
        let error = validate_fixture_metadata(&fixture).expect_err("missing generator should fail");
        assert!(error.contains("matrix.source.generator"));
    }

    #[test]
    fn checked_in_protocol_fixtures_pass() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("conformance/protocols");
        let report = run_protocol_conformance_inner(&root, None, None, false);
        assert_eq!(report.failed, 0, "{:#?}", report.errors);
        assert!(report.passed >= 25);
    }
}
