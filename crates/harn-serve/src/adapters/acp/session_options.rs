use super::*;

impl AcpServer {
    pub(super) fn validate_protocol_session_options(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) -> bool {
        if !params.is_object() {
            self.send_error(id, -32602, "session parameters must be an object");
            return false;
        }
        if let Some(servers) = params.get("mcpServers") {
            match servers.as_array() {
                Some(servers) if servers.is_empty() => {}
                _ => {
                    self.send_error(
                        id,
                        -32602,
                        "this agent does not support session mcpServers intake",
                    );
                    return false;
                }
            }
        }
        if params
            .get("systemPrompt")
            .is_some_and(|value| !value.is_null())
        {
            self.send_error(
                id,
                -32602,
                "this agent does not support a client systemPrompt",
            );
            return false;
        }
        true
    }
}
