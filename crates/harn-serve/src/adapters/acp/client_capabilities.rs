use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct ClientCapabilities {
    #[serde(deserialize_with = "or_default")]
    pub fs: FileSystemCapabilities,
    #[serde(deserialize_with = "or_default")]
    pub terminal: bool,
    #[serde(deserialize_with = "or_default")]
    pub auth: AuthCapabilities,
    #[serde(deserialize_with = "or_default")]
    pub elicitation: Option<ElicitationCapabilities>,
    #[serde(deserialize_with = "or_default")]
    pub session: Option<SessionCapabilities>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct FileSystemCapabilities {
    #[serde(deserialize_with = "or_default")]
    pub read_text_file: bool,
    #[serde(deserialize_with = "or_default")]
    pub write_text_file: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub(super) struct AuthCapabilities {
    #[serde(deserialize_with = "or_default")]
    pub terminal: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub(super) struct ElicitationCapabilities {
    #[serde(deserialize_with = "or_default")]
    pub form: Option<ObjectCapability>,
    #[serde(deserialize_with = "or_default")]
    pub url: Option<ObjectCapability>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct SessionCapabilities {
    #[serde(deserialize_with = "or_default")]
    pub notices: Option<ObjectCapability>,
    #[serde(deserialize_with = "or_default")]
    pub compaction: Option<ObjectCapability>,
    #[serde(deserialize_with = "or_default")]
    pub config_options: Option<ConfigOptionsCapabilities>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub(super) struct ConfigOptionsCapabilities {
    #[serde(deserialize_with = "or_default")]
    pub boolean: Option<ObjectCapability>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct ObjectCapability {}

// ACP defaults each malformed optional capability independently, so one future
// capability cannot erase an otherwise valid terminal or filesystem grant.
fn or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

impl ClientCapabilities {
    pub(super) fn from_initialize(params: &serde_json::Value) -> Self {
        params
            .get("clientCapabilities")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default()
    }

    pub(super) fn supports_method(&self, method: &str) -> bool {
        match method {
            "fs/read_text_file" => self.fs.read_text_file,
            "fs/write_text_file" => self.fs.write_text_file,
            _ if method.starts_with("terminal/") => self.terminal,
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_capabilities_default_independently_and_require_real_grants() {
        let capabilities = ClientCapabilities::from_initialize(&serde_json::json!({
            "clientCapabilities": {
                "terminal": true,
                "fs": {"readTextFile": true, "writeTextFile": "true"},
                "auth": null,
                "elicitation": {"form": {}, "url": false},
                "session": {"notices": {}, "compaction": true},
            },
        }));
        assert!(capabilities.supports_method("terminal/create"));
        assert!(capabilities.supports_method("fs/read_text_file"));
        assert!(!capabilities.supports_method("fs/write_text_file"));
        assert!(!capabilities.auth.terminal);
        let elicitation = capabilities.elicitation.unwrap();
        assert!(elicitation.form.is_some());
        assert!(elicitation.url.is_none());
        let session = capabilities.session.unwrap();
        assert!(session.notices.is_some());
        assert!(session.compaction.is_none());
        for invalid in [
            serde_json::json!({}),
            serde_json::json!({"clientCapabilities": null}),
            serde_json::json!({"clientCapabilities": {"terminal": "true"}}),
        ] {
            let capabilities = ClientCapabilities::from_initialize(&invalid);
            assert!(!capabilities.supports_method("terminal/create"));
            assert!(capabilities.supports_method("host/call"));
        }
    }
}
