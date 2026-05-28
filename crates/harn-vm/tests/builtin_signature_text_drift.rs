//! Drift test between each builtin's raw `sig = "..."` text and the
//! [`BuiltinSignature`] that `#[harn_builtin]` parsed it into.
//!
//! The proc-macro stores both representations on every `VmBuiltinDef`:
//!
//! * `signature_text` — the literal string the human typed.
//! * `sig` (a parsed `BuiltinSignature` struct) — the AST the typechecker
//!   actually consumes.
//!
//! If these ever disagree structurally (e.g. a future parser tweak silently
//! changes how `a | b | c` associates, or `...rest` loses its `has_rest`
//! bit, or shape fields get reordered), tooling that surfaces
//! `signature_text` to the user would show one thing while the typechecker
//! enforces another. This test catches that by rendering the parsed
//! `BuiltinSignature` back through its `Display` impl and comparing — after
//! canonicalization — against the raw text.
//!
//! Canonicalization handles documented sugar (the sig parser desugars these
//! into the same `Ty` shape, so `Display` picks one canonical rendering):
//! * Whitespace differences (`a:dict` vs `a: dict`) — squashed.
//! * `T?` and `T|nil` — normalized to `T?` (Display's choice).
//! * `int|float` and `number` — normalized to `number`.

use harn_builtin_meta::BuiltinSignature;

/// Recognized type-name first chars in canonicalization. Sig type names are
/// always lowercase identifiers (`dict`, `string`, …) plus the
/// uppercase-leading `Schema` and user generic params.
fn is_typename_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_typename_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// True when the previous byte indicates a type position in the squashed sig
/// stream: after `:` (param/field type), `<` (apply arg), `>` (after `->`),
/// `|` (union continuation), `(` / `,` (fn-type params).
fn at_type_pos(prev: u8) -> bool {
    matches!(prev, b':' | b'<' | b'>' | b'|' | b'(' | b',')
}

/// Normalize a sig string so equivalent grammatical forms compare equal:
/// strip whitespace, then rewrite `T|nil` → `T?` and `int|float` → `number`
/// in type positions.
fn canonical(s: &str) -> String {
    let squashed: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = squashed.as_bytes();
    let mut out = String::with_capacity(squashed.len());
    let mut i = 0;
    while i < bytes.len() {
        let prev = if i == 0 { b'\0' } else { bytes[i - 1] };
        if at_type_pos(prev) && is_typename_start(bytes[i]) {
            let start = i;
            while i < bytes.len() && is_typename_cont(bytes[i]) {
                i += 1;
            }
            let name = &squashed[start..i];
            // `int|float` (when not part of a wider union) → `number`.
            if name == "int"
                && squashed[i..].starts_with("|float")
                && bytes
                    .get(i + 6)
                    .map(|c| matches!(*c, b',' | b')' | b'>' | b'}' | b'\0'))
                    .unwrap_or(true)
            {
                out.push_str("number");
                i += 6;
                continue;
            }
            // `T|nil` (when not part of a wider union) → `T?`.
            if squashed[i..].starts_with("|nil") {
                let after = i + 4;
                let terminates = bytes
                    .get(after)
                    .map(|c| matches!(*c, b',' | b')' | b'>' | b'}'))
                    .unwrap_or(true);
                if terminates && name != "nil" {
                    out.push_str(name);
                    out.push('?');
                    i = after;
                    continue;
                }
            }
            out.push_str(name);
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

#[test]
fn every_builtin_signature_text_round_trips_through_display() {
    let mut mismatches: Vec<String> = Vec::new();

    for def in harn_vm::stdlib::macros::ALL_BUILTIN_DEFS.iter() {
        let Some(raw_text) = def.signature_text else {
            // `runtime_only = true` / `parser_only = true` builtins legitimately
            // omit the text; nothing to compare.
            continue;
        };

        let rendered = format!("{}", &def.sig as &BuiltinSignature);
        if canonical(raw_text) != canonical(&rendered) {
            mismatches.push(format!(
                "builtin {:?}:\n  raw:      {}\n  rendered: {}\n  raw(canon):      {}\n  rendered(canon): {}",
                def.sig.name,
                raw_text,
                rendered,
                canonical(raw_text),
                canonical(&rendered),
            ));
        }
    }

    assert!(
        mismatches.is_empty(),
        "signature_text drifted from BuiltinSignature for {} builtin(s):\n\n{}",
        mismatches.len(),
        mismatches.join("\n\n")
    );
}

#[test]
fn signature_text_present_for_user_visible_builtins() {
    // Sanity: most non-runtime-only entries should have populated text.
    // This isn't a strict contract (some categories deliberately suppress
    // it), but a near-total absence would indicate the macro stopped
    // forwarding the literal.
    let total = harn_vm::stdlib::macros::ALL_BUILTIN_DEFS.len();
    let with_text = harn_vm::stdlib::macros::ALL_BUILTIN_DEFS
        .iter()
        .filter(|d| d.signature_text.is_some())
        .count();
    assert!(
        with_text * 2 > total,
        "fewer than half of builtins have signature_text populated ({with_text}/{total}); \
         the proc-macro may have stopped forwarding `sig = \"...\"` to signature_text"
    );
}
