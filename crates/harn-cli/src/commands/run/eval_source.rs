//! Inline-source wrapping and invocation-owned tempfile lifecycle.

/// Build the wrapped source and temp file backing a `harn run -e` invocation.
///
/// `import` is a top-level declaration in Harn, so leading imports are hoisted
/// out of the generated pipeline. The tempfile lives beside the project so
/// relative imports and `harn.toml` discovery use the caller's workspace.
pub(crate) fn prepare_eval_temp_file(
    code: &str,
) -> Result<(String, tempfile::NamedTempFile), String> {
    Ok((eval_source_for_code(code), create_eval_temp_file()?))
}

pub(super) fn eval_source_for_code(code: &str) -> String {
    if eval_code_parses_as_program(code) {
        return code.to_string();
    }
    let (header, body) = split_eval_header(code);
    if header.is_empty() {
        format!("pipeline main(task) {{\n{body}\n}}")
    } else {
        format!("{header}\npipeline main(task) {{\n{body}\n}}")
    }
}

fn eval_code_parses_as_program(code: &str) -> bool {
    harn_parser::parse_source(code)
        .map(|program| {
            program.iter().any(|node| {
                let (_, inner) = harn_parser::peel_attributes(node);
                matches!(&inner.node, harn_parser::Node::Pipeline { .. })
            })
        })
        .unwrap_or(false)
}

/// Create the source beside the project when possible so relative imports
/// resolve correctly, falling back to the system temporary directory.
pub(super) fn create_eval_temp_file() -> Result<tempfile::NamedTempFile, String> {
    if let Some(dir) = std::env::current_dir().ok().as_deref() {
        match tempfile::Builder::new()
            .prefix(".harn-eval-")
            .suffix(".harn")
            .tempfile_in(dir)
        {
            Ok(tmp) => return Ok(tmp),
            Err(error) => eprintln!(
                "warning: harn run -e: could not create temp file in {}: {error}; \
                 relative imports will not resolve",
                dir.display()
            ),
        }
    }
    tempfile::Builder::new()
        .prefix("harn-eval-")
        .suffix(".harn")
        .tempfile()
        .map_err(|error| format!("failed to create temp file for -e: {error}"))
}

/// Split leading imports, blanks, and comments from the pipeline body.
pub(super) fn split_eval_header(code: &str) -> (String, String) {
    let mut header_end = 0usize;
    let mut last_kept = 0usize;
    for (idx, line) in code.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            header_end = idx + 1;
            continue;
        }
        let is_import = trimmed.starts_with("import ")
            || trimmed.starts_with("import\t")
            || trimmed.starts_with("import\"")
            || trimmed.starts_with("pub import ")
            || trimmed.starts_with("pub import\t");
        if is_import {
            header_end = idx + 1;
            last_kept = idx + 1;
        } else {
            break;
        }
    }
    if last_kept == 0 {
        return (String::new(), code.to_string());
    }
    let mut header_lines = Vec::new();
    let mut body_lines = Vec::new();
    for (idx, line) in code.lines().enumerate() {
        if idx < header_end {
            header_lines.push(line);
        } else {
            body_lines.push(line);
        }
    }
    (header_lines.join("\n"), body_lines.join("\n"))
}
