//! Guard the shared sink for explicitly labeled CLI errors and warnings.

use std::path::{Path, PathBuf};

fn rust_sources(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).expect("CLI source directory must be readable") {
        let entry = entry.expect("CLI source entry must be readable");
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

#[test]
fn labeled_command_diagnostics_use_the_shared_sink() {
    let direct_diagnostic =
        regex::Regex::new(r#"(?i)eprintln!\(\s*"(?:error:|warning:|warning\[|warn:)"#).unwrap();
    assert!(direct_diagnostic.is_match("eprintln!(\n  \"error: example\")"));
    assert!(direct_diagnostic.is_match("eprintln!(\n  \"WARN: example\")"));
    assert!(!direct_diagnostic.is_match("cli_diagnostic!(\"error: example\")"));

    let source = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("test runner must supply the package root"),
    )
    .join("src");
    let mut files = Vec::new();
    rust_sources(&source, &mut files);
    files.retain(|path| !path.starts_with(source.join("bin")));
    assert!(!files.is_empty(), "no CLI sources were inspected");
    let findings: Vec<_> = files
        .iter()
        .filter(|path| {
            let text = std::fs::read_to_string(path).expect("CLI source must be readable");
            direct_diagnostic.is_match(&text)
        })
        .collect();
    assert!(
        findings.is_empty(),
        "direct labeled diagnostics: {findings:?}"
    );
}
