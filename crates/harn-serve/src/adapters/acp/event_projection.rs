//! Small payload predicates the ACP event sink reads while projecting.

pub(super) fn has_progress_entries(entries: &serde_json::Value) -> bool {
    entries
        .as_array()
        .map(|entries| !entries.is_empty())
        .unwrap_or(false)
}
