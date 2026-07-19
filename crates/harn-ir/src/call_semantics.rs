pub(super) const HARNESS_SUB_HANDLES: &[&str] = &[
    "stdio", "term", "clock", "fs", "env", "random", "net", "process", "crypto", "system", "llm",
];

pub(super) fn is_workspace_mutation(name: &str) -> bool {
    matches!(
        name,
        "write_file"
            | "write_file_bytes"
            | "replace_file"
            | "replace_file_result"
            | "replace_file_bytes"
            | "replace_file_bytes_result"
            | "append_file"
            | "append_file_locked"
            | "delete_file"
            | "mkdir"
            | "mkdtemp"
            | "apply_edit"
            | "move_file"
    )
}
