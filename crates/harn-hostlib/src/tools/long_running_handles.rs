//! Session-scoped command identity snapshots from the owning handle store.

use harn_vm::{VmDictExt, VmValue};

use super::HANDLE_STORE;

pub(crate) fn list_session_handles(session_id: &str, completed_handle_ids: &[String]) -> VmValue {
    let store = HANDLE_STORE
        .lock()
        .expect("long-running handle store poisoned");
    let handles: Vec<VmValue> = store
        .entries
        .iter()
        .filter(|(_id, entry)| entry.session_id == session_id)
        .map(|(id, entry)| {
            let mut row = harn_vm::value::DictMap::new();
            row.put_str("handle_id", id.clone());
            row.put_str("session_id", entry.session_id.clone());
            row.put_str("lease", entry.lease.as_str());
            row.put_str("command_or_op_descriptor", entry.command_display.clone());
            row.put_str("started_at", entry.started_at.clone());
            VmValue::dict(row)
        })
        .collect();
    let mut response = harn_vm::value::DictMap::new();
    response.put("handles", VmValue::list(handles));
    if !completed_handle_ids.is_empty() {
        let completed = completed_handle_ids
            .iter()
            .filter_map(|id| {
                let entry = store.terminal_results.get(id)?;
                if entry.session_id != session_id {
                    return None;
                }
                let result = entry.value.as_dict()?;
                let mut row = harn_vm::value::DictMap::new();
                row.put_str("handle_id", id.clone());
                row.put_str("session_id", session_id);
                row.put_str("lease", entry.lease.as_str());
                for field in ["status", "command_or_op_descriptor"] {
                    if let Some(value) = result.get(field) {
                        row.put(field, value.clone());
                    }
                }
                Some(VmValue::dict(row))
            })
            .collect();
        response.put("completed_handles", VmValue::list(completed));
    }
    VmValue::dict(response)
}
