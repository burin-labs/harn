//! Private host-to-runtime projection. This is never a model/context schema.

use super::*;
use zeroize::Zeroize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrivateInputSnapshot {
    source_id: String,
    revision: u64,
    inputs: Vec<PrivateGrant>,
    redactions: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrivateGrant {
    spec: InputSpec,
    value: String,
}

impl Drop for PrivateGrant {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}

impl Drop for PrivateInputSnapshot {
    fn drop(&mut self) {
        self.redactions.zeroize();
    }
}

#[derive(Serialize)]
struct BorrowedGrant<'a> {
    spec: &'a InputSpec,
    value: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BorrowedSnapshot<'a> {
    source_id: &'a str,
    revision: u64,
    inputs: Vec<BorrowedGrant<'a>>,
    redactions: Vec<&'a str>,
}

impl SessionInputs {
    /// Write only to a private IPC channel or a zeroizing transport buffer.
    pub fn write_private_snapshot(
        &mut self,
        source_id: &str,
        now_ms: u64,
        writer: impl std::io::Write,
    ) -> Result<(), InputError> {
        validate_source(source_id)?;
        self.expire(now_ms);
        let snapshot = BorrowedSnapshot {
            source_id,
            revision: self.revision,
            inputs: self
                .entries
                .values()
                .map(|entry| BorrowedGrant {
                    spec: &entry.spec,
                    value: &entry.value,
                })
                .collect(),
            redactions: if self.closed {
                Vec::new()
            } else {
                self.retired.iter().map(|value| value.as_str()).collect()
            },
        };
        serde_json::to_writer(writer, &snapshot).map_err(|_| InputError::InvalidSnapshot)
    }

    /// Atomically install one host revision. A delayed transfer cannot restore
    /// a grant that a newer change removed. Invalid transfers change nothing.
    pub fn apply_private_snapshot(
        &mut self,
        mut snapshot: PrivateInputSnapshot,
        now_ms: u64,
    ) -> Result<bool, InputError> {
        if self.closed {
            return Err(InputError::Closed);
        }
        validate_source(&snapshot.source_id)?;
        if let Some(current) = self.source_revisions.get(&snapshot.source_id) {
            if snapshot.revision < *current {
                return Err(InputError::StaleSnapshot);
            }
            if snapshot.revision == *current {
                return Ok(false);
            }
        }
        if snapshot.inputs.len() > MAX_INPUTS
            || snapshot.redactions.len() > 4096
            || !self.source_revisions.contains_key(&snapshot.source_id)
                && self.source_revisions.len() >= MAX_INPUTS
        {
            return Err(InputError::InvalidSnapshot);
        }
        let mut next = Self::default();
        // A host replaces only its own grants. Preconfigured values and grants
        // from other authenticated hosts remain under their original owner.
        for entry in self
            .entries
            .values()
            .filter(|entry| entry.source.as_deref() != Some(snapshot.source_id.as_str()))
        {
            if matches!(entry.spec.lifetime, InputLifetime::Until { unix_ms } if unix_ms <= now_ms)
            {
                continue;
            }
            next.put(entry.spec.clone(), entry.value.to_string(), false, now_ms)?;
            next.entries.get_mut(&entry.spec.name).unwrap().source = entry.source.clone();
        }
        for grant in &mut snapshot.inputs {
            next.put(
                grant.spec.clone(),
                std::mem::take(&mut grant.value),
                false,
                now_ms,
            )?;
            next.entries.get_mut(&grant.spec.name).unwrap().source =
                Some(snapshot.source_id.clone());
        }
        for value in &mut snapshot.redactions {
            next.retain_redaction(std::mem::take(value))?;
        }
        for value in &self.retired {
            next.retain_redaction(value.to_string())?;
        }
        for entry in self
            .entries
            .values()
            .filter(|entry| entry.spec.kind == InputKind::Secret)
        {
            next.retain_redaction(entry.value.to_string())?;
        }
        next.source_revisions = self.source_revisions.clone();
        next.source_revisions
            .insert(snapshot.source_id.clone(), snapshot.revision);
        next.revision = self.revision.saturating_add(1);
        *self = next;
        Ok(true)
    }

    fn retain_redaction(&mut self, value: String) -> Result<(), InputError> {
        let value = Zeroizing::new(value);
        if value.is_empty() || value.len() > MAX_VALUE_BYTES {
            return Err(InputError::InvalidSnapshot);
        }
        if self.retired.iter().any(|existing| existing == &value)
            || self
                .entries
                .values()
                .any(|entry| entry.spec.kind == InputKind::Secret && entry.value == value)
        {
            return Ok(());
        }
        if self.retained_bytes.saturating_add(value.len()) > MAX_SESSION_BYTES
            || self.retired.len() >= 4096
        {
            return Err(InputError::LimitReached);
        }
        self.retained_bytes += value.len();
        self.retired.push(value);
        Ok(())
    }
}

fn validate_source(source: &str) -> Result<(), InputError> {
    if source.is_empty()
        || source.len() > 128
        || !source
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(InputError::InvalidSnapshot);
    }
    Ok(())
}
