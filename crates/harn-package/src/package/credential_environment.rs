use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use super::{
    ConnectorCredentialEnvironmentManifest, ConnectorSecretDirection,
    ConnectorSetupConfigurationField, ProviderSetupManifest,
};

const MAX_ENTRIES: usize = 16;
const MAX_NAMES_PER_SECRET: usize = 8;
const MAX_NAME_BYTES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialEnvironmentIssue {
    TooManyEntries,
    SecretNotRequired {
        secret: String,
    },
    SecretNotOutbound {
        secret: String,
        direction: ConnectorSecretDirection,
    },
    DuplicateSecret {
        secret: String,
    },
    NoNames {
        secret: String,
    },
    TooManyNames {
        secret: String,
    },
    InvalidName {
        name: String,
    },
    DuplicateName {
        secret: String,
        name: String,
    },
    NameAssignedToSeveralSecrets {
        name: String,
        first_secret: String,
        second_secret: String,
    },
}

impl fmt::Display for CredentialEnvironmentIssue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyEntries => write!(formatter, "accepts at most {MAX_ENTRIES} entries"),
            Self::SecretNotRequired { secret } => {
                write!(
                    formatter,
                    "secret '{secret}' must also appear in required_secrets"
                )
            }
            Self::SecretNotOutbound { secret, direction } => write!(
                formatter,
                "secret '{secret}' must be outbound, but is declared {}",
                direction.as_str()
            ),
            Self::DuplicateSecret { secret } => write!(formatter, "repeats secret '{secret}'"),
            Self::NoNames { secret } => {
                write!(
                    formatter,
                    "secret '{secret}' must declare at least one environment name"
                )
            }
            Self::TooManyNames { secret } => write!(
                formatter,
                "secret '{secret}' accepts at most {MAX_NAMES_PER_SECRET} environment names"
            ),
            Self::InvalidName { name } => write!(
                formatter,
                "name '{name}' must use uppercase letters, digits, and underscores"
            ),
            Self::DuplicateName { secret, name } => {
                write!(
                    formatter,
                    "secret '{secret}' repeats environment name '{name}'"
                )
            }
            Self::NameAssignedToSeveralSecrets {
                name,
                first_secret,
                second_secret,
            } => write!(
                formatter,
                "name '{name}' is assigned to both '{first_secret}' and '{second_secret}'"
            ),
        }
    }
}

pub fn credential_environment_issues(
    setup: &ProviderSetupManifest,
) -> Vec<CredentialEnvironmentIssue> {
    let mut issues = Vec::new();
    if setup.credential_environment.len() > MAX_ENTRIES {
        issues.push(CredentialEnvironmentIssue::TooManyEntries);
    }
    let mut declared_secrets = BTreeSet::new();
    let mut environment_owners = BTreeMap::new();
    for source in &setup.credential_environment {
        match setup.required_secret(&source.secret) {
            None => issues.push(CredentialEnvironmentIssue::SecretNotRequired {
                secret: source.secret.clone(),
            }),
            Some(requirement) if requirement.direction != ConnectorSecretDirection::Outbound => {
                issues.push(CredentialEnvironmentIssue::SecretNotOutbound {
                    secret: source.secret.clone(),
                    direction: requirement.direction,
                });
            }
            Some(_) => {}
        }
        if !declared_secrets.insert(source.secret.as_str()) {
            issues.push(CredentialEnvironmentIssue::DuplicateSecret {
                secret: source.secret.clone(),
            });
        }
        if source.environment_names.is_empty() {
            issues.push(CredentialEnvironmentIssue::NoNames {
                secret: source.secret.clone(),
            });
        }
        if source.environment_names.len() > MAX_NAMES_PER_SECRET {
            issues.push(CredentialEnvironmentIssue::TooManyNames {
                secret: source.secret.clone(),
            });
        }
        let mut declared_names = BTreeSet::new();
        for name in &source.environment_names {
            if !environment_name_is_valid(name) {
                issues.push(CredentialEnvironmentIssue::InvalidName { name: name.clone() });
            }
            if !declared_names.insert(name.as_str()) {
                issues.push(CredentialEnvironmentIssue::DuplicateName {
                    secret: source.secret.clone(),
                    name: name.clone(),
                });
            }
            if let Some(first_secret) =
                environment_owners.insert(name.as_str(), source.secret.as_str())
            {
                if first_secret != source.secret {
                    issues.push(CredentialEnvironmentIssue::NameAssignedToSeveralSecrets {
                        name: name.clone(),
                        first_secret: first_secret.to_string(),
                        second_secret: source.secret.clone(),
                    });
                }
            }
        }
    }
    issues
}

/// Validate the allowlisted environment names for non-secret setup fields.
/// This is intentionally separate from credential sources: a public OAuth
/// client id must never be mistaken for an access token or stored in the
/// credential index.
pub fn configuration_environment_issues(setup: &ProviderSetupManifest) -> Vec<String> {
    let sources = &setup.configuration_environment;
    let mut issues = Vec::new();
    if sources.len() > MAX_ENTRIES {
        issues.push(format!("must include at most {MAX_ENTRIES} entries"));
    }
    let mut fields = BTreeSet::new();
    let mut names = BTreeSet::new();
    for source in sources {
        if !fields.insert(source.field) {
            issues.push(format!("repeats field '{}'", source.field.as_str()));
        }
        if source.environment_names.is_empty() {
            issues.push(format!(
                "field '{}' must declare at least one environment name",
                source.field.as_str()
            ));
        }
        if source.environment_names.len() > MAX_NAMES_PER_SECRET {
            issues.push(format!(
                "field '{}' must declare at most {MAX_NAMES_PER_SECRET} environment names",
                source.field.as_str()
            ));
        }
        let mut field_names = BTreeSet::new();
        for name in &source.environment_names {
            if !environment_name_is_valid(name) {
                issues.push(format!("environment name '{name}' is invalid"));
            }
            if !field_names.insert(name.as_str()) {
                issues.push(format!(
                    "field '{}' repeats environment name '{name}'",
                    source.field.as_str()
                ));
            }
            if !names.insert(name.as_str()) {
                issues.push(format!(
                    "environment name '{name}' is assigned to several configuration fields"
                ));
            }
        }
    }
    issues
}

/// Resolve one non-secret setup value only from names explicitly declared by
/// the connector manifest. The value is returned to the setup adapter and must
/// not be included in status, plan, event, or diagnostic output.
pub fn process_configuration_environment_value(
    setup: &ProviderSetupManifest,
    field: ConnectorSetupConfigurationField,
) -> Option<String> {
    setup
        .configuration_environment
        .iter()
        .filter(|source| source.field == field)
        .flat_map(|source| source.environment_names.iter())
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
}

pub fn available_process_credential_environment_name<'a>(
    sources: &'a [ConnectorCredentialEnvironmentManifest],
    secret: &str,
) -> Option<&'a str> {
    available_credential_environment_name(sources, secret, |name| {
        std::env::var(name)
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
    })
}

pub fn available_credential_environment_name<'a>(
    sources: &'a [ConnectorCredentialEnvironmentManifest],
    secret: &str,
    mut is_present: impl FnMut(&str) -> bool,
) -> Option<&'a str> {
    sources
        .iter()
        .filter(|source| source.secret == secret)
        .flat_map(|source| source.environment_names.iter())
        .find(|name| is_present(name))
        .map(String::as_str)
}

pub fn credential_environment_names(setup: &ProviderSetupManifest) -> Vec<String> {
    let mut names = setup
        .credential_environment
        .iter()
        .flat_map(|source| source.environment_names.iter().cloned())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

pub(crate) fn credential_environment_names_for_secret(
    sources: &[ConnectorCredentialEnvironmentManifest],
    secret: &str,
) -> Vec<String> {
    let mut names = sources
        .iter()
        .filter(|source| source.secret == secret)
        .flat_map(|source| source.environment_names.iter().cloned())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

pub fn configuration_environment_names(setup: &ProviderSetupManifest) -> Vec<String> {
    let mut names = setup
        .configuration_environment
        .iter()
        .flat_map(|source| source.environment_names.iter().cloned())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn environment_name_is_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_uses_only_declared_nonempty_sources() {
        let sources = [ConnectorCredentialEnvironmentManifest {
            secret: "duffel/test-access-token".to_string(),
            environment_names: vec!["DUFFEL_TEST_KEY".to_string(), "DUFFEL_BACKUP".to_string()],
        }];
        let found =
            available_credential_environment_name(&sources, "duffel/test-access-token", |name| {
                name == "DUFFEL_BACKUP"
            });
        assert_eq!(found, Some("DUFFEL_BACKUP"));
        assert_eq!(
            available_credential_environment_name(&sources, "duffel/live-token", |_| true),
            None
        );
    }

    #[test]
    fn fallback_names_are_exact_secret_scoped_and_deduplicated() {
        let sources = [
            ConnectorCredentialEnvironmentManifest {
                secret: "duffel/test-access-token".to_string(),
                environment_names: vec!["DUFFEL_TEST_KEY".to_string(), "DUFFEL_BACKUP".to_string()],
            },
            ConnectorCredentialEnvironmentManifest {
                secret: "duffel/test-access-token".to_string(),
                environment_names: vec!["DUFFEL_TEST_KEY".to_string()],
            },
            ConnectorCredentialEnvironmentManifest {
                secret: "duffel/live-token".to_string(),
                environment_names: vec!["DUFFEL_LIVE_KEY".to_string()],
            },
        ];

        assert_eq!(
            credential_environment_names_for_secret(&sources, "duffel/test-access-token"),
            ["DUFFEL_BACKUP", "DUFFEL_TEST_KEY"]
        );
    }

    #[test]
    fn manifest_shape_decodes_logical_secret_environment_sources() {
        let setup: ProviderSetupManifest = toml::from_str(
            r#"
auth_type = "api-key"
required_secrets = [{ id = "duffel/test-access-token", direction = "outbound" }]
credential_environment = [
  { secret = "duffel/test-access-token", environment_names = ["DUFFEL_TEST_KEY"] },
]
"#,
        )
        .expect("setup manifest");
        assert_eq!(
            setup.credential_environment,
            [ConnectorCredentialEnvironmentManifest {
                secret: "duffel/test-access-token".to_string(),
                environment_names: vec!["DUFFEL_TEST_KEY".to_string()],
            }]
        );
    }

    #[test]
    fn configuration_environment_is_allowlisted_and_value_safe() {
        const NAME: &str = "HARN_TEST_OAUTH_CLIENT_ID_6615";
        let _environment = crate::env_guard::ScopedEnvVar::set(NAME, "fixture-client-id");
        let setup: ProviderSetupManifest = toml::from_str(&format!(
            r#"
auth_type = "oauth2"
configuration_environment = [
  {{ field = "oauth_client_id", environment_names = ["{NAME}"] }},
]
"#,
        ))
        .expect("setup manifest");
        assert!(configuration_environment_issues(&setup).is_empty());
        assert_eq!(
            process_configuration_environment_value(
                &setup,
                ConnectorSetupConfigurationField::OAuthClientId,
            )
            .as_deref(),
            Some("fixture-client-id")
        );
        let encoded = serde_json::to_string(&setup.configuration_environment).unwrap();
        assert!(encoded.contains(NAME));
        assert!(!encoded.contains("fixture-client-id"));
    }

    #[test]
    fn configuration_environment_rejects_ambiguous_or_unsafe_names() {
        let setup: ProviderSetupManifest = toml::from_str(
            r#"
configuration_environment = [
  { field = "oauth_client_id", environment_names = ["bad-name", "SHARED_ID"] },
  { field = "oauth_client_id", environment_names = ["SHARED_ID"] },
]
"#,
        )
        .expect("setup manifest");
        let issues = configuration_environment_issues(&setup).join("\n");
        assert!(issues.contains("repeats field"));
        assert!(issues.contains("'bad-name' is invalid"));
        assert!(issues.contains("assigned to several configuration fields"));
    }
}
