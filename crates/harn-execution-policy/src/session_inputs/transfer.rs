//! Private host-to-runtime projection. This is never a model/context schema.

use super::*;
use zeroize::Zeroize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrivateInputSnapshot {
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
    revision: u64,
    inputs: Vec<BorrowedGrant<'a>>,
    redactions: Vec<&'a str>,
}

impl SessionInputs {
    /// Write only to a private IPC channel or a zeroizing transport buffer.
    pub fn write_private_snapshot(
        &mut self,
        now_ms: u64,
        writer: impl std::io::Write,
    ) -> Result<(), InputError> {
        self.expire(now_ms);
        let snapshot = BorrowedSnapshot {
            revision: self.revision,
            inputs: self
                .entries
                .values()
                .map(|entry| BorrowedGrant {
                    spec: &entry.spec,
                    value: &entry.value,
                })
                .collect(),
            redactions: self.retired.iter().map(|value| value.as_str()).collect(),
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
        if let Some(current) = self.source_revision {
            if snapshot.revision < current {
                return Err(InputError::StaleSnapshot);
            }
            if snapshot.revision == current {
                return Ok(false);
            }
        }
        if snapshot.inputs.len() > MAX_INPUTS || snapshot.redactions.len() > 4096 {
            return Err(InputError::InvalidSnapshot);
        }
        let mut next = Self::default();
        for grant in &mut snapshot.inputs {
            next.put(
                grant.spec.clone(),
                std::mem::take(&mut grant.value),
                false,
                now_ms,
            )?;
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
        next.source_revision = Some(snapshot.revision);
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
