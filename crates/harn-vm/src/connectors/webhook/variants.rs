use std::collections::BTreeMap;

use futures::executor::block_on;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use crate::connectors::hmac::{verify_hmac_signed, HmacSignatureStyle};
use crate::connectors::ConnectorError;
use crate::event_log::AnyEventLog;
use crate::triggers::ProviderId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookSignatureVariant {
    Standard,
    Stripe,
    GitHub,
    Slack,
}

impl WebhookSignatureVariant {
    pub fn parse(raw: Option<&str>) -> Result<Self, ConnectorError> {
        match raw.unwrap_or("standard") {
            "standard" => Ok(Self::Standard),
            "stripe" => Ok(Self::Stripe),
            "github" => Ok(Self::GitHub),
            "slack" => Ok(Self::Slack),
            other => Err(ConnectorError::Unsupported(format!(
                "unsupported generic webhook signature scheme `{other}`; expected one of standard, stripe, github, slack"
            ))),
        }
    }

    pub fn default_timestamp_window(self) -> Option<Duration> {
        match self {
            Self::Standard | Self::Stripe | Self::Slack => Some(Duration::minutes(5)),
            Self::GitHub => None,
        }
    }

    pub fn verify(
        self,
        event_log: &AnyEventLog,
        provider: &ProviderId,
        body: &[u8],
        headers: &BTreeMap<String, String>,
        secret: &str,
        timestamp_window: Option<Duration>,
        now: OffsetDateTime,
    ) -> Result<(), ConnectorError> {
        let style = match self {
            Self::Standard => HmacSignatureStyle::standard_webhooks(),
            Self::Stripe => HmacSignatureStyle::stripe(),
            Self::GitHub => HmacSignatureStyle::github(),
            Self::Slack => HmacSignatureStyle::slack(),
        };
        block_on(verify_hmac_signed(
            event_log,
            provider,
            style,
            body,
            headers,
            secret,
            timestamp_window.or_else(|| self.default_timestamp_window()),
            now,
        ))
    }
}
