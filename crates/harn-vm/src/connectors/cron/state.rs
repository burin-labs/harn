use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::connectors::ConnectorError;
use crate::event_log::{AnyEventLog, EventLog, LogEvent, Topic};

pub(crate) const CRON_STATE_TOPIC: &str = "connectors.cron.state";
const STATE_EVENT_KIND: &str = "cron_trigger_state";

#[derive(Clone)]
pub(crate) struct CronStateStore {
    event_log: Arc<AnyEventLog>,
    topic: Topic,
}

impl CronStateStore {
    pub(crate) fn new(event_log: Arc<AnyEventLog>) -> Self {
        Self {
            event_log,
            topic: Topic::new(CRON_STATE_TOPIC).expect("cron state topic is valid"),
        }
    }

    pub(crate) async fn load_all(
        &self,
    ) -> Result<BTreeMap<String, PersistedCronState>, ConnectorError> {
        let mut state = BTreeMap::new();
        let events = self
            .event_log
            .read_range(&self.topic, None, usize::MAX)
            .await
            .map_err(ConnectorError::from)?;
        for (_, record) in events {
            if record.kind != STATE_EVENT_KIND {
                continue;
            }
            let payload: PersistedCronState =
                serde_json::from_value(record.payload).map_err(ConnectorError::from)?;
            state.insert(payload.trigger_id.clone(), payload);
        }
        Ok(state)
    }

    pub(crate) async fn load(
        &self,
        trigger_id: &str,
    ) -> Result<Option<PersistedCronState>, ConnectorError> {
        Ok(self.load_all().await?.remove(trigger_id))
    }

    pub(crate) async fn persist(&self, state: PersistedCronState) -> Result<(), ConnectorError> {
        let payload = serde_json::to_value(&state).map_err(ConnectorError::from)?;
        self.event_log
            .append(&self.topic, LogEvent::new(STATE_EVENT_KIND, payload))
            .await
            .map_err(ConnectorError::from)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PersistedCronState {
    pub trigger_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub last_fired_at: OffsetDateTime,
}
