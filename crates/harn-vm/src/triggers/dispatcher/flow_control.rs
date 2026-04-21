use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use time::OffsetDateTime;
use tokio::sync::{Mutex, Notify};

use crate::connectors::cron::scheduler::Clock;
use crate::event_log::{
    sanitize_topic_component, AnyEventLog, EventLog, LogError, LogEvent, Topic,
};
use crate::triggers::test_util::clock;
use crate::TriggerEvent;

#[derive(Debug)]
pub enum BatchDecision {
    Dispatch(Vec<TriggerEvent>),
    Merged,
}

#[derive(Clone, Debug)]
pub struct ConcurrencyPermit {
    gate: String,
}

#[derive(Debug, Default)]
struct FlowControlState {
    concurrency_active: HashMap<String, u32>,
    concurrency_waiters: HashMap<String, Vec<ConcurrencyWaiter>>,
    singleton_active: HashSet<String>,
    throttle_hits: HashMap<String, VecDeque<OffsetDateTime>>,
    rate_limit_hits: HashMap<String, VecDeque<OffsetDateTime>>,
    debounce_latest: HashMap<String, u64>,
    batch_groups: HashMap<String, BatchGroup>,
    batch_consumed: HashSet<u64>,
}

#[derive(Clone, Debug)]
struct ConcurrencyWaiter {
    token: u64,
    priority_rank: usize,
    queued_order: u64,
}

#[derive(Clone, Debug)]
struct BatchMember {
    token: u64,
    event: TriggerEvent,
}

#[derive(Clone, Debug)]
struct BatchGroup {
    leader: u64,
    deadline: OffsetDateTime,
    members: Vec<BatchMember>,
}

#[derive(Clone)]
pub struct FlowControlManager {
    event_log: Arc<AnyEventLog>,
    state: Arc<Mutex<FlowControlState>>,
    notify: Arc<Notify>,
    sequence: Arc<AtomicU64>,
}

impl std::fmt::Debug for FlowControlManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlowControlManager").finish_non_exhaustive()
    }
}

impl FlowControlManager {
    pub fn new(event_log: Arc<AnyEventLog>) -> Self {
        Self {
            event_log,
            state: Arc::new(Mutex::new(FlowControlState::default())),
            notify: Arc::new(Notify::new()),
            sequence: Arc::new(AtomicU64::new(1)),
        }
    }

    pub async fn debounce(&self, gate: &str, period: Duration) -> Result<bool, LogError> {
        let token = self.sequence.fetch_add(1, Ordering::Relaxed);
        {
            let mut state = self.state.lock().await;
            state.debounce_latest.insert(gate.to_string(), token);
        }
        self.append_event(
            "debounce",
            gate,
            "debounce_seen",
            json!({"gate": gate, "token": token}),
        )
        .await?;
        sleep_duration(period).await;
        let latest = {
            let state = self.state.lock().await;
            state.debounce_latest.get(gate).copied()
        };
        let latest = latest == Some(token);
        if latest {
            self.append_event(
                "debounce",
                gate,
                "debounce_selected",
                json!({"gate": gate, "token": token}),
            )
            .await?;
        }
        Ok(latest)
    }

    pub async fn check_rate_limit(
        &self,
        gate: &str,
        period: Duration,
        max: u32,
    ) -> Result<bool, LogError> {
        let now = clock::now_utc();
        let allowed = {
            let mut state = self.state.lock().await;
            let hits = state.rate_limit_hits.entry(gate.to_string()).or_default();
            trim_window(hits, now, period);
            if hits.len() >= max as usize {
                false
            } else {
                hits.push_back(now);
                true
            }
        };
        self.append_event(
            "rate_limit",
            gate,
            if allowed {
                "rate_limit_allowed"
            } else {
                "rate_limit_blocked"
            },
            json!({"gate": gate, "max": max}),
        )
        .await?;
        Ok(allowed)
    }

    pub async fn wait_for_throttle(
        &self,
        gate: &str,
        period: Duration,
        max: u32,
    ) -> Result<(), LogError> {
        loop {
            let wait_for = {
                let now = clock::now_utc();
                let mut state = self.state.lock().await;
                let hits = state.throttle_hits.entry(gate.to_string()).or_default();
                trim_window(hits, now, period);
                if hits.len() < max as usize {
                    hits.push_back(now);
                    None
                } else {
                    hits.front().map(|first| {
                        let deadline =
                            *first + time::Duration::try_from(period).unwrap_or_default();
                        (deadline - now)
                            .try_into()
                            .unwrap_or(Duration::from_millis(1))
                    })
                }
            };
            match wait_for {
                Some(delay) => {
                    self.append_event(
                        "throttle",
                        gate,
                        "throttle_wait",
                        json!({"gate": gate, "delay_ms": delay.as_millis()}),
                    )
                    .await?;
                    sleep_duration(delay).await;
                }
                None => {
                    self.append_event(
                        "throttle",
                        gate,
                        "throttle_acquired",
                        json!({"gate": gate, "max": max}),
                    )
                    .await?;
                    return Ok(());
                }
            }
        }
    }

    pub async fn try_acquire_singleton(&self, gate: &str) -> Result<bool, LogError> {
        let acquired = {
            let mut state = self.state.lock().await;
            state.singleton_active.insert(gate.to_string())
        };
        self.append_event(
            "singleton",
            gate,
            if acquired {
                "singleton_acquired"
            } else {
                "singleton_skipped"
            },
            json!({"gate": gate}),
        )
        .await?;
        Ok(acquired)
    }

    pub async fn acquire_singleton(&self, gate: &str) -> Result<(), LogError> {
        loop {
            if self.try_acquire_singleton(gate).await? {
                return Ok(());
            }
            self.notify.notified().await;
        }
    }

    pub async fn release_singleton(&self, gate: &str) -> Result<(), LogError> {
        {
            let mut state = self.state.lock().await;
            state.singleton_active.remove(gate);
        }
        self.notify.notify_waiters();
        self.append_event(
            "singleton",
            gate,
            "singleton_released",
            json!({"gate": gate}),
        )
        .await
    }

    pub async fn acquire_concurrency(
        &self,
        gate: &str,
        max: u32,
        priority_rank: usize,
    ) -> Result<ConcurrencyPermit, LogError> {
        let token = self.sequence.fetch_add(1, Ordering::Relaxed);
        loop {
            let acquired = {
                let mut state = self.state.lock().await;
                let head_token = {
                    let waiters = state
                        .concurrency_waiters
                        .entry(gate.to_string())
                        .or_default();
                    if !waiters.iter().any(|waiter| waiter.token == token) {
                        waiters.push(ConcurrencyWaiter {
                            token,
                            priority_rank,
                            queued_order: token,
                        });
                    }
                    waiters.sort_by(|left, right| {
                        left.priority_rank
                            .cmp(&right.priority_rank)
                            .then(left.queued_order.cmp(&right.queued_order))
                    });
                    waiters.first().map(|waiter| waiter.token)
                };
                let active = state
                    .concurrency_active
                    .entry(gate.to_string())
                    .or_default();
                if *active < max && head_token == Some(token) {
                    *active += 1;
                    if let Some(waiters) = state.concurrency_waiters.get_mut(gate) {
                        waiters.retain(|waiter| waiter.token != token);
                    }
                    true
                } else {
                    false
                }
            };
            if acquired {
                self.append_event(
                    "concurrency",
                    gate,
                    "concurrency_acquired",
                    json!({"gate": gate, "token": token, "max": max}),
                )
                .await?;
                return Ok(ConcurrencyPermit {
                    gate: gate.to_string(),
                });
            }
            self.notify.notified().await;
        }
    }

    pub async fn release_concurrency(&self, permit: ConcurrencyPermit) -> Result<(), LogError> {
        {
            let mut state = self.state.lock().await;
            if let Some(active) = state.concurrency_active.get_mut(&permit.gate) {
                *active = active.saturating_sub(1);
                if *active == 0 {
                    state.concurrency_active.remove(&permit.gate);
                }
            }
        }
        self.notify.notify_waiters();
        self.append_event(
            "concurrency",
            &permit.gate,
            "concurrency_released",
            json!({"gate": permit.gate}),
        )
        .await
    }

    pub async fn consume_batch(
        &self,
        gate: &str,
        size: u32,
        timeout: Duration,
        event: TriggerEvent,
    ) -> Result<BatchDecision, LogError> {
        let token = self.sequence.fetch_add(1, Ordering::Relaxed);
        {
            let mut state = self.state.lock().await;
            let group = state
                .batch_groups
                .entry(gate.to_string())
                .or_insert_with(|| BatchGroup {
                    leader: token,
                    deadline: clock::now_utc()
                        + time::Duration::try_from(timeout).unwrap_or_default(),
                    members: Vec::new(),
                });
            group.members.push(BatchMember { token, event });
        }
        self.notify.notify_waiters();
        self.append_event(
            "batch",
            gate,
            "batch_enqueued",
            json!({"gate": gate, "token": token, "size": size}),
        )
        .await?;

        loop {
            let maybe_batch = {
                let mut state = self.state.lock().await;
                if state.batch_consumed.remove(&token) {
                    return Ok(BatchDecision::Merged);
                }
                let now = clock::now_utc();
                let Some(group) = state.batch_groups.get_mut(gate) else {
                    continue;
                };
                if group.leader == token
                    && (group.members.len() >= size as usize || now >= group.deadline)
                {
                    let members = std::mem::take(&mut group.members);
                    for member in members.iter().skip(1) {
                        state.batch_consumed.insert(member.token);
                    }
                    state.batch_groups.remove(gate);
                    Some(
                        members
                            .into_iter()
                            .map(|member| member.event)
                            .collect::<Vec<_>>(),
                    )
                } else {
                    None
                }
            };
            if let Some(events) = maybe_batch {
                self.notify.notify_waiters();
                self.append_event(
                    "batch",
                    gate,
                    "batch_dispatched",
                    json!({"gate": gate, "count": events.len()}),
                )
                .await?;
                return Ok(BatchDecision::Dispatch(events));
            }

            let deadline = {
                let state = self.state.lock().await;
                state.batch_groups.get(gate).map(|group| group.deadline)
            };
            match deadline {
                Some(deadline) => {
                    let notified = self.notify.notified();
                    tokio::pin!(notified);
                    tokio::select! {
                        _ = sleep_until(deadline) => {}
                        _ = &mut notified => {}
                    }
                }
                None => self.notify.notified().await,
            }
        }
    }

    async fn append_event(
        &self,
        primitive: &str,
        gate: &str,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<(), LogError> {
        let topic = Topic::new(format!(
            "trigger.{primitive}.{}",
            sanitize_topic_component(gate)
        ))?;
        self.event_log
            .append(&topic, LogEvent::new(kind, payload))
            .await
            .map(|_| ())
    }
}

fn trim_window(hits: &mut VecDeque<OffsetDateTime>, now: OffsetDateTime, period: Duration) {
    let period = time::Duration::try_from(period).unwrap_or_default();
    while let Some(first) = hits.front().copied() {
        if now - first >= period {
            hits.pop_front();
        } else {
            break;
        }
    }
}

async fn sleep_duration(duration: Duration) {
    if duration.is_zero() {
        return;
    }
    if let Some(mock_clock) = clock::active_mock_clock() {
        mock_clock
            .sleep_until(clock::now_utc() + time::Duration::try_from(duration).unwrap_or_default())
            .await;
    } else {
        tokio::time::sleep(duration).await;
    }
}

async fn sleep_until(deadline: OffsetDateTime) {
    let now = clock::now_utc();
    if deadline <= now {
        return;
    }
    if let Ok(duration) = (deadline - now).try_into() {
        sleep_duration(duration).await;
    }
}
