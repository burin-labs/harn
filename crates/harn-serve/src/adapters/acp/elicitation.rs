//! MCP input requests retain their runtime owner across the ACP projection.
use super::*;

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
enum ContentValue {
    Text(String),
    Number(serde_json::Number),
    Boolean(bool),
    Selection(Vec<String>),
}

#[derive(Default)]
pub(super) struct UrlElicitations {
    outstanding: std::sync::Mutex<HashMap<(String, String, String), String>>,
}

impl UrlElicitations {
    pub(super) fn complete(&self, session: &str, server: &str, source: &str) -> Option<String> {
        self.outstanding
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(session.into(), server.into(), source.into()))
    }

    pub(super) fn close_session(&self, session: &str) {
        self.outstanding
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(owner, _, _), _| owner != session);
    }
}

struct UrlRegistration<'a> {
    registry: &'a UrlElicitations,
    key: (String, String, String),
    id: String,
    accepted: bool,
}

impl Drop for UrlRegistration<'_> {
    fn drop(&mut self) {
        if !self.accepted {
            let mut outstanding = self
                .registry
                .outstanding
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if outstanding.get(&self.key) == Some(&self.id) {
                outstanding.remove(&self.key);
            }
        }
    }
}

impl AcpBridge {
    pub(super) async fn elicit(
        &self,
        mut params: serde_json::Value,
    ) -> Result<serde_json::Value, harn_vm::VmError> {
        let mode = match params.get("mode") {
            None => "form",
            Some(value) => value
                .as_str()
                .ok_or_else(|| harn_vm::VmError::Runtime("Invalid MCP elicitation mode".into()))?,
        }
        .to_owned();
        if !self.client_capabilities.supports_elicitation(&mode) {
            return Err(harn_vm::VmError::Runtime(format!(
                "ACP client did not advertise elicitation mode {mode}"
            )));
        }
        let invalid = || harn_vm::VmError::Runtime("Invalid MCP elicitation request".into());
        let object = params.as_object_mut().ok_or_else(invalid)?;
        if object
            .get("message")
            .and_then(serde_json::Value::as_str)
            .is_none()
        {
            return Err(invalid());
        }
        object.insert("mode".into(), serde_json::json!(mode));
        object.insert("sessionId".into(), serde_json::json!(self.session_id));
        let mut url_registration = None;
        if mode == "url" {
            let server = object
                .remove("server")
                .and_then(|v| v.as_str().map(str::to_owned))
                .ok_or_else(invalid)?;
            let source = object
                .get("elicitationId")
                .map(|value| {
                    value
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .ok_or_else(invalid)
                })
                .transpose()?;
            let url = object
                .get("url")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(invalid)?;
            url::Url::parse(url).map_err(|_| invalid())?;
            let id = format!("harn-elicitation-{}", self.outbound.next_id());
            if let Some(source) = source {
                let key = (self.session_id.clone(), server, source);
                let mut outstanding = self
                    .elicitations
                    .outstanding
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if outstanding.contains_key(&key) {
                    return Err(harn_vm::VmError::Runtime(
                        "MCP elicitation ID is already outstanding".into(),
                    ));
                }
                outstanding.insert(key.clone(), id.clone());
                url_registration = Some(UrlRegistration {
                    registry: &self.elicitations,
                    key,
                    id: id.clone(),
                    accepted: false,
                });
            }
            object.insert("elicitationId".into(), serde_json::json!(id));
        } else {
            let schema = object.get("requestedSchema").ok_or_else(invalid)?;
            harn_vm::schema::elicitation_validate_schema(&harn_vm::json_to_vm_value(schema))?;
        }
        object.remove("server");
        let mut response = self.call_client("elicitation/create", params).await?;
        let action = response
            .get("action")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                harn_vm::VmError::Runtime("Invalid ACP elicitation response action".into())
            })?
            .to_owned();
        if action == "accept" {
            if let Some(content) = response.get("content").filter(|content| !content.is_null()) {
                let content: BTreeMap<String, ContentValue> =
                    serde_json::from_value(content.clone()).map_err(|_| {
                        harn_vm::VmError::Runtime("Invalid ACP elicitation response content".into())
                    })?;
                response["content"] = serde_json::to_value(content).map_err(|_| {
                    harn_vm::VmError::Runtime("Invalid ACP elicitation response content".into())
                })?;
            }
        }
        if let Some(registration) = &mut url_registration {
            registration.accepted = action == "accept";
        }
        Ok(response)
    }
}
