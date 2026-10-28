//! A bounded journal lookup through an existing canonical-store owner.

use super::*;

pub(super) fn lookup(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let session_id = Args::runtime("__session_store_presence", &args)
        .non_empty_string(0, "session_id")?
        .to_string();
    validate_session_id(&session_id)?;
    let options = Args::runtime("__session_store_presence", &args).opt_dict(1, "options")?;
    let tenant_id = option_string(options, "tenant_id")?;
    let state_dir = store_state_dir(options)?;
    let legacy_path = legacy_session_path(&state_dir, &session_id)?;
    let legacy_exists = || {
        legacy_path.try_exists().map_err(|error| {
            VmError::Runtime(format!(
                "session_store: cannot inspect legacy journal: {error}"
            ))
        })
    };
    if let Some(store) = crate::agent_sessions::with_journal_in_state_dir(
        state_dir.as_path(),
        crate::agent_session_journal::JournalState::store,
    ) {
        match store.journal_presence(&session_id).map_err(store_error)? {
            Some(presence) => {
                validate_tenant(&session_id, &presence.tenant_id, tenant_id.as_deref())?;
                return Ok(json_to_vm_value(&json!({
                    "state": "present", "has_events": presence.has_events,
                })));
            }
            None if !legacy_exists()? => return Ok(json_to_vm_value(&json!({"state": "missing"}))),
            None => return Ok(json_to_vm_value(&json!({"state": "unavailable"}))),
        }
    }
    let store_exists = store_path(&state_dir).try_exists().map_err(|error| {
        VmError::Runtime(format!("session_store: cannot inspect store: {error}"))
    })?;
    let state = if !store_exists && !legacy_exists()? {
        "absent"
    } else {
        "unavailable"
    };
    Ok(json_to_vm_value(&json!({"state": state})))
}
