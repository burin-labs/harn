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

/// Words that read as a Harn type but name none, with the tag to use
/// instead.
const NON_CANONICAL: &[(&str, &str)] = &[
    ("integer", "int"),
    ("boolean", "bool"),
    ("record", "dict"),
    ("object", "dict"),
    ("array", "list"),
    ("number", "int or a float"),
];

/// A type claim: `must be`/`expected`, an optional article, an optional
/// adjective, then the noun. Matching the whole phrase rather than the bare
/// word is what lets `must be a positive integer` fail while `trust.record:`
/// and a `record.attempt` field access pass.
fn type_claim_pattern() -> regex::Regex {
    let nouns = NON_CANONICAL
        .iter()
        .map(|(bad, _)| *bad)
        .collect::<Vec<_>>()
        .join("|");
    regex::Regex::new(&format!(
        r"(?:must be|must contain|expected|requires)(?: an?| the)?(?: [a-z][a-z-]*)? ({nouns})\b"
    ))
    .expect("type-claim pattern compiles")
}

/// Opt-out marker for messages that describe something other than a Harn
/// value — a JSON document, a migration file name, a template token. Honoured
/// on the offending line or the line above it, so a long message can carry the
/// reason on its own line.
const OPT_OUT: &str = "not-a-harn-type:";

fn stdlib_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/stdlib")
}

fn visit(dir: &Path, root: &Path, claim: &regex::Regex, found: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).expect("read stdlib dir") {
        let path = entry.expect("stdlib dir entry").path();
        if path.is_dir() {
            visit(&path, root, claim, found);
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
            // Only diagnostics are checked; a comment explaining the code can
            // use whatever words it likes.
            if line.trim_start().starts_with("//") {
                continue;
            }
            // The opt-out sits on the message's line or up to two lines above
            // it, since a wrapped `format!` puts the noun below the comment.
            let opted_out = (number.saturating_sub(2)..=number)
                .filter_map(|index| lines.get(index))
                .any(|line| line.contains(OPT_OUT));
            if opted_out {
                continue;
            }
            // A claim and its noun can land on separate wrapped lines, so read
            // each line joined to the one before it — and report only when the
            // noun itself is on this line, so a match is not counted twice.
            let prior = number
                .checked_sub(1)
                .and_then(|index| lines.get(index))
                .filter(|line| !line.trim_start().starts_with("//"))
                .map(|line| line.trim_end())
                .unwrap_or_default();
            let offset = if prior.is_empty() { 0 } else { prior.len() + 1 };
            let context = format!("{prior} {line}");
            for capture in claim.captures_iter(context.trim_start()) {
                let noun = capture.get(1).expect("group 1 is the noun");
                if noun.start() + context.len() - context.trim_start().len() < offset {
                    continue;
                }
                let fix = NON_CANONICAL
                    .iter()
                    .find(|(bad, _)| *bad == noun.as_str())
                    .map(|(_, fix)| *fix)
                    .expect("captured noun comes from NON_CANONICAL");
                let relative = path.strip_prefix(root).unwrap_or(&path).display();
                found.push(format!(
                    "{relative}:{}: `{}` -> `{fix}`",
                    number + 1,
                    noun.as_str()
                ));
            }
        }
    }
}

#[test]
fn stdlib_diagnostics_name_types_the_runtime_has() {
    let root = stdlib_root();
    let mut found = Vec::new();
    visit(&root, &root, &type_claim_pattern(), &mut found);
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
    let recommended: BTreeSet<&str> = NON_CANONICAL
        .iter()
        .flat_map(|(_, fix)| fix.split(" or a "))
        .collect();
    for word in recommended {
        assert!(
            tag_is_canonical(word),
            "recommended spelling `{word}` is not a runtime type tag"
        );
    }
}

#[test]
fn the_pattern_matches_claims_and_not_identifiers() {
    let claim = type_claim_pattern();
    for asserted in [
        "value must be an integer",
        "ahead must be a positive integer",
        "`indent` requires an integer width",
        "expected an integer millisecond timestamp",
        "entries must contain an array",
    ] {
        assert!(claim.is_match(asserted), "should flag: {asserted}");
    }
    for innocent in [
        "\"trust.record: expected decision dict\"",
        "if record.attempt != expected {",
        "/// the record crosses the boundary",
        "let integer_like = 3;",
    ] {
        assert!(!claim.is_match(innocent), "should not flag: {innocent}");
    }
}
