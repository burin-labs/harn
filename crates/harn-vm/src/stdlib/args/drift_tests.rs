//! Keeps hand-written diagnostics on the canonical type vocabulary.
//!
//! [`Expected`](super::Expected) makes the type names in messages built
//! through [`Args`](super::Args) structurally correct, but a builtin can
//! still `format!` a message itself. This test walks the stdlib source and
//! fails on the spellings that are not types the runtime has — the ones that
//! actually accumulated before the contract existed: `integer` for `int`,
//! `boolean` for `bool`, `number` for `int`/`float`, `record` for `dict`.
//!
//! A message genuinely about a JSON document, a file name, or a token stream
//! is not describing a Harn value, so it opts out with a
//! `// not-a-harn-type: <reason>` comment on the message's line or the line
//! above it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::tag_is_canonical;

/// Spellings that read as a Harn type but name none. Each maps to what the
/// author should write instead, so the failure tells you the fix.
const NON_CANONICAL: &[(&str, &str)] = &[
    ("must be an integer", "must be an int"),
    ("must be a boolean", "must be a bool"),
    ("must be a number", "must be an int or a float"),
    ("must be a record", "must be a dict"),
    ("must be an object", "must be a dict"),
    ("must be an array", "must be a list"),
    ("expected integer", "expected int"),
    ("expected boolean", "expected bool"),
    ("expected number", "expected int or float"),
    ("expected object", "expected dict"),
    ("expected array", "expected list"),
    ("expected record", "expected dict"),
];

/// Opt-out marker for messages that describe something other than a Harn
/// value — a JSON document, a migration file name, a template token. Honoured
/// on the offending line or the line above it, so a long message can carry the
/// reason on its own line.
const OPT_OUT: &str = "not-a-harn-type:";

fn stdlib_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/stdlib")
}

fn visit(dir: &Path, root: &Path, found: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).expect("read stdlib dir") {
        let path = entry.expect("stdlib dir entry").path();
        if path.is_dir() {
            visit(&path, root, found);
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        // This module's own doc comments quote the bad spellings on purpose.
        if path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == Some("args")
        {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read stdlib source");
        let lines: Vec<&str> = source.lines().collect();
        for (number, line) in lines.iter().enumerate() {
            let opted_out = line.contains(OPT_OUT)
                || number
                    .checked_sub(1)
                    .and_then(|previous| lines.get(previous))
                    .is_some_and(|previous| previous.contains(OPT_OUT));
            if opted_out {
                continue;
            }
            for (bad, fix) in NON_CANONICAL {
                if line.contains(bad) {
                    let relative = path.strip_prefix(root).unwrap_or(&path).display();
                    found.push(format!("{relative}:{}: `{bad}` -> `{fix}`", number + 1));
                }
            }
        }
    }
}

#[test]
fn stdlib_diagnostics_name_types_the_runtime_has() {
    let root = stdlib_root();
    let mut found = Vec::new();
    visit(&root, &root, &mut found);
    found.sort();
    assert!(
        found.is_empty(),
        "stdlib diagnostics name types `type_of` never returns. Prefer building the \
         message with `Args`/`Options` so the vocabulary is structural; otherwise use \
         the canonical spelling, or mark the line `// {OPT_OUT} <reason>` when it \
         describes something other than a Harn value.\n  {}",
        found.join("\n  ")
    );
}

/// The replacements this test recommends have to be canonical themselves.
#[test]
fn recommended_spellings_are_canonical() {
    let mut recommended = BTreeSet::new();
    for (_, fix) in NON_CANONICAL {
        for word in fix.split_whitespace() {
            if matches!(word, "must" | "be" | "a" | "an" | "or" | "expected") {
                continue;
            }
            recommended.insert(word);
        }
    }
    for word in recommended {
        assert!(
            tag_is_canonical(word),
            "recommended spelling `{word}` is not a runtime type tag"
        );
    }
}
