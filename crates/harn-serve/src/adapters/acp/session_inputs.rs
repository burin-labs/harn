use super::*;
use harn_vm::session_inputs::{self, InputSpec};
use serde::Deserialize;

/// Private control-plane input. Deliberately lacks Debug and Serialize.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    session_id: String,
    command: Command,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    List,
    Sync {
        snapshot: harn_vm::session_inputs::PrivateInputSnapshot,
    },
    Put {
        spec: InputSpec,
        value: String,
        #[serde(default)]
        replace: bool,
    },
    Remove {
        name: String,
    },
}

impl AcpServer {
    pub(super) fn handle_session_inputs(
        &mut self,
        id: &serde_json::Value,
        params: serde_json::Value,
    ) {
        let Some(session_id) = params.get("sessionId").and_then(serde_json::Value::as_str) else {
            self.send_error(id, -32602, "Session inputs require a sessionId");
            return;
        };
        let Some(session) = self.sessions.get(session_id) else {
            self.send_error(id, -32004, "The session is unavailable");
            return;
        };
        match execute(params, &session.concurrent_control.inputs) {
            Ok(value) => self.send_response(id, value),
            Err(message) => self.send_error(id, -32602, &message),
        }
    }
}

pub(super) fn execute(
    params: serde_json::Value,
    store: &session_inputs::Store,
) -> Result<serde_json::Value, String> {
    let request: Request = serde_json::from_value(params).map_err(|_| {
        "Session inputs require a sessionId and a valid list, put, or remove command".to_owned()
    })?;
    if request.session_id.is_empty() {
        return Err("Session inputs require a sessionId".into());
    }
    let mut store = store.lock();
    match request.command {
        Command::Sync { snapshot } => store.apply_private_snapshot(snapshot, session_inputs::now_ms())
            .map(|applied| serde_json::json!({"applied":applied, "inputs":store.list(session_inputs::now_ms())})).map_err(|error| error.to_string()),
        Command::List => Ok(serde_json::json!({"inputs": store.list(session_inputs::now_ms())})),
        Command::Put {
            spec,
            value,
            replace,
        } => store
            .put(spec, value, replace, session_inputs::now_ms())
            .map(|entry| serde_json::json!({"input": entry}))
            .map_err(|error| error.to_string()),
        Command::Remove { name } => Ok(serde_json::json!({"removed": store.remove(&name)})),
    }
}
