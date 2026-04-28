//! Fixture-based golden tests for the `ast::*` builtins (issue #564).
//!
//! Layout: `tests/fixtures/ast/<language>/source.<ext>` paired with two
//! goldens — `symbols.golden.json` and `outline.golden.json` — generated
//! from the live extractor. Adding a new language is: drop in a source
//! file, set `HARN_AST_UPDATE_GOLDEN=1`, run the test, commit. The
//! per-language goldens are the hostlib compatibility contract, so changes
//! here are visible in code review.

use std::fs;
use std::path::{Path, PathBuf};

use harn_hostlib::ast::{api, Language, OutlineItem, Symbol};

const UPDATE_GOLDEN_ENV: &str = "HARN_AST_UPDATE_GOLDEN";

/// Walk every language fixture and compare its symbols/outline against
/// the goldens. One Rust test rather than 22 keeps the file lean and
/// makes failures easy to scan.
#[test]
fn every_language_fixture_matches_its_golden() {
    let fixtures_root = manifest_dir().join("tests/fixtures/ast");
    let mut fixture_dirs: Vec<PathBuf> = fs::read_dir(&fixtures_root)
        .unwrap_or_else(|err| panic!("read fixtures dir {}: {err}", fixtures_root.display()))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|entry| entry.path())
        .collect();
    fixture_dirs.sort();

    assert!(
        !fixture_dirs.is_empty(),
        "no language fixtures under {}",
        fixtures_root.display()
    );

    let update = std::env::var(UPDATE_GOLDEN_ENV).is_ok();
    let mut failures: Vec<String> = Vec::new();

    for dir in &fixture_dirs {
        if let Err(message) = run_fixture(dir, update) {
            failures.push(format!(
                "[{}] {message}",
                dir.file_name().unwrap().to_string_lossy()
            ));
        }
    }

    if !failures.is_empty() {
        panic!(
            "{} fixture(s) failed:\n  - {}\n\nRe-run with {UPDATE_GOLDEN_ENV}=1 to regenerate goldens.",
            failures.len(),
            failures.join("\n  - "),
        );
    }
}

/// Every shipped language must have a fixture. Catches "I added a new
/// language but forgot the fixture" in code review.
#[test]
fn every_language_has_a_fixture() {
    let fixtures_root = manifest_dir().join("tests/fixtures/ast");
    let dirs: std::collections::HashSet<String> = fs::read_dir(&fixtures_root)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    let missing: Vec<&str> = Language::all()
        .iter()
        .map(|l| l.name())
        .filter(|name| !dirs.contains(*name))
        .collect();
    assert!(
        missing.is_empty(),
        "languages without fixtures: {missing:?} (under {})",
        fixtures_root.display()
    );
}

fn run_fixture(dir: &Path, update: bool) -> Result<(), String> {
    let language_name = dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("non-UTF8 fixture dir {}", dir.display()))?;
    let language = Language::from_name(language_name)
        .ok_or_else(|| format!("unknown language `{language_name}`"))?;

    let source_path =
        find_source(dir).ok_or_else(|| format!("no source.* file under {}", dir.display()))?;

    let (_, symbols) = api::symbols(&source_path, Some(language.name()))
        .map_err(|err| format!("symbols extraction failed: {err}"))?;
    let (_, outline) = api::outline(&source_path, Some(language.name()))
        .map_err(|err| format!("outline build failed: {err}"))?;

    compare_or_update(
        &dir.join("symbols.golden.json"),
        &symbols_to_json(&symbols),
        update,
    )?;
    compare_or_update(
        &dir.join("outline.golden.json"),
        &outline_to_json(&outline),
        update,
    )?;
    Ok(())
}

fn find_source(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(|name| name == "source")
                .unwrap_or(false)
                && path.extension().is_some()
        })
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn compare_or_update(path: &Path, actual: &str, update: bool) -> Result<(), String> {
    let pretty = format!("{actual}\n");
    if update || !path.exists() {
        fs::write(path, &pretty).map_err(|err| format!("write {}: {err}", path.display()))?;
        return Ok(());
    }
    let expected_raw =
        fs::read_to_string(path).map_err(|err| format!("read {}: {err}", path.display()))?;
    let expected = normalize_line_endings(&expected_raw);
    if expected != pretty {
        // Compute a small diff so the failure message is actionable.
        let mismatch = first_mismatch(&expected, &pretty);
        return Err(format!(
            "golden {} does not match (first mismatch: {})\n--- expected\n{expected}\n+++ actual\n{pretty}",
            path.display(),
            mismatch,
        ));
    }
    Ok(())
}

fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn first_mismatch(a: &str, b: &str) -> String {
    for (line, (la, lb)) in (1..).zip(a.lines().zip(b.lines())) {
        if la != lb {
            return format!("line {line}: `{la}` vs `{lb}`");
        }
    }
    if a.lines().count() != b.lines().count() {
        return format!(
            "different line counts: {} vs {}",
            a.lines().count(),
            b.lines().count()
        );
    }
    "trailing whitespace".into()
}

fn symbols_to_json(symbols: &[Symbol]) -> String {
    let mut out = String::from("[\n");
    for (i, sym) in symbols.iter().enumerate() {
        let comma = if i + 1 == symbols.len() { "" } else { "," };
        out.push_str(&format!(
            "  {{\"name\":{}, \"kind\":\"{}\", \"container\":{}, \"signature\":{}, \"start_row\":{}, \"start_col\":{}, \"end_row\":{}, \"end_col\":{}}}{comma}\n",
            quote(&sym.name),
            sym.kind.as_str(),
            sym.container
                .as_ref()
                .map(|s| quote(s))
                .unwrap_or_else(|| "null".into()),
            quote(&sym.signature),
            sym.start_row,
            sym.start_col,
            sym.end_row,
            sym.end_col,
        ));
    }
    out.push(']');
    out
}

fn outline_to_json(items: &[OutlineItem]) -> String {
    let mut out = String::new();
    write_outline_items(&mut out, items, 0);
    out
}

fn write_outline_items(buf: &mut String, items: &[OutlineItem], depth: usize) {
    let indent = "  ".repeat(depth);
    if items.is_empty() {
        buf.push_str(&indent);
        buf.push_str("[]");
        return;
    }
    buf.push_str(&indent);
    buf.push_str("[\n");
    for (i, item) in items.iter().enumerate() {
        let inner = "  ".repeat(depth + 1);
        buf.push_str(&inner);
        buf.push_str(&format!(
            "{{\"name\":{}, \"kind\":\"{}\", \"signature\":{}, \"start_row\":{}, \"end_row\":{}, \"children\":",
            quote(&item.name),
            item.kind.as_str(),
            quote(&item.signature),
            item.start_row,
            item.end_row,
        ));
        if item.children.is_empty() {
            buf.push_str("[]}");
        } else {
            buf.push('\n');
            write_outline_items(buf, &item.children, depth + 2);
            buf.push('\n');
            buf.push_str(&inner);
            buf.push('}');
        }
        if i + 1 < items.len() {
            buf.push(',');
        }
        buf.push('\n');
    }
    buf.push_str(&indent);
    buf.push(']');
}

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
