//! Doc comments and `@annotations` projected onto MCP tools.
//!
//! The script already has a title, a description, and (optionally) behavior
//! hints. This module reads those and stops at the declaration — inventing a
//! "destructive" or "open-world" hint the author never wrote would be a
//! safety claim nobody made.

use harn_parser::{Attribute, Node};

use super::{ExportDiagnostic, ANNOTATIONS_BAD_ARGS};

/// Behavior hints declared with `@annotations(...)`, in MCP's vocabulary.
///
/// Every field is optional and stays `None` unless the script says. An omitted
/// hint is not the same as a false one: MCP defines its own defaults for a
/// missing hint (a tool is assumed destructive and open-world), and asserting
/// otherwise on a script's behalf would be a safety claim nobody made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToolAnnotations {
    /// The tool does not modify its environment.
    pub read_only: Option<bool>,
    /// The tool may perform destructive updates. Meaningful only when the tool
    /// is not read-only.
    pub destructive: Option<bool>,
    /// Repeated calls with the same arguments have no additional effect.
    pub idempotent: Option<bool>,
    /// The tool interacts with entities outside its own closed world.
    pub open_world: Option<bool>,
}

impl ToolAnnotations {
    fn is_empty(self) -> bool {
        self == Self::default()
    }
}

/// Read `@annotations(readOnly: true, idempotent: true, openWorld: false)`.
///
/// Named-argument spellings are camelCase to match the wire field they become
/// (`readOnlyHint`), so an author writing the attribute and an author reading
/// the protocol see the same word.
pub(super) fn annotations_from_attributes(
    attrs: &[Attribute],
    fn_name: &str,
    diagnostics: &mut Vec<ExportDiagnostic>,
) -> Option<ToolAnnotations> {
    let attr = attrs.iter().find(|attr| attr.name == "annotations")?;
    let mut annotations = ToolAnnotations::default();
    for arg in &attr.args {
        let Some(key) = arg.name.as_deref() else {
            diagnostics.push(ExportDiagnostic {
                code: ANNOTATIONS_BAD_ARGS,
                line: arg.span.line,
                message: format!(
                    "`@annotations` on `{fn_name}` takes named boolean arguments \
                     (`@annotations(readOnly: true)`); positional argument ignored"
                ),
            });
            continue;
        };
        let Node::BoolLiteral(value) = arg.value.node else {
            diagnostics.push(ExportDiagnostic {
                code: ANNOTATIONS_BAD_ARGS,
                line: arg.span.line,
                message: format!(
                    "`@annotations({key}:)` on `{fn_name}` requires a boolean literal; hint ignored"
                ),
            });
            continue;
        };
        match key {
            "readOnly" => annotations.read_only = Some(value),
            "destructive" => annotations.destructive = Some(value),
            "idempotent" => annotations.idempotent = Some(value),
            "openWorld" => annotations.open_world = Some(value),
            _ => diagnostics.push(ExportDiagnostic {
                code: ANNOTATIONS_BAD_ARGS,
                line: arg.span.line,
                message: format!(
                    "`@annotations` on `{fn_name}` does not define `{key}`; expected one of \
                     `readOnly`, `destructive`, `idempotent`, `openWorld`. Hint ignored"
                ),
            }),
        }
    }
    (!annotations.is_empty()).then_some(annotations)
}

/// Strip the comment markers off one doc-comment body.
fn doc_line_text(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed == "*/" || trimmed == "/**" {
        return Some("");
    }
    for prefix in ["///", "/**", "*"] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let rest = rest.strip_suffix("*/").unwrap_or(rest);
            return Some(rest.trim());
        }
    }
    None
}

/// The doc comment immediately above the declaration starting at `decl_line`.
///
/// Anchored on the parser's own span rather than matched out of the file, so a
/// `pub fn` mentioned inside a string or a nested block cannot pick up the
/// wrong comment. Attribute lines sit between the comment and the declaration
/// and are skipped; a blank line between them ends the association, because a
/// comment separated from a declaration is a comment about something else.
pub(super) fn doc_comment_above(lines: &[&str], decl_line: usize) -> Option<String> {
    if decl_line == 0 {
        return None;
    }
    let mut index = decl_line - 1;
    while index > 0 {
        let trimmed = lines[index - 1].trim();
        if trimmed.starts_with('@') {
            index -= 1;
            continue;
        }
        break;
    }
    let mut body: Vec<String> = Vec::new();
    while index > 0 {
        let Some(text) = doc_line_text(lines[index - 1]) else {
            break;
        };
        body.push(text.to_string());
        let opened_block = lines[index - 1].trim().starts_with("/**");
        index -= 1;
        if opened_block {
            break;
        }
    }
    body.reverse();
    let joined = body.join("\n");
    let trimmed = joined.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// The script's own leading doc comment, if it opens with one.
///
/// Only a comment before every declaration counts: this becomes the server's
/// `instructions`, and a comment that actually belongs to the first function
/// would describe one tool while claiming to describe the server.
pub(super) fn module_doc_comment(lines: &[&str]) -> Option<String> {
    let mut body: Vec<String> = Vec::new();
    let mut started = false;
    for line in lines {
        let trimmed = line.trim();
        if !started {
            if trimmed.is_empty() || trimmed.starts_with("#!") {
                continue;
            }
            if !trimmed.starts_with("/**") && !trimmed.starts_with("///") {
                return None;
            }
            started = true;
        }
        match doc_line_text(line) {
            Some(text) => body.push(text.to_string()),
            None => break,
        }
        if trimmed.ends_with("*/") && trimmed != "/**" {
            break;
        }
    }
    let joined = body.join("\n");
    let trimmed = joined.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Split a doc comment into a short title (first line) and the full body.
pub(super) fn title_and_description(doc: Option<String>) -> (Option<String>, Option<String>) {
    let Some(doc) = doc else {
        return (None, None);
    };
    let first = doc.lines().next().unwrap_or("").trim();
    let title = if !first.is_empty() && first.chars().count() <= 80 {
        Some(first.to_string())
    } else {
        None
    };
    (title, Some(doc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExportCatalog;

    fn catalog_from_source(source: &str) -> ExportCatalog {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.harn");
        std::fs::write(&path, source).expect("write script");
        ExportCatalog::from_path(&path).expect("catalog")
    }

    #[test]
    fn export_catalog_reads_doc_comments_and_annotation_hints() {
        let catalog = catalog_from_source(
            r#"
/**
 * Eval debugger
 *
 * Start with find_runs.
 */
@annotations(readOnly: true, idempotent: true, openWorld: false)
/// Find eval runs
///
/// Lists recent evals. Does not start one.
pub fn find_runs() -> string { return "ok" }

pub fn ping() -> string { return "pong" }
"#,
        );
        assert_eq!(
            catalog.instructions.as_deref(),
            Some("Eval debugger\n\nStart with find_runs.")
        );
        let find_runs = catalog.function("find_runs").expect("find_runs");
        assert_eq!(find_runs.title.as_deref(), Some("Find eval runs"));
        assert_eq!(
            find_runs.description.as_deref(),
            Some("Find eval runs\n\nLists recent evals. Does not start one.")
        );
        let hints = find_runs.annotations.expect("declared hints");
        assert_eq!(hints.read_only, Some(true));
        assert_eq!(hints.idempotent, Some(true));
        assert_eq!(hints.open_world, Some(false));
        assert_eq!(hints.destructive, None);
        assert!(catalog
            .function("ping")
            .expect("ping")
            .annotations
            .is_none());
        assert!(
            catalog.diagnostics().is_empty(),
            "{:?}",
            catalog.diagnostics()
        );
    }

    #[test]
    fn annotations_with_unknown_key_are_diagnosed() {
        let catalog = catalog_from_source(
            r#"
@annotations(readOnly: true, spicy: true)
pub fn peek() -> string { return "ok" }
"#,
        );
        let peek = catalog.function("peek").expect("peek");
        assert_eq!(
            peek.annotations.expect("kept valid hint").read_only,
            Some(true)
        );
        let codes: Vec<&str> = catalog.diagnostics().iter().map(|d| d.code).collect();
        assert_eq!(codes, vec![ANNOTATIONS_BAD_ARGS]);
    }

    #[test]
    fn title_comes_from_the_first_doc_line() {
        let (title, description) =
            title_and_description(Some("Find eval runs\n\nLists recent evals.".to_string()));
        assert_eq!(title.as_deref(), Some("Find eval runs"));
        assert_eq!(
            description.as_deref(),
            Some("Find eval runs\n\nLists recent evals.")
        );
    }
}
