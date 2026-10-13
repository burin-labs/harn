//! The generated method reference in `docs/src/builtins.md`, rendered from
//! the registry so the docs table cannot drift from the methods that exist.

use super::{methods_of, MethodReceiver, REMOVED_METHODS};

pub const REFERENCE_BEGIN_MARKER: &str =
    "<!-- BEGIN GENERATED: builtin methods (make gen-builtin-methods) -->";
pub const REFERENCE_END_MARKER: &str = "<!-- END GENERATED: builtin methods -->";

const SECTIONS: [(MethodReceiver, &str); 6] = [
    (MethodReceiver::String, "String methods"),
    (MethodReceiver::List, "List methods"),
    (MethodReceiver::Set, "Set methods"),
    (MethodReceiver::Dict, "Dict methods"),
    (MethodReceiver::Range, "Range methods"),
    (MethodReceiver::Iter, "Iterator methods"),
];

/// Markdown tables for every builtin receiver, without the region markers.
pub fn render_reference() -> String {
    let mut out = String::new();
    for (receiver, title) in SECTIONS {
        out.push_str(&format!("### {title}\n\n"));
        out.push_str("| Method | Signature | Mutates | Description |\n");
        out.push_str("|---|---|---|---|\n");
        for method in methods_of(receiver) {
            let mutates = if method.is_mutating() { "yes" } else { "" };
            out.push_str(&format!(
                "| `.{}()` | `{}` | {mutates} | {} |\n",
                method.name(),
                escape_cell(&method.signature.to_string()),
                escape_cell(method.doc),
            ));
        }
        out.push('\n');
    }
    out.push_str("### Removed method spellings\n\n");
    out.push_str(
        "Each of these is a type error (`HARN-NAM-013`).\n\
         `harn fix --apply --safety behavior-preserving` rewrites the ones with a\n\
         mechanical replacement.\n\n",
    );
    out.push_str("| Receiver | Removed | Use instead |\n|---|---|---|\n");
    for removed in REMOVED_METHODS {
        out.push_str(&format!(
            "| `{}` | `.{}()` | {} |\n",
            removed.receiver.type_name(),
            removed.name,
            escape_cell(&removed.repair_hint()),
        ));
    }
    out
}

/// Replace the generated region of `document` with a fresh rendering.
/// `None` when the document lacks the markers.
pub fn splice_reference(document: &str) -> Option<String> {
    let begin = document.find(REFERENCE_BEGIN_MARKER)?;
    let end = document.find(REFERENCE_END_MARKER)?;
    if end < begin {
        return None;
    }
    let head = document.get(..begin + REFERENCE_BEGIN_MARKER.len())?;
    let tail = document.get(end..)?;
    Some(format!(
        "{head}\n\n{}\n{tail}",
        render_reference().trim_end()
    ))
}

fn escape_cell(text: &str) -> String {
    text.replace('|', "\\|")
}
