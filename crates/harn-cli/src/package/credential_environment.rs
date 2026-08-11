use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use super::{ConnectorCredentialEnvironmentManifest, ProviderSetupManifest};

const MAX_ENTRIES: usize = 16;
const MAX_NAMES_PER_SECRET: usize = 8;
const MAX_NAME_BYTES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialEnvironmentIssue {
    TooManyEntries,
    SecretNotRequired {
        secret: String,
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
    let required = setup.required_secrets.iter().collect::<BTreeSet<_>>();
    let mut declared_secrets = BTreeSet::new();
    let mut environment_owners = BTreeMap::new();
    for source in &setup.credential_environment {
        if !required.contains(&source.secret) {
            issues.push(CredentialEnvironmentIssue::SecretNotRequired {
                secret: source.secret.clone(),
            });
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
    fn manifest_shape_decodes_logical_secret_environment_sources() {
        let setup: ProviderSetupManifest = toml::from_str(
            r#"
auth_type = "api-key"
required_secrets = ["duffel/test-access-token"]
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
}
