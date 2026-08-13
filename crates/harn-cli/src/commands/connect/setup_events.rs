use serde::Serialize;

use crate::json_envelope::JsonEnvelope;

pub(crate) const CONNECT_SETUP_EVENT_SCHEMA_VERSION: u32 = 1;
pub(crate) const CONNECT_SETUP_EVENT_SCHEMA: &str = "harn.connector_setup.event.v1";
#[cfg(test)]
pub(crate) const CONNECTOR_SETUP_STAGES: &[&str] = &[
    "resolving",
    "opening_browser",
    "waiting_for_user",
    "exchanging",
    "storing",
    "validating",
    "ready",
];
#[cfg(test)]
pub(crate) const CONNECTOR_SETUP_STATUSES: &[&str] = &[
    "in_progress",
    "succeeded",
    "failed",
    "cancelled",
    "timed_out",
];
#[cfg(test)]
pub(crate) const CONNECTOR_SETUP_INTERACTIONS: &[&str] =
    &["none", "browser", "secret_entry", "user_code"];
#[cfg(test)]
pub(crate) const CONNECTOR_SETUP_ERROR_CODES: &[&str] = &[
    "connector_unavailable",
    "configuration_missing",
    "browser_open_failed",
    "callback_timeout",
    "user_denied",
    "state_mismatch",
    "token_exchange_failed",
    "credential_store_failed",
    "validation_failed",
    "cancelled",
    "unknown",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ConnectorSetupStage {
    Resolving,
    OpeningBrowser,
    WaitingForUser,
    Exchanging,
    Storing,
    Validating,
    Ready,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // The shared vocabulary includes setup modes not yet used by OAuth.
pub(super) enum ConnectorSetupStatus {
    InProgress,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // API-key and device-code hosts consume the same closed vocabulary.
pub(super) enum ConnectorSetupInteraction {
    None,
    Browser,
    SecretEntry,
    UserCode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // Some terminal codes are emitted by hosts around the CLI process.
pub(super) enum ConnectorSetupErrorCode {
    ConnectorUnavailable,
    ConfigurationMissing,
    BrowserOpenFailed,
    CallbackTimeout,
    UserDenied,
    StateMismatch,
    TokenExchangeFailed,
    CredentialStoreFailed,
    ValidationFailed,
    Cancelled,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ConnectorSetupFailure {
    pub(super) status: ConnectorSetupStatus,
    pub(super) code: ConnectorSetupErrorCode,
    pub(super) stage: ConnectorSetupStage,
    pub(super) detail: String,
}

impl ConnectorSetupFailure {
    pub(super) fn failed(
        code: ConnectorSetupErrorCode,
        stage: ConnectorSetupStage,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            status: ConnectorSetupStatus::Failed,
            code,
            stage,
            detail: detail.into(),
        }
    }

    pub(super) fn timed_out(stage: ConnectorSetupStage, detail: impl Into<String>) -> Self {
        Self {
            status: ConnectorSetupStatus::TimedOut,
            code: ConnectorSetupErrorCode::CallbackTimeout,
            stage,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for ConnectorSetupFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct ConnectorSetupEvent {
    schema: &'static str,
    sequence: u64,
    connector: String,
    stage: ConnectorSetupStage,
    status: ConnectorSetupStatus,
    interaction: ConnectorSetupInteraction,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<ConnectorSetupErrorCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery: Option<String>,
}

pub(super) struct ConnectorSetupReporter {
    connector: String,
    json: bool,
    sequence: u64,
}

impl ConnectorSetupReporter {
    pub(super) fn new(connector: impl Into<String>, json: bool) -> Self {
        Self {
            connector: connector.into(),
            json,
            sequence: 0,
        }
    }

    pub(super) fn progress(
        &mut self,
        stage: ConnectorSetupStage,
        interaction: ConnectorSetupInteraction,
        message: impl Into<String>,
    ) {
        self.emit(
            stage,
            ConnectorSetupStatus::InProgress,
            interaction,
            message,
            None,
            None,
        );
    }

    pub(super) fn succeeded(&mut self, message: impl Into<String>) {
        self.emit(
            ConnectorSetupStage::Ready,
            ConnectorSetupStatus::Succeeded,
            ConnectorSetupInteraction::None,
            message,
            None,
            None,
        );
    }

    pub(super) fn failed(&mut self, failure: &ConnectorSetupFailure) {
        self.emit(
            failure.stage,
            failure.status,
            ConnectorSetupInteraction::None,
            plain_failure_message(failure.code),
            Some(failure.code),
            Some(recovery_for(failure.code).to_string()),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        stage: ConnectorSetupStage,
        status: ConnectorSetupStatus,
        interaction: ConnectorSetupInteraction,
        message: impl Into<String>,
        error_code: Option<ConnectorSetupErrorCode>,
        recovery: Option<String>,
    ) {
        self.sequence += 1;
        let event = ConnectorSetupEvent {
            schema: CONNECT_SETUP_EVENT_SCHEMA,
            sequence: self.sequence,
            connector: self.connector.clone(),
            stage,
            status,
            interaction,
            message: message.into(),
            error_code,
            recovery,
        };
        if self.json {
            match encode_event(&event) {
                Ok(line) => println!("{line}"),
                Err(error) => eprintln!("error: failed to encode connector setup event: {error}"),
            }
        } else {
            println!("{}", event.message);
        }
    }
}

fn encode_event(event: &ConnectorSetupEvent) -> Result<String, serde_json::Error> {
    serde_json::to_string(&JsonEnvelope::ok(CONNECT_SETUP_EVENT_SCHEMA_VERSION, event))
}

fn plain_failure_message(code: ConnectorSetupErrorCode) -> &'static str {
    match code {
        ConnectorSetupErrorCode::ConnectorUnavailable => "This service is not available.",
        ConnectorSetupErrorCode::ConfigurationMissing => {
            "This service needs more setup information."
        }
        ConnectorSetupErrorCode::BrowserOpenFailed => "The sign-in page could not be opened.",
        ConnectorSetupErrorCode::CallbackTimeout => "Sign-in timed out.",
        ConnectorSetupErrorCode::UserDenied => "Sign-in was not approved.",
        ConnectorSetupErrorCode::StateMismatch => "The sign-in response could not be verified.",
        ConnectorSetupErrorCode::TokenExchangeFailed => "The service did not finish sign-in.",
        ConnectorSetupErrorCode::CredentialStoreFailed => {
            "The signed-in account could not be saved."
        }
        ConnectorSetupErrorCode::ValidationFailed => "The connected service could not be verified.",
        ConnectorSetupErrorCode::Cancelled => "Setup was cancelled.",
        ConnectorSetupErrorCode::Unknown => "Service setup failed.",
    }
}

fn recovery_for(code: ConnectorSetupErrorCode) -> &'static str {
    match code {
        ConnectorSetupErrorCode::ConnectorUnavailable => {
            "Install or enable the service, then try again."
        }
        ConnectorSetupErrorCode::ConfigurationMissing => {
            "Review the service setup requirements, then try again."
        }
        ConnectorSetupErrorCode::BrowserOpenFailed => "Check your default browser and try again.",
        ConnectorSetupErrorCode::CallbackTimeout => {
            "Start setup again and finish the browser step within five minutes."
        }
        ConnectorSetupErrorCode::UserDenied => {
            "Review the requested permissions and start setup again if you want to connect."
        }
        ConnectorSetupErrorCode::StateMismatch => "Close the sign-in page and start setup again.",
        ConnectorSetupErrorCode::TokenExchangeFailed => {
            "Try setup again. If it still fails, check the service configuration."
        }
        ConnectorSetupErrorCode::CredentialStoreFailed => {
            "Check access to the operating system credential store, then try again."
        }
        ConnectorSetupErrorCode::ValidationFailed => {
            "Reconnect the service or review its permissions."
        }
        ConnectorSetupErrorCode::Cancelled => "Start setup again when you are ready.",
        ConnectorSetupErrorCode::Unknown => {
            "Try again. Use service diagnostics if the problem continues."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_is_one_versioned_secret_free_json_line() {
        let event = ConnectorSetupEvent {
            schema: CONNECT_SETUP_EVENT_SCHEMA,
            sequence: 3,
            connector: "google_workspace".to_string(),
            stage: ConnectorSetupStage::WaitingForUser,
            status: ConnectorSetupStatus::InProgress,
            interaction: ConnectorSetupInteraction::Browser,
            message: "Finish sign-in in your browser.".to_string(),
            error_code: None,
            recovery: None,
        };
        let line = encode_event(&event).unwrap();
        assert!(!line.contains('\n'));
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["data"]["schema"], CONNECT_SETUP_EVENT_SCHEMA);
        assert_eq!(value["data"]["stage"], "waiting_for_user");
        assert_eq!(value["data"]["interaction"], "browser");
    }

    #[test]
    fn callback_timeout_has_typed_terminal_recovery() {
        let failure = ConnectorSetupFailure::timed_out(
            ConnectorSetupStage::WaitingForUser,
            "OAuth callback timed out",
        );
        assert_eq!(failure.status, ConnectorSetupStatus::TimedOut);
        assert_eq!(failure.code, ConnectorSetupErrorCode::CallbackTimeout);
        assert!(recovery_for(failure.code).contains("five minutes"));
    }

    #[test]
    fn rust_setup_enums_match_the_published_wire_lists() {
        let stages = [
            ConnectorSetupStage::Resolving,
            ConnectorSetupStage::OpeningBrowser,
            ConnectorSetupStage::WaitingForUser,
            ConnectorSetupStage::Exchanging,
            ConnectorSetupStage::Storing,
            ConnectorSetupStage::Validating,
            ConnectorSetupStage::Ready,
        ];
        let statuses = [
            ConnectorSetupStatus::InProgress,
            ConnectorSetupStatus::Succeeded,
            ConnectorSetupStatus::Failed,
            ConnectorSetupStatus::Cancelled,
            ConnectorSetupStatus::TimedOut,
        ];
        let interactions = [
            ConnectorSetupInteraction::None,
            ConnectorSetupInteraction::Browser,
            ConnectorSetupInteraction::SecretEntry,
            ConnectorSetupInteraction::UserCode,
        ];
        let error_codes = [
            ConnectorSetupErrorCode::ConnectorUnavailable,
            ConnectorSetupErrorCode::ConfigurationMissing,
            ConnectorSetupErrorCode::BrowserOpenFailed,
            ConnectorSetupErrorCode::CallbackTimeout,
            ConnectorSetupErrorCode::UserDenied,
            ConnectorSetupErrorCode::StateMismatch,
            ConnectorSetupErrorCode::TokenExchangeFailed,
            ConnectorSetupErrorCode::CredentialStoreFailed,
            ConnectorSetupErrorCode::ValidationFailed,
            ConnectorSetupErrorCode::Cancelled,
            ConnectorSetupErrorCode::Unknown,
        ];
        assert_eq!(serialized_values(&stages), CONNECTOR_SETUP_STAGES);
        assert_eq!(serialized_values(&statuses), CONNECTOR_SETUP_STATUSES);
        assert_eq!(
            serialized_values(&interactions),
            CONNECTOR_SETUP_INTERACTIONS
        );
        assert_eq!(serialized_values(&error_codes), CONNECTOR_SETUP_ERROR_CODES);
    }

    fn serialized_values<T: Serialize>(values: &[T]) -> Vec<String> {
        values
            .iter()
            .map(|value| {
                serde_json::to_value(value)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }
}
