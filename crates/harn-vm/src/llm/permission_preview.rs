//! Pre-approval filesystem evidence for ACP permission requests.
//!
//! The host must not have to race the agent by reading files after receiving
//! `session/request_permission`. This module captures bounded UTF-8 pre-images
//! immediately before the request is sent and reconstructs the complete
//! post-image for mutation shapes whose semantics are unambiguous.

use std::fs::File;
use std::io::{ErrorKind, Read};

use serde_json::{json, Map, Value as JsonValue};
use sha2::{Digest, Sha256};

use crate::tool_annotations::ToolKind;
use crate::workspace_path::WorkspacePathInfo;

const MAX_PREVIEW_BYTES: u64 = 1024 * 1024;
const MAX_PREVIEW_FILES: usize = 16;

pub(super) fn capture(
    annotations: Option<&crate::tool_annotations::ToolAnnotations>,
    tool_name: &str,
    tool_args: &JsonValue,
) -> Vec<JsonValue> {
    let Some(annotations) = annotations else {
        return Vec::new();
    };
    if !matches!(
        annotations.kind,
        ToolKind::Edit | ToolKind::Delete | ToolKind::Move
    ) {
        return Vec::new();
    }
    let paths = crate::orchestration::tool_declared_path_entries(annotations, tool_args);
    capture_for_paths(
        annotations.kind,
        tool_name,
        tool_args,
        &paths,
        &crate::stdlib::process::execution_root_path(),
    )
}

fn capture_for_paths(
    kind: ToolKind,
    tool_name: &str,
    tool_args: &JsonValue,
    paths: &[WorkspacePathInfo],
    execution_root: &std::path::Path,
) -> Vec<JsonValue> {
    let allow_pathless_ops = paths.len() == 1;
    paths
        .iter()
        .take(MAX_PREVIEW_FILES)
        .map(|path| {
            capture_path(
                kind,
                tool_name,
                tool_args,
                path,
                execution_root,
                allow_pathless_ops,
            )
        })
        .collect()
}

fn capture_path(
    kind: ToolKind,
    tool_name: &str,
    tool_args: &JsonValue,
    path: &WorkspacePathInfo,
    execution_root: &std::path::Path,
    allow_pathless_ops: bool,
) -> JsonValue {
    let display_path = path.display_path();
    let wire_path = path
        .host_path
        .as_deref()
        .unwrap_or(display_path)
        .to_string();
    let Some(host_path) = path.resolved_host_path() else {
        return unavailable_evidence(&wire_path, "unresolved_path", None);
    };
    if path.normalized_workspace_path().is_none() {
        return unavailable_evidence(&wire_path, "outside_execution_root", None);
    }
    if host_path.exists() {
        let Some(canonical_workspace_path) =
            crate::workspace_path::canonicalize_existing_workspace_path(&host_path, execution_root)
        else {
            return unavailable_evidence(&wire_path, "outside_execution_root", None);
        };
        if Some(canonical_workspace_path.as_str()) != path.normalized_workspace_path() {
            return unavailable_evidence(&wire_path, "symlink", None);
        }
    }
    let preimage = read_preimage(&host_path);
    match preimage {
        Preimage::Text(old_text) => {
            let digest = sha256(old_text.as_bytes());
            let byte_count = old_text.len() as u64;
            if let Some(new_text) = proposed_text(
                kind,
                tool_name,
                tool_args,
                display_path,
                Some(&old_text),
                allow_pathless_ops,
            ) {
                diff_evidence(
                    &wire_path,
                    Some(old_text),
                    new_text,
                    Some(digest),
                    byte_count,
                )
            } else {
                json!({
                    "kind": "file_preimage",
                    "path": wire_path,
                    "oldText": old_text,
                    "preimageSha256": digest,
                    "byteCount": byte_count,
                    "source": "pre_approval"
                })
            }
        }
        Preimage::Absent => {
            if let Some(new_text) = proposed_text(
                kind,
                tool_name,
                tool_args,
                display_path,
                None,
                allow_pathless_ops,
            ) {
                diff_evidence(&wire_path, None, new_text, None, 0)
            } else {
                json!({
                    "kind": "file_preimage",
                    "path": wire_path,
                    "oldText": null,
                    "byteCount": 0,
                    "source": "pre_approval"
                })
            }
        }
        Preimage::Unavailable { reason, byte_count } => {
            unavailable_evidence(&wire_path, reason, byte_count)
        }
    }
}

fn diff_evidence(
    path: &str,
    old_text: Option<String>,
    new_text: String,
    preimage_sha256: Option<String>,
    byte_count: u64,
) -> JsonValue {
    json!({
        "kind": "file_mutation_diff",
        "path": path,
        "oldText": old_text,
        "newText": new_text,
        "preimageSha256": preimage_sha256,
        "byteCount": byte_count,
        "source": "pre_approval"
    })
}

fn unavailable_evidence(path: &str, reason: &str, byte_count: Option<u64>) -> JsonValue {
    json!({
        "kind": "file_preimage",
        "path": path,
        "available": false,
        "reason": reason,
        "byteCount": byte_count,
        "source": "pre_approval"
    })
}

enum Preimage {
    Text(String),
    Absent,
    Unavailable {
        reason: &'static str,
        byte_count: Option<u64>,
    },
}

fn read_preimage(path: &std::path::Path) -> Preimage {
    if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Preimage::Unavailable {
            reason: "symlink",
            byte_count: None,
        };
    }
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Preimage::Absent,
        Err(_) => {
            return Preimage::Unavailable {
                reason: "read_failed",
                byte_count: None,
            }
        }
    };
    let byte_count = file.metadata().ok().map(|metadata| metadata.len());
    if byte_count.is_some_and(|len| len > MAX_PREVIEW_BYTES) {
        return Preimage::Unavailable {
            reason: "too_large",
            byte_count,
        };
    }
    let mut bytes = Vec::new();
    if file
        .take(MAX_PREVIEW_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Preimage::Unavailable {
            reason: "read_failed",
            byte_count,
        };
    }
    if bytes.len() as u64 > MAX_PREVIEW_BYTES {
        return Preimage::Unavailable {
            reason: "too_large",
            byte_count: Some(bytes.len() as u64),
        };
    }
    match String::from_utf8(bytes) {
        Ok(text) => Preimage::Text(text),
        Err(error) => Preimage::Unavailable {
            reason: "not_utf8",
            byte_count: Some(error.as_bytes().len() as u64),
        },
    }
}

fn proposed_text(
    kind: ToolKind,
    tool_name: &str,
    tool_args: &JsonValue,
    path: &str,
    old_text: Option<&str>,
    allow_pathless_ops: bool,
) -> Option<String> {
    if kind == ToolKind::Delete {
        return Some(String::new());
    }
    let object = tool_args.as_object()?;
    if let Some(ops) = object.get("ops").and_then(JsonValue::as_array) {
        let mut current = old_text.unwrap_or_default().to_string();
        let mut changed = false;
        for op in ops {
            let Some(op) = op.as_object() else {
                continue;
            };
            match string_field(op, &["path", "file"]) {
                Some(op_path) if op_path != path => continue,
                Some(_) => {}
                None if allow_pathless_ops => {}
                None => return None,
            }
            current = proposed_text_for_object(tool_name, op, Some(&current))?;
            changed = true;
        }
        return changed.then_some(current);
    }
    proposed_text_for_object(tool_name, object, old_text)
}

fn proposed_text_for_object(
    tool_name: &str,
    object: &Map<String, JsonValue>,
    old_text: Option<&str>,
) -> Option<String> {
    let action = string_field(object, &["action", "op"])
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    match action.as_deref() {
        Some("create") => {
            return string_field(
                object,
                &["content", "text", "body", "new_text", "new_string"],
            )
            .map(str::to_string);
        }
        Some("append") => {
            let content = string_field(object, &["content", "text", "body"])?;
            return Some(format!("{}{content}", old_text.unwrap_or_default()));
        }
        Some("exact_patch") => return apply_exact_patch(old_text?, object),
        Some("replace_range" | "insert_after" | "insert_before" | "delete_range") => {
            return apply_line_edit(old_text?, action.as_deref()?, object);
        }
        Some(_) => return None,
        None => {}
    }

    if has_old_new_pair(object) {
        return apply_exact_patch(old_text?, object);
    }
    if tool_name.eq_ignore_ascii_case("write")
        || tool_name.to_ascii_lowercase().starts_with("write_")
        || bool_field(object, "overwrite")
    {
        return string_field(
            object,
            &["content", "text", "body", "new_text", "new_string"],
        )
        .map(str::to_string);
    }
    None
}

fn apply_exact_patch(old_text: &str, object: &Map<String, JsonValue>) -> Option<String> {
    let old = string_field(
        object,
        &[
            "old_string",
            "old_text",
            "old",
            "old_content",
            "current_content",
            "before",
        ],
    )?;
    let new = string_field(
        object,
        &[
            "new_string",
            "new_text",
            "new",
            "new_content",
            "content",
            "after",
        ],
    )?;
    if old.is_empty() {
        return None;
    }
    if bool_field(object, "replace_all") {
        return old_text.contains(old).then(|| old_text.replace(old, new));
    }
    let mut matches = old_text.match_indices(old);
    let (start, _) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let mut output = String::with_capacity(old_text.len() - old.len() + new.len());
    output.push_str(&old_text[..start]);
    output.push_str(new);
    output.push_str(&old_text[start + old.len()..]);
    Some(output)
}

fn apply_line_edit(
    old_text: &str,
    action: &str,
    object: &Map<String, JsonValue>,
) -> Option<String> {
    let range_start = int_field(object, &["range_start", "start_line", "line", "start"])?;
    if range_start == 0 {
        return None;
    }
    let range_end = int_field(object, &["range_end", "end_line", "end"]).unwrap_or(range_start);
    let content =
        string_field(object, &["content", "replacement", "body", "new_text"]).unwrap_or("");
    let final_newline = old_text.ends_with('\n');
    let body = old_text.strip_suffix('\n').unwrap_or(old_text);
    let mut lines = if body.is_empty() {
        Vec::new()
    } else {
        body.split('\n').map(str::to_string).collect::<Vec<_>>()
    };
    let content_body = content.strip_suffix('\n').unwrap_or(content);
    let content_lines = if content_body.is_empty() {
        Vec::new()
    } else {
        content_body
            .split('\n')
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let start = range_start.saturating_sub(1).min(lines.len());
    let end = range_end.max(range_start).min(lines.len()).max(start);
    match action {
        "insert_after" => {
            let insert_at = range_start.min(lines.len());
            lines.splice(insert_at..insert_at, content_lines);
        }
        "insert_before" => {
            lines.splice(start..start, content_lines);
        }
        "replace_range" => {
            lines.splice(start..end, content_lines);
        }
        "delete_range" => {
            lines.drain(start..end);
        }
        _ => return None,
    }
    let mut output = lines.join("\n");
    if final_newline {
        output.push('\n');
    }
    Some(output)
}

fn has_old_new_pair(object: &Map<String, JsonValue>) -> bool {
    string_field(
        object,
        &[
            "old_string",
            "old_text",
            "old",
            "old_content",
            "current_content",
            "before",
        ],
    )
    .is_some()
        && string_field(
            object,
            &[
                "new_string",
                "new_text",
                "new",
                "new_content",
                "content",
                "after",
            ],
        )
        .is_some()
}

fn string_field<'a>(object: &'a Map<String, JsonValue>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(JsonValue::as_str))
}

fn int_field(object: &Map<String, JsonValue>, keys: &[&str]) -> Option<usize> {
    keys.iter().find_map(|key| {
        let value = object.get(*key)?;
        value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .or_else(|| value.as_i64().and_then(|value| usize::try_from(value).ok()))
    })
}

fn bool_field(object: &Map<String, JsonValue>, key: &str) -> bool {
    object
        .get(key)
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::RunExecutionRecord;
    use crate::tool_annotations::{ToolAnnotations, ToolArgSchema};
    use crate::workspace_path::classify_workspace_path;

    #[test]
    fn exact_patch_captures_complete_pre_and_post_images() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("src.rs");
        std::fs::write(&path, "fn old() {}\nfn keep() {}\n").expect("fixture");
        let entry = classify_workspace_path("src.rs", Some(directory.path()));

        let evidence = capture_for_paths(
            ToolKind::Edit,
            "edit",
            &json!({
                "action": "exact_patch",
                "path": "src.rs",
                "old_string": "fn old() {}",
                "new_string": "fn new() {}"
            }),
            &[entry],
            directory.path(),
        );

        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0]["kind"], "file_mutation_diff");
        assert_eq!(evidence[0]["oldText"], "fn old() {}\nfn keep() {}\n");
        assert_eq!(evidence[0]["newText"], "fn new() {}\nfn keep() {}\n");
        assert_eq!(
            evidence[0]["preimageSha256"],
            sha256(b"fn old() {}\nfn keep() {}\n")
        );
    }

    #[test]
    fn line_range_preview_uses_current_file_preimage() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::write(directory.path().join("src.rs"), "one\ntwo\nthree\n").expect("fixture");
        let entry = classify_workspace_path("src.rs", Some(directory.path()));

        let evidence = capture_for_paths(
            ToolKind::Edit,
            "edit",
            &json!({
                "action": "replace_range",
                "path": "src.rs",
                "range_start": 2,
                "range_end": 2,
                "content": "TWO"
            }),
            &[entry],
            directory.path(),
        );

        assert_eq!(evidence[0]["oldText"], "one\ntwo\nthree\n");
        assert_eq!(evidence[0]["newText"], "one\nTWO\nthree\n");
    }

    #[test]
    fn create_preview_uses_null_preimage() {
        let directory = tempfile::tempdir().expect("tempdir");
        let entry = classify_workspace_path("new.rs", Some(directory.path()));

        let evidence = capture_for_paths(
            ToolKind::Edit,
            "edit",
            &json!({"action": "create", "path": "new.rs", "content": "new\n"}),
            &[entry],
            directory.path(),
        );

        assert_eq!(evidence[0]["kind"], "file_mutation_diff");
        assert_eq!(evidence[0]["oldText"], JsonValue::Null);
        assert_eq!(evidence[0]["newText"], "new\n");
    }

    #[test]
    fn unsupported_structural_edit_still_carries_preimage_evidence() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::write(directory.path().join("src.rs"), "fn old() {}\n").expect("fixture");
        let entry = classify_workspace_path("src.rs", Some(directory.path()));

        let evidence = capture_for_paths(
            ToolKind::Edit,
            "edit",
            &json!({
                "action": "replace_body",
                "path": "src.rs",
                "function_name": "old",
                "new_body": "42"
            }),
            &[entry],
            directory.path(),
        );

        assert_eq!(evidence[0]["kind"], "file_preimage");
        assert_eq!(evidence[0]["oldText"], "fn old() {}\n");
        assert!(evidence[0].get("newText").is_none());
    }

    #[test]
    fn pathless_batch_ops_do_not_fabricate_a_diff_for_multiple_files() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::write(directory.path().join("one.txt"), "one\n").expect("fixture");
        std::fs::write(directory.path().join("two.txt"), "two\n").expect("fixture");
        let paths = [
            classify_workspace_path("one.txt", Some(directory.path())),
            classify_workspace_path("two.txt", Some(directory.path())),
        ];

        let evidence = capture_for_paths(
            ToolKind::Edit,
            "edit",
            &json!({
                "ops": [{
                    "action": "exact_patch",
                    "old_string": "one",
                    "new_string": "changed"
                }]
            }),
            &paths,
            directory.path(),
        );

        assert_eq!(evidence.len(), 2);
        assert!(evidence.iter().all(|item| item["kind"] == "file_preimage"));
        assert!(evidence.iter().all(|item| item.get("newText").is_none()));
    }

    #[test]
    fn oversized_preimage_is_explicitly_unavailable() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            directory.path().join("large.txt"),
            vec![b'x'; MAX_PREVIEW_BYTES as usize + 1],
        )
        .expect("fixture");
        let entry = classify_workspace_path("large.txt", Some(directory.path()));

        let evidence = capture_for_paths(
            ToolKind::Edit,
            "write_file",
            &json!({"path": "large.txt", "content": "replacement"}),
            &[entry],
            directory.path(),
        );

        assert_eq!(evidence[0]["available"], false);
        assert_eq!(evidence[0]["reason"], "too_large");
    }

    #[test]
    fn absolute_path_outside_execution_root_is_not_read() {
        let directory = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::NamedTempFile::new().expect("outside fixture");
        std::fs::write(outside.path(), "secret\n").expect("fixture");
        crate::stdlib::process::set_thread_execution_context(Some(RunExecutionRecord {
            cwd: Some(directory.path().to_string_lossy().into_owned()),
            ..Default::default()
        }));
        let annotations = ToolAnnotations {
            kind: ToolKind::Edit,
            arg_schema: ToolArgSchema {
                path_params: vec!["path".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };

        let evidence = capture(
            Some(&annotations),
            "write_file",
            &json!({"path": outside.path(), "content": "replacement"}),
        );

        crate::stdlib::process::set_thread_execution_context(None);
        assert_eq!(evidence[0]["available"], false);
        assert_eq!(evidence[0]["reason"], "outside_execution_root");
        assert!(evidence[0].get("oldText").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_preimage_is_not_followed() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::write(directory.path().join("target.txt"), "secret\n").expect("fixture");
        std::os::unix::fs::symlink("target.txt", directory.path().join("link.txt"))
            .expect("symlink");
        let entry = classify_workspace_path("link.txt", Some(directory.path()));

        let evidence = capture_for_paths(
            ToolKind::Edit,
            "write_file",
            &json!({"path": "link.txt", "content": "replacement"}),
            &[entry],
            directory.path(),
        );

        assert_eq!(evidence[0]["available"], false);
        assert_eq!(evidence[0]["reason"], "symlink");
        assert!(evidence[0].get("oldText").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn intermediate_symlink_is_not_followed() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(directory.path().join("target")).expect("target directory");
        std::fs::write(directory.path().join("target/secret.txt"), "secret\n").expect("fixture");
        std::os::unix::fs::symlink("target", directory.path().join("link")).expect("symlink");
        let entry = classify_workspace_path("link/secret.txt", Some(directory.path()));

        let evidence = capture_for_paths(
            ToolKind::Edit,
            "write_file",
            &json!({"path": "link/secret.txt", "content": "replacement"}),
            &[entry],
            directory.path(),
        );

        assert_eq!(evidence[0]["available"], false);
        assert_eq!(evidence[0]["reason"], "symlink");
        assert!(evidence[0].get("oldText").is_none());
    }

    #[test]
    fn capture_uses_the_declared_edit_path_and_execution_root() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::write(directory.path().join("src.rs"), "old\n").expect("fixture");
        crate::stdlib::process::set_thread_execution_context(Some(RunExecutionRecord {
            cwd: Some(directory.path().to_string_lossy().into_owned()),
            ..Default::default()
        }));
        let annotations = ToolAnnotations {
            kind: ToolKind::Edit,
            arg_schema: ToolArgSchema {
                path_params: vec!["path".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };

        let evidence = capture(
            Some(&annotations),
            "edit",
            &json!({
                "action": "exact_patch",
                "path": "src.rs",
                "old_string": "old",
                "new_string": "new"
            }),
        );

        crate::stdlib::process::set_thread_execution_context(None);
        assert_eq!(evidence.len(), 1);
        assert_eq!(
            std::path::Path::new(evidence[0]["path"].as_str().expect("captured path")),
            directory.path().join("src.rs")
        );
        assert_eq!(evidence[0]["oldText"], "old\n");
        assert_eq!(evidence[0]["newText"], "new\n");
    }
}
