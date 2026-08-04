//! In-process coverage of `harn check` / `harn lint` regression behaviors.
//!
//! Tier 1H of the de-flake epic (#1057, #1067): subprocess CLI tests
//! pay full cold-start cost per test and rely on stderr scraping. The
//! regressions here live in workspace library crates (`harn_parser`
//! type-checking and `harn_modules::build` cycle handling), so the
//! tests can call those library APIs directly and assert on the
//! returned values without going through the `harn` binary.

use std::fs;
use std::path::{Path, PathBuf};

use harn_parser::{check_source, DiagnosticSeverity, PipelineError, TypeDiagnostic};
use tempfile::TempDir;

#[test]
fn check_reports_unknown_struct_type_with_precise_location() {
    let temp = TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    let source = "const p = Point { x: 3, y: 4 }\n";
    fs::write(&script, source).unwrap();

    let (_program, diagnostics) = match check_source(source) {
        Ok(out) => out,
        Err(PipelineError::TypeCheck(boxed)) => (Vec::new(), vec![*boxed]),
        Err(other) => panic!("unexpected non-type-check pipeline error: {other:?}"),
    };

    let errors: Vec<&TypeDiagnostic> = diagnostics
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .collect();
    assert!(
        !errors.is_empty(),
        "expected at least one type-check error; got diagnostics: {diagnostics:#?}"
    );

    let unknown_struct = errors
        .iter()
        .find(|d| d.message.contains("unknown struct type `Point`"))
        .unwrap_or_else(|| {
            panic!(
                "missing unknown-struct diagnostic; got: {:#?}",
                errors.iter().map(|d| &d.message).collect::<Vec<_>>()
            )
        });

    let span = unknown_struct
        .span
        .as_ref()
        .expect("unknown-struct diagnostic should carry a span");
    let (line, column) = line_column_for_offset(source, span.start);
    assert_eq!(
        (line, column),
        (1, 11),
        "expected diagnostic at 1:11, got {line}:{column} (span={span:?})"
    );
}

/// In-process regression for the Linux hang on cycles across sibling
/// directories (#748). The fix lives in `harn_modules::build` (#93),
/// which canonicalizes paths before the seen-set dedupe; that is the
/// surface this test exercises directly. Four sibling directories ×
/// six files importing every other directory by relative path — the
/// exact pattern that produced fresh path spellings on every
/// round-trip and OOM-killed Linux CI runners around 48 s pre-fix.
#[test]
fn module_graph_terminates_on_large_cross_directory_cycle() {
    let temp = TempDir::new().unwrap();
    let pipelines = temp.path().join("Sources/pipelines");
    let files = write_cross_directory_cycle_workspace(&pipelines);

    // The pre-fix bug appended distinct path spellings forever and
    // never returned. The post-fix `build()` terminates and returns a
    // graph with import edges resolvable for every seed file. Calling
    // `imported_names_for_file` on each seed forces the build to have
    // visited every node.
    let graph = harn_modules::build(&files);
    for seed in &files {
        assert!(
            graph.imported_names_for_file(seed).is_some(),
            "module graph dropped seed file {seed:?}; pre-fix this loop never terminated"
        );
    }
}

fn write_cross_directory_cycle_workspace(pipelines: &Path) -> Vec<PathBuf> {
    let dirs = ["context", "runtime", "host", "tools"];
    let files_per_dir: usize = 6;
    for dir in dirs {
        fs::create_dir_all(pipelines.join(dir)).unwrap();
    }
    let mut written: Vec<PathBuf> = Vec::new();
    for (dir_idx, dir) in dirs.iter().enumerate() {
        for file_idx in 0..files_per_dir {
            let mut source = String::new();
            for (other_idx, other) in dirs.iter().enumerate() {
                if other_idx == dir_idx {
                    continue;
                }
                let target = format!("m{}", (file_idx + other_idx) % files_per_dir);
                source.push_str(&format!("import \"../{other}/{target}\"\n"));
            }
            source.push_str(&format!("pub fn {dir}_m{file_idx}() {{ {file_idx} }}\n"));
            let path = pipelines.join(dir).join(format!("m{file_idx}.harn"));
            fs::write(&path, source).unwrap();
            written.push(path);
        }
    }
    written
}

fn line_column_for_offset(source: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for (idx, ch) in source.char_indices() {
        if idx >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}
