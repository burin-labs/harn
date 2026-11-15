//! LSP positions, URIs and `WorkspaceEdit` lowering into [`FilePlan`]s.
//!
//! Resource changes retain their order until the filesystem planner can prove
//! their preconditions. Text-only callers refuse them before applying edits.

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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TextEdit {
    pub range: Range,
    pub new_text: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct VersionedDocument {
    pub uri: String,
    pub version: Option<i32>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CreateOptions {
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default)]
    pub ignore_if_exists: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DeleteOptions {
    #[serde(default, rename = "recursive")]
    pub _recursive: bool,
    #[serde(default, rename = "ignoreIfNotExists")]
    pub _ignore_if_not_exists: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub(crate) enum ResourceOperation {
    Create {
        #[serde(rename = "uri")]
        _uri: String,
        #[serde(default, rename = "options")]
        _options: CreateOptions,
        #[serde(rename = "annotationId")]
        _annotation_id: Option<String>,
    },
    Rename {
        #[serde(rename = "oldUri")]
        old_uri: String,
        #[serde(rename = "newUri")]
        new_uri: String,
        #[serde(default)]
        options: CreateOptions,
        #[serde(rename = "annotationId")]
        annotation_id: Option<String>,
    },
    Delete {
        #[serde(rename = "uri")]
        _uri: String,
        #[serde(default, rename = "options")]
        _options: DeleteOptions,
        #[serde(rename = "annotationId")]
        _annotation_id: Option<String>,
    },
}

impl ResourceOperation {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Create { .. } => "create",
            Self::Rename { .. } => "rename",
            Self::Delete { .. } => "delete",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub(crate) enum DocumentChange {
    Edit {
        #[serde(rename = "textDocument")]
        text_document: VersionedDocument,
        edits: Vec<TextEdit>,
    },
    Resource(ResourceOperation),
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceEdit {
    #[serde(default)]
    pub changes: Option<BTreeMap<String, Vec<TextEdit>>>,
    #[serde(default)]
    pub document_changes: Option<Vec<DocumentChange>>,
}

pub(crate) struct DocumentEdits {
    pub version: Option<i32>,
    pub edits: Vec<TextEdit>,
    pub rename_to: Option<PathBuf>,
}

impl WorkspaceEdit {
    /// Per-file edits keyed by filesystem path. `Err` names the first
    /// construct this client does not apply.
    #[cfg(test)]
    pub(crate) fn into_file_edits(self) -> Result<BTreeMap<PathBuf, Vec<TextEdit>>, String> {
        Ok(self
            .into_versioned_file_edits()?
            .into_iter()
            .map(|(path, document)| (path, document.edits))
            .collect())
    }

    #[cfg(test)]
    pub(crate) fn into_versioned_file_edits(
        self,
    ) -> Result<BTreeMap<PathBuf, DocumentEdits>, String> {
        self.into_rename_file_edits(None)
    }

    pub(crate) fn into_rename_file_edits(
        self,
        allowed_rename: Option<(&Path, &Path)>,
    ) -> Result<BTreeMap<PathBuf, DocumentEdits>, String> {
        let mut out = BTreeMap::new();
        if self.document_changes.is_some()
            && self
                .changes
                .as_ref()
                .is_some_and(|changes| !changes.is_empty())
        {
            return Err("the server mixed changes and documentChanges".into());
        }
        if let Some(changes) = self.document_changes {
            let mut resource_started = false;
            for change in changes {
                match change {
                    DocumentChange::Edit {
                        text_document,
                        edits,
                    } => {
                        if resource_started {
                            return Err("text edits after file operations are unsupported".into());
                        }
                        if out
                            .insert(
                                uri_to_path(&text_document.uri)?,
                                DocumentEdits {
                                    version: text_document.version,
                                    edits,
                                    rename_to: None,
                                },
                            )
                            .is_some()
                        {
                            return Err("the server repeated a document in documentChanges".into());
                        }
                    }
                    DocumentChange::Resource(operation) => {
                        resource_started = true;
                        let ResourceOperation::Rename {
                            old_uri,
                            new_uri,
                            options,
                            annotation_id,
                        } = &operation
                        else {
                            return Err(format!(
                                "the server's edit needs a `{}` file operation",
                                operation.kind()
                            ));
                        };
                        let old = uri_to_path(old_uri)?;
                        let new = uri_to_path(new_uri)?;
                        if allowed_rename != Some((old.as_path(), new.as_path()))
                            || options.overwrite
                            || options.ignore_if_exists
                            || annotation_id.is_some()
                        {
                            return Err(
                                "the server requested an unsupported `rename` file operation"
                                    .into(),
                            );
                        }
                        if out.contains_key(&new) {
                            return Err("the server edited the rename destination".into());
                        }
                        let document = out
                            .get_mut(&old)
                            .ok_or("the server renamed a file before providing its text edits")?;
                        if document.rename_to.replace(new).is_some() {
                            return Err("the server repeated a file rename".into());
                        }
                    }
                }
            }
        } else if let Some(changes) = self.changes {
            for (uri, edits) in changes {
                if out
                    .insert(
                        uri_to_path(&uri)?,
                        DocumentEdits {
                            version: None,
                            edits,
                            rename_to: None,
                        },
                    )
                    .is_some()
                {
                    return Err("the server repeated a document through URI aliases".into());
                }
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

/// Expand coalesced server edits into the identifier replacements they prove.
/// Literal text, comments and strings must remain byte-identical.
pub(crate) fn identifier_replacements(
    source: &str,
    edits: &[EditSpan],
    language: crate::ast::Language,
    old_name: &str,
    new_name: &str,
) -> Result<Vec<EditSpan>, String> {
    use super::super::refactor_core::collect_identifier_spans;
    let tree = crate::ast::api::parse_tree(source, language).map_err(|error| error.to_string())?;
    if tree.root_node().has_error() {
        return Err("the document does not parse before the rename".into());
    }
    let kinds = language
        .rename_identifier_kinds()
        .ok_or("the language has no identifier projection")?;
    let mut identifiers = Vec::new();
    collect_identifier_spans(
        tree.root_node(),
        source.as_bytes(),
        old_name,
        "",
        kinds,
        "",
        &mut identifiers,
        &mut Vec::new(),
    );
    identifiers.sort_by_key(|span| span.start_byte);
    let mut replacements = Vec::new();
    for edit in edits {
        let mut before = edit.span.start_byte;
        let mut after = 0;
        for span in identifiers.iter().filter(|span| {
            edit.span.start_byte <= span.start_byte && span.end_byte <= edit.span.end_byte
        }) {
            let prefix = source
                .get(before..span.start_byte)
                .ok_or("invalid source edit boundary")?;
            let remaining = edit
                .after
                .get(after..)
                .ok_or("invalid replacement boundary")?;
            if !remaining.starts_with(prefix) {
                return Err("the server changed non-identifier source text".into());
            }
            after += prefix.len();
            let remaining = edit
                .after
                .get(after..)
                .ok_or("invalid replacement boundary")?;
            // Longest matching spelling distinguishes prefix-related names
            // without interpreting Java bindings or tokenizing new source.
            if remaining.starts_with(new_name)
                && (!remaining.starts_with(old_name) || new_name.len() >= old_name.len())
            {
                replacements.push(EditSpan {
                    span: *span,
                    before: old_name.into(),
                    after: new_name.into(),
                });
                after += new_name.len();
            } else if remaining.starts_with(old_name) {
                after += old_name.len();
            } else {
                return Err("the server changed more than the requested identifier".into());
            }
            before = span.end_byte;
        }
        let remaining = edit
            .after
            .get(after..)
            .ok_or("invalid replacement boundary")?;
        let original = source
            .get(before..edit.span.end_byte)
            .ok_or("invalid source edit boundary")?;
        if remaining != original {
            return Err("the server changed non-identifier source text".into());
        }
    }
    Ok(replacements)
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
    let rest = if rest.starts_with('/') {
        rest
    } else if let Some(path) = rest.strip_prefix("localhost/") {
        return uri_to_path(&format!("file:///{path}"));
    } else {
        return Err(format!("unsupported file URI authority in `{uri}`"));
    };
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
