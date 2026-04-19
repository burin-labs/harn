use async_trait::async_trait;

use super::{
    emit_secret_access_event, RotationHandle, SecretBytes, SecretError, SecretId, SecretMeta,
    SecretProvider, SecretVersion,
};

#[derive(Debug, Clone)]
pub struct EnvSecretProvider {
    namespace: String,
}

impl EnvSecretProvider {
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
        }
    }

    pub fn env_var_name(&self, id: &SecretId) -> String {
        let namespace = normalize_env_component(&id.namespace);
        let name = normalize_env_component(&id.name);
        match id.version {
            SecretVersion::Latest => format!("HARN_SECRET_{namespace}_{name}"),
            SecretVersion::Exact(version) => format!("HARN_SECRET_{namespace}_{name}_V{version}"),
        }
    }
}

#[async_trait]
impl SecretProvider for EnvSecretProvider {
    async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
        let env_name = self.env_var_name(id);
        match std::env::var(&env_name) {
            Ok(value) if !value.is_empty() => {
                emit_secret_access_event("env", id);
                Ok(SecretBytes::from(value))
            }
            _ => Err(SecretError::NotFound {
                provider: "env".to_string(),
                id: id.clone(),
            }),
        }
    }

    async fn put(&self, id: &SecretId, value: SecretBytes) -> Result<(), SecretError> {
        let env_name = self.env_var_name(id);
        let rendered = value.with_exposed(|bytes| {
            std::str::from_utf8(bytes)
                .map(|text| text.to_string())
                .map_err(|error| SecretError::Backend {
                    provider: "env".to_string(),
                    message: format!("env secrets must be valid UTF-8: {error}"),
                })
        })?;
        std::env::set_var(&env_name, rendered);
        Ok(())
    }

    async fn rotate(&self, _id: &SecretId) -> Result<RotationHandle, SecretError> {
        Err(SecretError::Unsupported {
            provider: "env".to_string(),
            operation: "rotate",
        })
    }

    async fn list(&self, prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
        let env_prefix = if prefix.name.is_empty() {
            format!(
                "HARN_SECRET_{}_",
                normalize_env_component(&prefix.namespace)
            )
        } else {
            self.env_var_name(prefix)
        };

        let items = std::env::vars()
            .filter_map(|(name, _)| {
                if !name.starts_with(&env_prefix) {
                    return None;
                }
                let suffix = name
                    .strip_prefix(&format!(
                        "HARN_SECRET_{}_",
                        normalize_env_component(&prefix.namespace)
                    ))
                    .unwrap_or_default()
                    .trim_start_matches('_')
                    .to_ascii_lowercase();
                Some(SecretMeta {
                    id: SecretId::new(prefix.namespace.clone(), suffix),
                    provider: "env".to_string(),
                })
            })
            .collect::<Vec<_>>();
        Ok(items)
    }

    fn namespace(&self) -> &str {
        &self.namespace
    }

    fn supports_versions(&self) -> bool {
        false
    }
}

fn normalize_env_component(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut last_was_underscore = false;
    for ch in value.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_uppercase()
        } else {
            '_'
        };
        if mapped == '_' {
            if !last_was_underscore {
                normalized.push(mapped);
            }
            last_was_underscore = true;
        } else {
            normalized.push(mapped);
            last_was_underscore = false;
        }
    }

    normalized.trim_matches('_').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_provider_uses_expected_variable_name() {
        let provider = EnvSecretProvider::new("harn/test");
        let id = SecretId::new("harn.orchestrator.github", "installation-12345/private-key");
        assert_eq!(
            provider.env_var_name(&id),
            "HARN_SECRET_HARN_ORCHESTRATOR_GITHUB_INSTALLATION_12345_PRIVATE_KEY"
        );
    }
}
