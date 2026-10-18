//! Named session inputs. Only descriptions cross the model/context seam.
//! Hosts own secure entry, durable custody, clocks, and concrete effects.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use url::Url;
use zeroize::Zeroizing;

mod transfer;
pub use transfer::PrivateInputSnapshot;

const MAX_INPUTS: usize = 256;
const MAX_VALUE_BYTES: usize = 65_536;
const MAX_SESSION_BYTES: usize = 1_048_576;
const MAX_REDACTION_MATCHES: usize = 10_000;

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    #[default]
    Secret,
    Variable,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputLifetime {
    #[default]
    Session,
    Until {
        unix_ms: u64,
    },
    Saved,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputUsage {
    #[default]
    SessionTools,
    Websites {
        origins: Vec<String>,
    },
}

/// Value-free policy, safe for settings, receipts, and model-facing metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputSpec {
    pub name: String,
    #[serde(default)]
    pub kind: InputKind,
    #[serde(default)]
    pub lifetime: InputLifetime,
    #[serde(default)]
    pub usage: InputUsage,
}

impl InputSpec {
    /// Inline schema for host configuration and API projections.
    pub fn json_schema() -> serde_json::Value {
        let generator = schemars::generate::SchemaSettings::openapi3()
            .for_deserialize()
            .with(|settings| settings.inline_subschemas = true)
            .into_generator();
        let mut schema = serde_json::to_value(generator.into_root_schema_for::<Self>())
            .expect("Session input policy schema is serializable");
        schema.as_object_mut().unwrap().remove("$schema");
        schema
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputDescription {
    #[serde(flatten)]
    pub spec: InputSpec,
    /// Present only for ordinary variables. Secret values never appear here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputError {
    InvalidName,
    InvalidValue,
    InvalidExpiry,
    InvalidWebsites,
    LimitReached,
    AlreadyExists,
    Unavailable,
    UsageDenied,
    Closed,
    InvalidSnapshot,
    StaleSnapshot,
}

impl fmt::Display for InputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidName => "Use a name starting with a letter or underscore, followed by letters, digits, or underscores (up to 128 characters).",
            Self::InvalidValue => "Secrets need a non-empty value. Values must be UTF-8, contain no NUL, and fit within 64 KiB.",
            Self::InvalidExpiry => "Choose an expiry time in the future.",
            Self::InvalidWebsites => "Choose 1 to 32 HTTPS websites, without paths, credentials, queries, or fragments.",
            Self::LimitReached => "This session has reached its input storage limit.",
            Self::AlreadyExists => "That name is already in use. Replace the existing entry or choose another name.",
            Self::Unavailable => "That input is missing or has expired. Add it again to continue.",
            Self::UsageDenied => "This input is not allowed for that use.",
            Self::Closed => "This session is closed.",
            Self::InvalidSnapshot => "The private input transfer is invalid or exceeds the session limit.",
            Self::StaleSnapshot => "A newer input change has already been applied.",
        })
    }
}

impl std::error::Error for InputError {}

struct Entry {
    spec: InputSpec,
    value: Zeroizing<String>,
    source: Option<String>,
}

/// No serialization, cloning, or raw-value accessor. A host resolves a value
/// only for one declared effect and filters the result before returning it.
#[derive(Default)]
pub struct SessionInputs {
    entries: BTreeMap<String, Entry>,
    retired: Vec<Zeroizing<String>>,
    retained_bytes: usize,
    closed: bool,
    revision: u64,
    source_revisions: BTreeMap<String, u64>,
}

impl fmt::Debug for SessionInputs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionInputs")
            .field("names", &self.entries.keys().collect::<Vec<_>>())
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

/// An effect's actual destination, checked immediately before resolution.
#[derive(Clone, Copy)]
pub enum InputUse<'a> {
    /// Uses the host's normal proxy and TLS trust settings.
    Http(&'a str),
    /// The caller supplies proxy or TLS options.
    HttpWithCustomTransport(&'a str),
    Command,
}

impl InputSpec {
    pub fn validate(mut self, now_ms: u64) -> Result<Self, InputError> {
        let mut chars = self.name.bytes();
        if self.name.len() > 128
            || !chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
            || !chars.all(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            return Err(InputError::InvalidName);
        }
        if matches!(self.lifetime, InputLifetime::Until { unix_ms } if unix_ms <= now_ms) {
            return Err(InputError::InvalidExpiry);
        }
        if let InputUsage::Websites { origins } = &mut self.usage {
            if origins.is_empty() || origins.len() > 32 || self.kind != InputKind::Secret {
                return Err(InputError::InvalidWebsites);
            }
            let mut normalized = Vec::new();
            for origin in origins.iter() {
                let url = Url::parse(origin).map_err(|_| InputError::InvalidWebsites)?;
                if url.scheme() != "https"
                    || url.host_str().is_none()
                    || url.host_str().is_some_and(|host| host.contains('*'))
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.query().is_some()
                    || url.fragment().is_some()
                    || url.path() != "/"
                {
                    return Err(InputError::InvalidWebsites);
                }
                normalized.push(url.origin().ascii_serialization());
            }
            normalized.sort();
            normalized.dedup();
            *origins = normalized;
        }
        Ok(self)
    }
}

impl SessionInputs {
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn has_secret_filters(&self) -> bool {
        !self.retired.is_empty()
            || self
                .entries
                .values()
                .any(|entry| entry.spec.kind == InputKind::Secret)
    }

    pub fn put(
        &mut self,
        spec: InputSpec,
        value: String,
        replace: bool,
        now_ms: u64,
    ) -> Result<InputDescription, InputError> {
        let value = Zeroizing::new(value);
        let spec = self.validate_put(spec, &value, replace, now_ms)?;
        let description = describe(&spec, &value);
        if self
            .entries
            .get(&spec.name)
            .is_some_and(|entry| entry.spec == spec && entry.value == value)
        {
            return Ok(description);
        }
        self.retained_bytes += value.len();
        if let Some(old) = self.entries.insert(
            spec.name.clone(),
            Entry {
                spec,
                value,
                source: None,
            },
        ) {
            self.retire(old);
        }
        self.revision = self.revision.saturating_add(1);
        Ok(description)
    }

    /// A custody adapter can check a write before saving it. Before `put`, it
    /// must confirm the session revision has not changed during custody I/O.
    pub fn validate_put(
        &mut self,
        spec: InputSpec,
        value: &str,
        replace: bool,
        now_ms: u64,
    ) -> Result<InputSpec, InputError> {
        if self.closed {
            return Err(InputError::Closed);
        }
        let spec = spec.validate(now_ms)?;
        if value.len() > MAX_VALUE_BYTES
            || value.contains('\0')
            || (spec.kind == InputKind::Secret && value.is_empty())
        {
            return Err(InputError::InvalidValue);
        }
        self.expire(now_ms);
        if self.entries.contains_key(&spec.name) && !replace {
            return Err(InputError::AlreadyExists);
        }
        if self
            .entries
            .get(&spec.name)
            .is_some_and(|entry| entry.spec == spec && entry.value.as_str() == value)
        {
            return Ok(spec);
        }
        if (!self.entries.contains_key(&spec.name) && self.entries.len() >= MAX_INPUTS)
            || self.entries.len().saturating_add(self.retired.len()) >= 4096
            || self.retained_bytes.saturating_add(value.len()) > MAX_SESSION_BYTES
        {
            return Err(InputError::LimitReached);
        }
        Ok(spec)
    }

    pub fn list(&mut self, now_ms: u64) -> Vec<InputDescription> {
        self.expire(now_ms);
        self.entries
            .values()
            .map(|entry| describe(&entry.spec, &entry.value))
            .collect()
    }

    pub fn remove(&mut self, name: &str) -> bool {
        if let Some(entry) = self.entries.remove(name) {
            self.retire(entry);
            self.revision = self.revision.saturating_add(1);
            true
        } else {
            false
        }
    }

    /// The callback belongs to the trusted host, never to model-supplied code.
    /// Its result still needs `redact` before crossing the model seam.
    pub fn with_value<T>(
        &mut self,
        name: &str,
        usage: InputUse<'_>,
        now_ms: u64,
        effect: impl FnOnce(&str) -> T,
    ) -> Result<T, InputError> {
        if self.closed {
            return Err(InputError::Closed);
        }
        self.expire(now_ms);
        let entry = self.entries.get(name).ok_or(InputError::Unavailable)?;
        if let InputUsage::Websites { origins } = &entry.spec.usage {
            let InputUse::Http(destination) = usage else {
                return Err(InputError::UsageDenied);
            };
            let url = Url::parse(destination).map_err(|_| InputError::UsageDenied)?;
            if url.scheme() != "https"
                || !url.username().is_empty()
                || url.password().is_some()
                || !origins.contains(&url.origin().ascii_serialization())
            {
                return Err(InputError::UsageDenied);
            }
        }
        Ok(effect(&entry.value))
    }

    /// Filter exact values, longest first. Removed, expired, and replaced
    /// secrets remain covered while the store is retained for late output.
    pub fn redact(&self, text: &str) -> String {
        String::from_utf8(self.redact_bytes(text.as_bytes()))
            .expect("Replacing complete UTF-8 secrets preserves UTF-8")
    }

    /// Copy unmatched bytes unchanged, including non-text command output.
    pub fn redact_bytes(&self, text: &[u8]) -> Vec<u8> {
        let mut secrets: Vec<&[u8]> = self
            .entries
            .values()
            .filter(|entry| entry.spec.kind == InputKind::Secret)
            .map(|entry| entry.value.as_bytes())
            .chain(self.retired.iter().map(|value| value.as_bytes()))
            .collect();
        secrets.sort_unstable_by(|left, right| right.len().cmp(&left.len()).then(left.cmp(right)));
        secrets.dedup();
        let mut next_matches: Vec<Option<usize>> = secrets
            .iter()
            .map(|value| memchr::memmem::find(text, value))
            .collect();
        let mut result = Vec::new();
        let mut cursor = 0;
        let mut match_count = 0;
        loop {
            let next = next_matches
                .iter()
                .enumerate()
                .filter_map(|(index, start)| start.map(|start| (index, start)))
                .min_by_key(|(index, start)| (*start, std::cmp::Reverse(secrets[*index].len())));
            let Some((index, start)) = next else { break };
            match_count += 1;
            if match_count > MAX_REDACTION_MATCHES {
                return b"[redacted]".to_vec();
            }
            if start >= cursor {
                result.extend_from_slice(&text[cursor..start]);
                result.extend_from_slice(b"[redacted]");
            }
            cursor = cursor.max(start + secrets[index].len());
            // Match starts advance through the original text, including
            // overlaps. Replacement text is never searched again.
            let after_start = start + 1;
            next_matches[index] = memchr::memmem::find(&text[after_start..], secrets[index])
                .map(|next| after_start + next);
        }
        result.extend_from_slice(&text[cursor..]);
        result
    }

    /// Destroy the grants. Keep the bounded redaction set until this owner is
    /// dropped, protecting output already in flight during session shutdown.
    pub fn close(&mut self) {
        let entries = std::mem::take(&mut self.entries);
        for entry in entries.into_values() {
            self.retire(entry);
        }
        self.closed = true;
        self.revision = self.revision.saturating_add(1);
    }

    pub fn revision(&mut self, now_ms: u64) -> u64 {
        self.expire(now_ms);
        self.revision
    }

    /// Begin a new conversation, dropping private material while preserving
    /// transfer ordering for a host that reuses its control connection.
    pub fn reset(&mut self) {
        let revision = self.revision.saturating_add(1);
        let source_revisions = std::mem::take(&mut self.source_revisions);
        *self = Self {
            revision,
            source_revisions,
            ..Self::default()
        };
    }

    fn expire(&mut self, now_ms: u64) {
        let expired: Vec<String> = self.entries.iter()
            .filter(|(_, entry)| matches!(entry.spec.lifetime, InputLifetime::Until { unix_ms } if unix_ms <= now_ms))
            .map(|(name, _)| name.clone())
            .collect();
        for name in expired {
            self.remove(&name);
        }
    }

    fn retire(&mut self, entry: Entry) {
        if entry.spec.kind == InputKind::Secret {
            if self.retired.iter().any(|value| value == &entry.value)
                || self.entries.values().any(|active| {
                    active.spec.kind == InputKind::Secret && active.value == entry.value
                })
            {
                self.retained_bytes -= entry.value.len();
            } else {
                self.retired.push(entry.value);
            }
        } else {
            self.retained_bytes -= entry.value.len();
        }
    }
}

fn describe(spec: &InputSpec, value: &str) -> InputDescription {
    InputDescription {
        spec: spec.clone(),
        value: (spec.kind == InputKind::Variable).then(|| value.to_owned()),
    }
}

#[cfg(test)]
mod tests;
