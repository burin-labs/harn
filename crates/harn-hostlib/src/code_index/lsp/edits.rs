//! LSP positions, URIs and `WorkspaceEdit` lowering into [`FilePlan`]s.
//!
//! Only the subset of the protocol a rename result uses is modelled. A
//! response that needs anything else (resource operations, annotations that
//! require confirmation) is reported as unsupported rather than guessed at.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde::{Deserialize, Serialize};

use super::super::refactor_core::{EditSpan, IdentifierSpan};

pub(crate) use super::super::source_positions::{Encoding, LineIndex, Position};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TextEdit {
    pub range: Range,
    pub new_text: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct VersionedDocument {
    pub uri: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum DocumentChange {
    Edit {
        #[serde(rename = "textDocument")]
        text_document: VersionedDocument,
        edits: Vec<TextEdit>,
    },
    /// `create` / `rename` / `delete`; the payload is irrelevant because any
    /// resource operation makes the edit unsupported.
    Resource { kind: String },
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceEdit {
    #[serde(default)]
    pub changes: Option<BTreeMap<String, Vec<TextEdit>>>,
    #[serde(default)]
    pub document_changes: Option<Vec<DocumentChange>>,
}

impl WorkspaceEdit {
    /// Per-file edits keyed by filesystem path. `Err` names the first
    /// construct this client does not apply.
    pub(crate) fn into_file_edits(self) -> Result<BTreeMap<PathBuf, Vec<TextEdit>>, String> {
        let mut out: BTreeMap<PathBuf, Vec<TextEdit>> = BTreeMap::new();
        if let Some(changes) = self.document_changes {
            for change in changes {
                match change {
                    DocumentChange::Edit {
                        text_document,
                        edits,
                    } => out
                        .entry(uri_to_path(&text_document.uri)?)
                        .or_default()
                        .extend(edits),
                    DocumentChange::Resource { kind } => {
                        return Err(format!("the server's edit needs a `{kind}` file operation"));
                    }
                }
            }
        } else if let Some(changes) = self.changes {
            for (uri, edits) in changes {
                out.entry(uri_to_path(&uri)?).or_default().extend(edits);
            }
        }
        Ok(out)
    }
}

impl Encoding {
    pub(crate) fn from_wire(raw: Option<&str>) -> Self {
        match raw {
            Some("utf-8") => Self::Utf8,
            _ => Self::Utf16,
        }
    }
}

impl LineIndex<'_> {
    fn span(&self, start: usize, end: usize) -> IdentifierSpan {
        let start_pos = self.position(start, Encoding::Utf8);
        let end_pos = self.position(end, Encoding::Utf8);
        IdentifierSpan {
            start_byte: start,
            end_byte: end,
            start_row: start_pos.line as usize,
            start_col: start_pos.character as usize,
            end_row: end_pos.line as usize,
            end_col: end_pos.character as usize,
        }
    }
}

/// Lower one file's text edits against the bytes Harn read. `Err` means the
/// server's view of the file disagrees with ours (out-of-range or
/// overlapping edits), which the caller treats as a stale document.
pub(crate) fn lower_edits(
    source: &str,
    edits: &[TextEdit],
    encoding: Encoding,
) -> Result<Vec<EditSpan>, String> {
    let index = LineIndex::new(source);
    let mut spans = Vec::with_capacity(edits.len());
    for edit in edits {
        let (Some(start), Some(end)) = (
            index.offset(edit.range.start, encoding),
            index.offset(edit.range.end, encoding),
        ) else {
            return Err(format!(
                "edit range {}:{}-{}:{} is outside the document",
                edit.range.start.line,
                edit.range.start.character,
                edit.range.end.line,
                edit.range.end.character
            ));
        };
        if end < start {
            return Err("edit range ends before it starts".into());
        }
        let before = source.get(start..end).unwrap_or_default().to_string();
        spans.push(EditSpan {
            span: index.span(start, end),
            before,
            after: edit.new_text.clone(),
        });
    }
    spans.sort_by_key(|edit| edit.span.start_byte);
    if spans
        .windows(2)
        .any(|pair| pair[1].span.start_byte < pair[0].span.end_byte)
    {
        return Err("the server returned overlapping edits".into());
    }
    Ok(spans)
}

/// RFC 3986 unreserved characters plus `/` and `:` (Windows drive letters).
const FILE_URI_PATH: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~')
    .remove(b'/')
    .remove(b':');

/// `file://` URI for an absolute path, percent-encoding everything outside
/// the RFC 3986 unreserved set and the path separator.
pub(crate) fn path_to_uri(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !raw.starts_with('/') {
        out.push('/');
    }
    out.extend(utf8_percent_encode(&raw, FILE_URI_PATH));
    out
}

pub(crate) fn uri_to_path(uri: &str) -> Result<PathBuf, String> {
    let rest = uri
        .strip_prefix("file://")
        .ok_or_else(|| format!("unsupported document URI `{uri}`"))?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let text = percent_decode_str(rest)
        .decode_utf8()
        .map_err(|_| format!("non-UTF-8 URI `{uri}`"))?;
    // `file:///C:/x` names `C:/x` on Windows.
    let windows_drive = text.len() > 2 && text.as_bytes()[2] == b':' && text.starts_with('/');
    Ok(PathBuf::from(if cfg!(windows) && windows_drive {
        text.strip_prefix('/').unwrap_or(&text)
    } else {
        &text
    }))
}

#[cfg(test)]
#[path = "edits_tests.rs"]
mod tests;
