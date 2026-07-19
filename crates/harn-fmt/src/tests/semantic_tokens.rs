//! Corpus-level guards against a formatter changing program tokens.
//!
//! Layout may change newlines, parser-confirmed trailing commas, and source
//! coordinates. Every other token is program text and must survive exactly.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use harn_lexer::{Lexer, StringSegment, TokenKind};
use harn_parser::Parser;

use crate::format_source;

const CORPUS_ROOTS: &[&str] = &[
    "crates/harn-stdlib/src/stdlib",
    "conformance/tests",
    "crates/harn-cli/assets/demo",
    "experiments",
    "scripts",
    "personas",
    "tests",
    "examples",
    "evals",
];

const FORMATTER_SYNTAX_FIXTURES: &[&str] = &[
    "semicolon_statements.harn",
    "semicolon_if_else_invalid.harn",
    "semicolon_try_catch_invalid.harn",
    "semicolon_empty_statement_invalid.harn",
    "import_broken_module_lib.harn",
];

#[test]
fn current_harn_corpus_preserves_semantic_tokens() {
    // A few real stdlib modules contain deeply nested expressions. Match the
    // CLI's practical stack headroom instead of making the invariant depend on
    // libtest's comparatively small worker stack.
    std::thread::Builder::new()
        .name("harn-fmt-corpus-audit".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(audit_current_harn_corpus)
        .expect("failed to spawn corpus audit thread")
        .join()
        .expect("corpus audit thread panicked");
}

fn audit_current_harn_corpus() {
    let root = workspace_root();
    let mut files = Vec::new();
    for relative in CORPUS_ROOTS {
        collect_harn_files(&root.join(relative), &mut files).unwrap_or_else(|error| {
            panic!("failed to enumerate formatter corpus at {relative}: {error}")
        });
    }
    files.sort();

    assert!(
        !files.is_empty(),
        "formatter corpus unexpectedly contains no files"
    );
    let mut failures = Vec::new();
    for path in files {
        let Some(relative) = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_str()
            .map(str::to_owned)
        else {
            failures.push(format!("{path:?}: corpus path is not UTF-8"));
            continue;
        };
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                failures.push(format!("{relative}: cannot read UTF-8 source: {error}"));
                continue;
            }
        };
        let formatted = match format_source(&source) {
            Ok(formatted) => formatted,
            Err(error) => {
                failures.push(format!(
                    "{relative}: formatter rejected corpus source: {error}"
                ));
                continue;
            }
        };
        if let Err(error) = assert_same_semantic_tokens(&source, &formatted) {
            failures.push(format!("{relative}: {error}"));
        }
    }

    assert!(
        failures.is_empty(),
        "formatter changed semantic tokens:\n{}",
        failures.join("\n")
    );
}

/// Audits the mechanical `.harn` rewrite in a formatter branch against its
/// merge base. The default test suite exercises the opt-in boundary without
/// assuming that an arbitrary source branch is a mechanical formatter change.
/// Run it through `make audit-fmt-harn-tokens` to set the base explicitly.
#[test]
fn merge_base_harn_rewrite_preserves_semantic_tokens() {
    let root = workspace_root();
    let Ok(base_ref) = std::env::var("HARN_FMT_AUDIT_BASE") else {
        return;
    };
    let base = git_text(&root, &["merge-base", "HEAD", &base_ref])
        .unwrap_or_else(|error| panic!("cannot resolve merge base with {base_ref}: {error}"));
    let base = base.trim();
    assert!(!base.is_empty(), "git merge-base returned an empty commit");

    let changed = git(
        &root,
        &[
            "diff",
            "--name-status",
            "-z",
            "--find-renames",
            base,
            "HEAD",
            "--",
            "*.harn",
        ],
    )
    .unwrap_or_else(|error| panic!("cannot enumerate changed .harn files: {error}"));
    let pairs = comparable_harn_paths(&changed)
        .unwrap_or_else(|error| panic!("cannot parse git's changed-path output: {error}"));

    let mut failures = Vec::new();
    for (base_path, head_path) in &pairs {
        let before = git_blob(&root, base, base_path).unwrap_or_else(|error| {
            panic!("cannot read {base}:{base_path} for token audit: {error}")
        });
        let after = git_blob(&root, "HEAD", head_path).unwrap_or_else(|error| {
            panic!("cannot read HEAD:{head_path} for token audit: {error}")
        });
        if let Err(error) = assert_same_semantic_tokens(&before, &after) {
            failures.push(format!("{base_path} -> {head_path}: {error}"));
        }
    }

    assert!(
        failures.is_empty(),
        "merge-base .harn rewrite changed semantic tokens:\n{}",
        failures.join("\n")
    );
    eprintln!("audited {} comparable changed .harn files", pairs.len());
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("harn-fmt must live under <workspace>/crates")
        .to_path_buf()
}

fn collect_harn_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(directory).map_err(|error| error.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if entry.file_name() != OsStr::new(".harn") {
                collect_harn_files(&path, files)?;
            }
        } else if path.extension() == Some(OsStr::new("harn"))
            && !FORMATTER_SYNTAX_FIXTURES
                .iter()
                .any(|fixture| path.file_name() == Some(OsStr::new(fixture)))
        {
            files.push(path);
        }
    }
    Ok(())
}

fn assert_same_semantic_tokens(before: &str, after: &str) -> Result<(), String> {
    let before = semantic_tokens(before).map_err(|error| format!("before: {error}"))?;
    let after = semantic_tokens(after).map_err(|error| format!("after: {error}"))?;
    if before == after {
        return Ok(());
    }

    let first = before
        .iter()
        .zip(&after)
        .position(|(left, right)| left != right)
        .unwrap_or_else(|| before.len().min(after.len()));
    let start = first.saturating_sub(2);
    let before_end = (first + 3).min(before.len());
    let after_end = (first + 3).min(after.len());
    Err(format!(
        "first difference at token {first}; before[{start}..{before_end}]={:?}, after[{start}..{after_end}]={:?} ({} vs {} tokens)",
        &before[start..before_end],
        &after[start..after_end],
        before.len(),
        after.len(),
    ))
}

#[test]
fn semantic_token_allowances_are_narrow() {
    let compact = "fn f() {\n  value?.call(a,)\n}\n";
    let laid_out = "fn f() {\n\n  value?.call(\n    a,\n  )\n}\n";
    assert_same_semantic_tokens(compact, laid_out).unwrap();

    let strict = laid_out.replace("?.", ".");
    assert!(assert_same_semantic_tokens(laid_out, &strict).is_err());
    let without_semicolon = "fn f() {\n  let a = 1\n  let b = 2\n}\n";
    let with_semicolon = "fn f() {\n  let a = 1; let b = 2\n}\n";
    assert!(assert_same_semantic_tokens(with_semicolon, without_semicolon).is_err());

    let crlf = "// comment\r\nfn f() {\r\n}\r\n";
    let lf = "// comment\nfn f() {\n}\n";
    assert_same_semantic_tokens(crlf, lf).unwrap();
}

fn semantic_tokens(source: &str) -> Result<Vec<TokenKind>, String> {
    let tokens = Lexer::new(source)
        .tokenize_with_comments()
        .map_err(|error| error.to_string())?;
    let parser_tokens = tokens
        .iter()
        .filter(|token| {
            !matches!(
                token.kind,
                TokenKind::LineComment { .. } | TokenKind::BlockComment { .. }
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    Parser::new(parser_tokens.clone())
        .parse()
        .map_err(|error| format!("source does not parse: {error}"))?;

    // Only commas directly before a closer in a successfully parsed stream are
    // layout-owned trailing commas. No arbitrary comma is hidden.
    let significant_tokens = parser_tokens
        .iter()
        .filter(|token| !matches!(token.kind, TokenKind::Newline))
        .collect::<Vec<_>>();
    let trailing_commas = significant_tokens
        .windows(2)
        .filter_map(|pair| {
            (matches!(pair[0].kind, TokenKind::Comma)
                && matches!(
                    pair[1].kind,
                    TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace
                ))
            .then_some(pair[0].span.start)
        })
        .collect::<HashSet<_>>();

    Ok(tokens
        .into_iter()
        .filter_map(|token| match token.kind {
            TokenKind::Newline => None,
            TokenKind::Comma if trailing_commas.contains(&token.span.start) => None,
            TokenKind::InterpolatedString(segments) => Some(TokenKind::InterpolatedString(
                segments
                    .into_iter()
                    .map(|segment| match segment {
                        StringSegment::Literal(text) => StringSegment::Literal(text),
                        StringSegment::Expression(text, _, _) => {
                            StringSegment::Expression(text, 0, 0)
                        }
                    })
                    .collect(),
            )),
            TokenKind::LineComment { text, is_doc } => Some(TokenKind::LineComment {
                text: text.trim_end_matches('\r').to_owned(),
                is_doc,
            }),
            TokenKind::BlockComment { text, is_doc } => Some(TokenKind::BlockComment {
                text: normalize_comment_line_endings(text),
                is_doc,
            }),
            kind => Some(kind),
        })
        .collect())
}

/// Block-comment line endings are layout, not source semantics. The formatter
/// emits canonical LF output, so the corpus audit must compare block-comment
/// content after normalizing platform-specific line endings.
fn normalize_comment_line_endings(text: String) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|error| format!("failed to execute git: {error}"))?;
    checked_output(output, args)
}

fn git_text(root: &Path, args: &[&str]) -> Result<String, String> {
    String::from_utf8(git(root, args)?).map_err(|error| format!("git emitted non-UTF-8: {error}"))
}

fn checked_output(output: Output, args: &[&str]) -> Result<Vec<u8>, String> {
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(format!(
            "git {} failed with {}: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
        ))
    }
}

fn git_blob(root: &Path, commit: &str, path: &str) -> Result<String, String> {
    let object = format!("{commit}:{path}");
    String::from_utf8(git(root, &["show", "--no-ext-diff", &object])?)
        .map_err(|error| format!("blob is not UTF-8: {error}"))
}

fn comparable_harn_paths(output: &[u8]) -> Result<Vec<(String, String)>, String> {
    let fields = output
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    let mut pairs = Vec::new();
    let mut index = 0;
    while index < fields.len() {
        let status = utf8_field(fields[index], "status")?;
        index += 1;
        let kind = status
            .as_bytes()
            .first()
            .copied()
            .ok_or_else(|| "empty change status".to_string())?;
        match kind {
            b'R' | b'C' => {
                let old = next_path(&fields, &mut index, &status)?;
                let new = next_path(&fields, &mut index, &status)?;
                pairs.push((old, new));
            }
            b'M' => {
                let path = next_path(&fields, &mut index, &status)?;
                pairs.push((path.clone(), path));
            }
            b'A' | b'D' => {
                let path = next_path(&fields, &mut index, &status)?;
                return Err(format!(
                    "cannot prove token preservation for {status} path {path:?}"
                ));
            }
            _ => return Err(format!("unsupported git change status {status:?}")),
        }
    }
    Ok(pairs)
}

fn next_path(fields: &[&[u8]], index: &mut usize, status: &str) -> Result<String, String> {
    let field = fields
        .get(*index)
        .ok_or_else(|| format!("missing path after git status {status:?}"))?;
    *index += 1;
    utf8_field(field, "path")
}

fn utf8_field(field: &[u8], label: &str) -> Result<String, String> {
    std::str::from_utf8(field)
        .map(str::to_owned)
        .map_err(|error| format!("git emitted a non-UTF-8 {label}: {error}"))
}
