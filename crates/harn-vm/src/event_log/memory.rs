use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use futures::stream::BoxStream;

use super::util::{prepare_event_after, stream_from_broadcast, BroadcastMap};
use super::{
    AppendOutcome, CompactReport, ConsumerId, EventId, EventLog, EventLogBackendKind,
    EventLogDescription, LogError, LogEvent, Topic,
};

#[derive(Default)]
struct MemoryState {
    topics: HashMap<String, VecDeque<(EventId, LogEvent)>>,
    latest: HashMap<String, EventId>,
    consumers: HashMap<(String, String), EventId>,
}

pub struct MemoryEventLog {
    state: Mutex<MemoryState>,
    pub(super) broadcasts: BroadcastMap,
    pub(super) queue_depth: usize,
}

impl MemoryEventLog {
    pub fn new(queue_depth: usize) -> Self {
        Self {
            state: Mutex::new(MemoryState::default()),
            broadcasts: BroadcastMap::default(),
            queue_depth: queue_depth.max(1),
        }
    }

    fn state(&self) -> Result<std::sync::MutexGuard<'_, MemoryState>, LogError> {
        self.state
            .lock()
            .map_err(|_| LogError::Io("memory event log state poisoned".to_string()))
    }

    pub(super) async fn topics(&self) -> Result<Vec<Topic>, LogError> {
        let state = self.state()?;
        let mut topics = state
            .topics
            .keys()
            .map(|topic| Topic::new(topic.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        topics.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        Ok(topics)
    }

    pub(super) async fn append_idempotent_by_header(
        &self,
        topic: &Topic,
        header: &str,
        value: &str,
        event: LogEvent,
    ) -> Result<AppendOutcome, LogError> {
        let mut state = self.state()?;
        if let Some((event_id, existing)) = state
            .topics
            .get(topic.as_str())
            .into_iter()
            .flat_map(|events| events.iter())
            .find(|(_, event)| {
                event
                    .headers
                    .get(header)
                    .is_some_and(|found| found == value)
            })
        {
            return Ok(AppendOutcome {
                event_id: *event_id,
                event: existing.clone(),
                inserted: false,
            });
        }

        let event_id = state.latest.get(topic.as_str()).copied().unwrap_or(0) + 1;
        let previous = state
            .topics
            .get(topic.as_str())
            .and_then(|events| events.back())
            .map(|(previous_id, previous_event)| (*previous_id, previous_event));
        let event = prepare_event_after(topic, event_id, previous, event)?;
        state.latest.insert(topic.as_str().to_string(), event_id);
        state
            .topics
            .entry(topic.as_str().to_string())
            .or_default()
            .push_back((event_id, event.clone()));
        drop(state);
        self.broadcasts
            .publish(topic, self.queue_depth, (event_id, event.clone()));
        Ok(AppendOutcome {
            event_id,
            event,
            inserted: true,
        })
    }

    /// Read counterpart of [`Self::append_idempotent_by_header`]. The in-memory
    /// backend has no header index, so this scans the topic — acceptable for a
    /// dev/test backend (SQLite is the durable default).
    pub(super) async fn read_idempotent_by_header(
        &self,
        topic: &Topic,
        header: &str,
        value: &str,
    ) -> Result<Option<(EventId, LogEvent)>, LogError> {
        let state = self.state()?;
        Ok(state
            .topics
            .get(topic.as_str())
            .into_iter()
            .flat_map(|events| events.iter())
            .find(|(_, event)| {
                event
                    .headers
                    .get(header)
                    .is_some_and(|found| found == value)
            })
            .map(|(event_id, event)| (*event_id, event.clone())))
    }
}

impl EventLog for MemoryEventLog {
    fn describe(&self) -> EventLogDescription {
        EventLogDescription {
            backend: EventLogBackendKind::Memory,
            location: None,
            size_bytes: None,
            queue_depth: self.queue_depth,
        }
    }

    async fn append(&self, topic: &Topic, event: LogEvent) -> Result<EventId, LogError> {
        let mut state = self.state()?;
        let event_id = state.latest.get(topic.as_str()).copied().unwrap_or(0) + 1;
        let previous = state
            .topics
            .get(topic.as_str())
            .and_then(|events| events.back())
            .map(|(previous_id, previous_event)| (*previous_id, previous_event));
        let event = prepare_event_after(topic, event_id, previous, event)?;
        state.latest.insert(topic.as_str().to_string(), event_id);
        state
            .topics
            .entry(topic.as_str().to_string())
            .or_default()
            .push_back((event_id, event.clone()));
        drop(state);
        self.broadcasts
            .publish(topic, self.queue_depth, (event_id, event));
        Ok(event_id)
    }

    async fn flush(&self) -> Result<(), LogError> {
        Ok(())
    }

    async fn read_range(
        &self,
        topic: &Topic,
        from: Option<EventId>,
        limit: usize,
    ) -> Result<Vec<(EventId, LogEvent)>, LogError> {
        let from = from.unwrap_or(0);
        let state = self.state()?;
        let events = state
            .topics
            .get(topic.as_str())
            .into_iter()
            .flat_map(|events| events.iter())
            .filter(|(event_id, _)| *event_id > from)
            .take(limit)
            .map(|(event_id, event)| (*event_id, event.clone()))
            .collect();
        Ok(events)
    }

    async fn subscribe(
        self: Arc<Self>,
        topic: &Topic,
        from: Option<EventId>,
    ) -> Result<BoxStream<'static, Result<(EventId, LogEvent), LogError>>, LogError> {
        let rx = self.broadcasts.subscribe(topic, self.queue_depth);
        let history = self.read_range(topic, from, usize::MAX).await?;
        Ok(stream_from_broadcast(history, from, rx, self.queue_depth))
    }

    async fn ack(
        &self,
        topic: &Topic,
        consumer: &ConsumerId,
        up_to: EventId,
    ) -> Result<(), LogError> {
        let mut state = self.state()?;
        state.consumers.insert(
            (topic.as_str().to_string(), consumer.as_str().to_string()),
            up_to,
        );
        Ok(())
    }

    async fn consumer_cursor(
        &self,
        topic: &Topic,
        consumer: &ConsumerId,
    ) -> Result<Option<EventId>, LogError> {
        let state = self.state()?;
        Ok(state
            .consumers
            .get(&(topic.as_str().to_string(), consumer.as_str().to_string()))
            .copied())
    }

    async fn latest(&self, topic: &Topic) -> Result<Option<EventId>, LogError> {
        let state = self.state()?;
        Ok(state.latest.get(topic.as_str()).copied())
    }

    async fn compact(&self, topic: &Topic, before: EventId) -> Result<CompactReport, LogError> {
        let mut state = self.state()?;
        let Some(events) = state.topics.get_mut(topic.as_str()) else {
            return Ok(CompactReport::default());
        };
        let removed = events
            .iter()
            .take_while(|(event_id, _)| *event_id <= before)
            .count();
        for _ in 0..removed {
            events.pop_front();
        }
        Ok(CompactReport {
            removed,
            remaining: events.len(),
            latest: state.latest.get(topic.as_str()).copied(),
            checkpointed: false,
        })
    }
}
