//! In-process tests for `harn check` / `harn lint` semantics.
//!
//! Replaces the prior subprocess-spawning version (#1067). The two
//! regressions guarded here both live in workspace library crates
//! (`harn_parser` for the type-diagnostic case, `harn_modules` for the
//! cross-directory cycle case), so the tests call those crates directly
//! without paying the cold-start cost of spawning the `harn` binary.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use harn_lexer::Lexer;
use harn_parser::{diagnostic::render_type_diagnostic, DiagnosticSeverity, Parser, TypeChecker};
use tempfile::TempDir;

#[test]
fn check_reports_unknown_struct_type_in_stderr() {
    let temp = TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    let source = "let p = Point { x: 3, y: 4 }\n";
    fs::write(&script, source).unwrap();

    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().expect("lex");
    let program = Parser::new(tokens).parse().expect("parse");
    let diagnostics = TypeChecker::new().check_with_source(&program, source);

    let path_str = script.to_string_lossy().into_owned();
    let mut rendered_messages = String::new();
    let mut had_error = false;
    for diag in &diagnostics {
        if matches!(diag.severity, DiagnosticSeverity::Error) {
            had_error = true;
        }
        rendered_messages.push_str(&render_type_diagnostic(source, &path_str, diag));
    }

    assert!(
        had_error,
        "expected a type-checker error; got diagnostics={:#?}",
        diagnostics
    );
    assert!(
        rendered_messages.contains("unknown struct type `Point`"),
        "missing unknown-struct diagnostic in:\n{rendered_messages}"
    );
    assert!(
        rendered_messages.contains(&format!("{}:1:9", path_str)),
        "missing precise location in:\n{rendered_messages}"
    );
}

/// Regression for the Linux hang on `harn lint <dir>` and
/// `harn check --workspace` against pipeline trees with cyclic
/// cross-sibling-directory relative imports (#748). The underlying fix
/// (`harn_modules::build()` canonicalizing before seen-set dedupe, #93)
/// already has a unit test in `harn-modules`; this test guards the same
/// regression at the API the CLI walkers actually call. Pre-fix the
/// `build` call OOM-killed Linux CI runners around 48s; post-fix every
/// supported platform completes in well under a second.
#[test]
fn module_graph_build_completes_on_large_cross_directory_cycle_workspace() {
    let temp = TempDir::new().unwrap();
    let pipelines = temp.path().join("Sources/pipelines");
    let files = write_cross_directory_cycle_workspace(&pipelines);

    let budget = Duration::from_secs(60);
    let started = Instant::now();
    let graph = harn_modules::build(&files);
    let elapsed = started.elapsed();
    assert!(
        elapsed < budget,
        "harn_modules::build took {elapsed:?} (>{budget:?}) — likely a regression to the path-spelling explosion fixed by #93"
    );
    // Every file participated in the graph (no silent drops). The walker
    // must reach every entry point at least once even with cyclic edges.
    for file in &files {
        // A file that the walker actually visited will have at least one
        // imported-name set (every file imports from three siblings).
        assert!(
            graph.imported_names_for_file(file).is_some(),
            "module graph missing entry for {}",
            file.display()
        );
    }
}

fn write_cross_directory_cycle_workspace(pipelines: &Path) -> Vec<std::path::PathBuf> {
    let dirs = ["context", "runtime", "host", "tools"];
    let files_per_dir = 6;
    let mut files = Vec::new();
    for dir in dirs {
        fs::create_dir_all(pipelines.join(dir)).unwrap();
    }
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
            files.push(path);
        }
    }
    files
}
