//! Typed producer identity projected into canonical event headers.

use std::collections::BTreeMap;

/// Reserved canonical header keys used to correlate an event with its producer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EventIdentityField {
    RunId,
    TurnId,
    SourceEventId,
    MessageId,
    ToolCallId,
}

impl EventIdentityField {
    pub const ALL: [Self; 5] = [
        Self::RunId,
        Self::TurnId,
        Self::SourceEventId,
        Self::MessageId,
        Self::ToolCallId,
    ];

    pub const fn header_name(self) -> &'static str {
        match self {
            Self::RunId => "run_id",
            Self::TurnId => "turn_id",
            Self::SourceEventId => "source_event_id",
            Self::MessageId => "message_id",
            Self::ToolCallId => "tool_call_id",
        }
    }
}

/// Producer-stamped run, turn, message, and tool correlation values.
///
/// The store persists these values in the existing signed `headers` member of
/// `harn.session.event.v1`; adding identity therefore needs no schema fork.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventIdentity {
    values: BTreeMap<EventIdentityField, String>,
}

impl EventIdentity {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a field, rejecting blank/control-bearing or conflicting values.
    pub fn with(
        mut self,
        field: EventIdentityField,
        value: impl Into<String>,
    ) -> Result<Self, EventIdentityError> {
        self.insert(field, value)?;
        Ok(self)
    }

    pub fn insert(
        &mut self,
        field: EventIdentityField,
        value: impl Into<String>,
    ) -> Result<(), EventIdentityError> {
        let value = normalize_value(field, value.into())?;
        if let Some(existing) = self.values.get(&field) {
            if existing != &value {
                return Err(EventIdentityError::Conflict {
                    field,
                    existing: existing.clone(),
                    incoming: value,
                });
            }
            return Ok(());
        }
        self.values.insert(field, value);
        Ok(())
    }

    pub fn get(&self, field: EventIdentityField) -> Option<&str> {
        self.values.get(&field).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Parse and normalize only the reserved identity keys in `headers`.
    pub fn from_headers(headers: &BTreeMap<String, String>) -> Result<Self, EventIdentityError> {
        let mut identity = Self::new();
        for field in EventIdentityField::ALL {
            if let Some(value) = headers.get(field.header_name()) {
                identity.insert(field, value.clone())?;
            }
        }
        Ok(identity)
    }

    /// Project identity into headers without silently replacing a producer ID.
    pub fn apply_to_headers(
        &self,
        headers: &mut BTreeMap<String, String>,
    ) -> Result<(), EventIdentityError> {
        for (&field, value) in &self.values {
            let key = field.header_name();
            if let Some(existing) = headers.get(key) {
                let existing = normalize_value(field, existing.clone())?;
                if existing != *value {
                    return Err(EventIdentityError::Conflict {
                        field,
                        existing,
                        incoming: value.clone(),
                    });
                }
            }
        }
        for (&field, value) in &self.values {
            headers.insert(field.header_name().to_string(), value.clone());
        }
        Ok(())
    }
}

/// Normalize reserved identity headers in place at the store boundary.
pub(crate) fn normalize_identity_headers(
    headers: &mut BTreeMap<String, String>,
) -> Result<EventIdentity, EventIdentityError> {
    let identity = EventIdentity::from_headers(headers)?;
    identity.apply_to_headers(headers)?;
    Ok(identity)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventIdentityError {
    Invalid {
        field: EventIdentityField,
        reason: &'static str,
    },
    Conflict {
        field: EventIdentityField,
        existing: String,
        incoming: String,
    },
}

impl std::fmt::Display for EventIdentityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid { field, reason } => {
                write!(formatter, "invalid {}: {reason}", field.header_name())
            }
            Self::Conflict {
                field,
                existing,
                incoming,
            } => write!(
                formatter,
                "conflicting {} values '{existing}' and '{incoming}'",
                field.header_name()
            ),
        }
    }
}

impl std::error::Error for EventIdentityError {}

fn normalize_value(field: EventIdentityField, value: String) -> Result<String, EventIdentityError> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(EventIdentityError::Invalid {
            field,
            reason: "value must not be blank",
        });
    }
    if normalized.chars().any(char::is_control) {
        return Err(EventIdentityError::Invalid {
            field,
            reason: "value must not contain control characters",
        });
    }
    Ok(normalized.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_normalizes_and_round_trips_headers() {
        let identity = EventIdentity::new()
            .with(EventIdentityField::RunId, " run-1 ")
            .unwrap()
            .with(EventIdentityField::TurnId, "turn-1")
            .unwrap();
        let mut headers = BTreeMap::from([("traceparent".to_string(), "trace-1".to_string())]);

        identity.apply_to_headers(&mut headers).unwrap();

        assert_eq!(headers["run_id"], "run-1");
        assert_eq!(EventIdentity::from_headers(&headers).unwrap(), identity);
        assert_eq!(headers["traceparent"], "trace-1");
    }

    #[test]
    fn identity_rejects_conflicting_producer_values() {
        let identity = EventIdentity::new()
            .with(EventIdentityField::RunId, "run-2")
            .unwrap();
        let mut headers = BTreeMap::from([("run_id".to_string(), "run-1".to_string())]);

        let error = identity.apply_to_headers(&mut headers).unwrap_err();

        assert!(matches!(error, EventIdentityError::Conflict { .. }));
    }
}
