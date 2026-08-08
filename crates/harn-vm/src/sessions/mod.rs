use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::event_log::{AnyEventLog, EventId, EventLog, LogError, LogEvent, Topic};

pub const SESSIONS_TOPIC: &str = "orchestrator.sessions";

const SESSION_CREATED_KIND: &str = "session_created";
const SESSION_TOUCHED_KIND: &str = "session_touched";
const SESSION_EXPIRED_KIND: &str = "session_expired";
const GENERATED_TOKEN_PREFIX: &str = "harn_sess_";
const MIN_SESSION_TOKEN_CHARS: usize = 32;
const TOKEN_RANDOM_BYTES: usize = 32;
const TOKEN_HANDLE_PREFIX: &str = "sha256:v1:";

pub type SessionAttributes = BTreeMap<String, serde_json::Value>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub principal: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(default)]
    pub attributes: SessionAttributes,
}

impl Session {
    pub fn is_expired_at(&self, now: OffsetDateTime) -> bool {
        self.expires_at <= now
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateSession {
    pub id: Option<String>,
    pub principal: String,
    pub created_at: Option<OffsetDateTime>,
    pub expires_at: OffsetDateTime,
    pub attributes: SessionAttributes,
}

impl CreateSession {
    pub fn new(principal: impl Into<String>, expires_at: OffsetDateTime) -> Self {
        Self {
            id: None,
            principal: principal.into(),
            created_at: None,
            expires_at,
            attributes: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TouchSession {
    pub id: String,
    pub last_seen_at: OffsetDateTime,
    pub expires_at: Option<OffsetDateTime>,
    pub attributes: Option<SessionAttributes>,
}

impl TouchSession {
    pub fn new(id: impl Into<String>, last_seen_at: OffsetDateTime) -> Self {
        Self {
            id: id.into(),
            last_seen_at,
            expires_at: None,
            attributes: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpireSession {
    pub id: String,
    pub expired_at: OffsetDateTime,
}

impl ExpireSession {
    pub fn new(id: impl Into<String>, expired_at: OffsetDateTime) -> Self {
        Self {
            id: id.into(),
            expired_at,
        }
    }
}

#[derive(Clone)]
pub struct SessionStore {
    event_log: Arc<AnyEventLog>,
    topic: Topic,
    projection: Arc<Mutex<SessionProjection>>,
}

impl SessionStore {
    pub fn new(event_log: Arc<AnyEventLog>) -> Self {
        Self {
            event_log,
            topic: Topic::new(SESSIONS_TOPIC).expect("sessions topic is valid"),
            projection: Arc::new(Mutex::new(SessionProjection::default())),
        }
    }

    pub async fn create(&self, request: CreateSession) -> Result<Session, SessionError> {
        let created_at = request.created_at.unwrap_or_else(OffsetDateTime::now_utc);
        let id = match request.id {
            Some(id) => validate_session_id(id)?,
            None => generate_session_token(),
        };
        let token_handle = session_token_handle(&id)?;
        let principal = validate_principal(request.principal)?;
        if request.expires_at <= created_at {
            return Err(SessionError::Invalid(
                "expires_at must be later than created_at".to_string(),
            ));
        }

        self.refresh().await?;
        if self.has_seen_handle(&token_handle) {
            return Err(SessionError::Duplicate(id));
        }

        let record = SessionRecord {
            token_handle,
            principal,
            created_at,
            last_seen_at: created_at,
            expires_at: request.expires_at,
            attributes: request.attributes,
        };
        let created_event_id = self
            .append(
                SESSION_CREATED_KIND,
                serde_json::to_value(SessionCreatedPayload {
                    record: record.clone(),
                })?,
            )
            .await?;
        self.refresh().await?;
        if !self.created_by_event(&record.token_handle, created_event_id) {
            return Err(SessionError::Duplicate(id));
        }
        Ok(record.to_session(id))
    }

    pub async fn get(
        &self,
        id: &str,
        now: OffsetDateTime,
    ) -> Result<Option<Session>, SessionError> {
        let id = validate_session_id(id)?;
        let token_handle = session_token_handle(&id)?;
        self.refresh().await?;
        Ok(self.projected_session(&token_handle, id, now))
    }

    pub async fn touch(&self, request: TouchSession) -> Result<Option<Session>, SessionError> {
        let id = validate_session_id(request.id)?;
        let token_handle = session_token_handle(&id)?;
        self.refresh().await?;
        if self
            .projected_session(&token_handle, id.clone(), request.last_seen_at)
            .is_none()
        {
            return Ok(None);
        }
        if let Some(expires_at) = request.expires_at {
            if expires_at <= request.last_seen_at {
                return Err(SessionError::Invalid(
                    "expires_at must be later than last_seen_at".to_string(),
                ));
            }
        }

        self.append(
            SESSION_TOUCHED_KIND,
            serde_json::to_value(SessionTouchedPayload {
                token_handle: token_handle.clone(),
                last_seen_at: request.last_seen_at,
                expires_at: request.expires_at,
                attributes: request.attributes,
            })?,
        )
        .await?;
        self.refresh().await?;
        Ok(self.projected_session(&token_handle, id, request.last_seen_at))
    }

    pub async fn expire(&self, request: ExpireSession) -> Result<Option<Session>, SessionError> {
        let id = validate_session_id(request.id)?;
        let token_handle = session_token_handle(&id)?;
        self.refresh().await?;
        if !self.has_seen_handle(&token_handle) {
            return Ok(None);
        }
        self.append(
            SESSION_EXPIRED_KIND,
            serde_json::to_value(SessionExpiredPayload {
                token_handle: token_handle.clone(),
                expired_at: request.expired_at,
            })?,
        )
        .await?;
        self.refresh().await?;
        Ok(self
            .projection
            .lock()
            .expect("session projection poisoned")
            .sessions
            .get(&token_handle)
            .map(|entry| entry.record.to_session(id)))
    }

    async fn append(
        &self,
        kind: &'static str,
        payload: serde_json::Value,
    ) -> Result<EventId, SessionError> {
        self.event_log
            .append(&self.topic, LogEvent::new(kind, payload))
            .await
            .map_err(SessionError::from)
    }

    async fn refresh(&self) -> Result<(), SessionError> {
        let from = self
            .projection
            .lock()
            .expect("session projection poisoned")
            .last_event_id;
        let events = self
            .event_log
            .read_range(&self.topic, from, usize::MAX)
            .await?;
        if events.is_empty() {
            return Ok(());
        }

        let mut projection = self.projection.lock().expect("session projection poisoned");
        for (event_id, event) in events {
            if projection
                .last_event_id
                .is_some_and(|last_event_id| event_id <= last_event_id)
            {
                continue;
            }
            apply_event(&mut projection, event_id, event)?;
            projection.last_event_id = Some(event_id);
        }
        Ok(())
    }

    fn has_seen_handle(&self, token_handle: &str) -> bool {
        self.projection
            .lock()
            .expect("session projection poisoned")
            .seen_handles
            .contains(token_handle)
    }

    fn created_by_event(&self, token_handle: &str, event_id: EventId) -> bool {
        self.projection
            .lock()
            .expect("session projection poisoned")
            .sessions
            .get(token_handle)
            .is_some_and(|entry| entry.created_event_id == event_id)
    }

    fn projected_session(
        &self,
        token_handle: &str,
        id: impl Into<String>,
        now: OffsetDateTime,
    ) -> Option<Session> {
        let id = id.into();
        self.projection
            .lock()
            .expect("session projection poisoned")
            .sessions
            .get(token_handle)
            .filter(|entry| !entry.expired)
            .map(|entry| &entry.record)
            .filter(|record| !record.is_expired_at(now))
            .map(|record| record.to_session(id))
    }
}

pub async fn create(store: &SessionStore, request: CreateSession) -> Result<Session, SessionError> {
    store.create(request).await
}

pub async fn get(
    store: &SessionStore,
    id: &str,
    now: OffsetDateTime,
) -> Result<Option<Session>, SessionError> {
    store.get(id, now).await
}

pub async fn touch(
    store: &SessionStore,
    request: TouchSession,
) -> Result<Option<Session>, SessionError> {
    store.touch(request).await
}

pub async fn expire(
    store: &SessionStore,
    request: ExpireSession,
) -> Result<Option<Session>, SessionError> {
    store.expire(request).await
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    Duplicate(String),
    Invalid(String),
    Log(String),
    Serde(String),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate(id) => write!(f, "session '{id}' already exists"),
            Self::Invalid(message) | Self::Log(message) | Self::Serde(message) => message.fmt(f),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<LogError> for SessionError {
    fn from(value: LogError) -> Self {
        Self::Log(value.to_string())
    }
}

impl From<serde_json::Error> for SessionError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serde(value.to_string())
    }
}

#[derive(Default)]
struct SessionProjection {
    sessions: BTreeMap<String, ProjectedSession>,
    seen_handles: BTreeSet<String>,
    last_event_id: Option<EventId>,
}

struct ProjectedSession {
    record: SessionRecord,
    expired: bool,
    created_event_id: EventId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct SessionRecord {
    token_handle: String,
    principal: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    last_seen_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
    #[serde(default)]
    attributes: SessionAttributes,
}

impl SessionRecord {
    fn is_expired_at(&self, now: OffsetDateTime) -> bool {
        self.expires_at <= now
    }

    fn to_session(&self, id: impl Into<String>) -> Session {
        Session {
            id: id.into(),
            principal: self.principal.clone(),
            created_at: self.created_at,
            last_seen_at: self.last_seen_at,
            expires_at: self.expires_at,
            attributes: self.attributes.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SessionCreatedPayload {
    record: SessionRecord,
}

#[derive(Serialize, Deserialize)]
struct SessionTouchedPayload {
    token_handle: String,
    #[serde(with = "time::serde::rfc3339")]
    last_seen_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
    #[serde(default)]
    attributes: Option<SessionAttributes>,
}

#[derive(Serialize, Deserialize)]
struct SessionExpiredPayload {
    token_handle: String,
    #[serde(with = "time::serde::rfc3339")]
    expired_at: OffsetDateTime,
}

fn apply_event(
    projection: &mut SessionProjection,
    event_id: EventId,
    event: LogEvent,
) -> Result<(), SessionError> {
    match event.kind.as_str() {
        SESSION_CREATED_KIND => {
            let payload: SessionCreatedPayload = serde_json::from_value(event.payload)?;
            projection
                .seen_handles
                .insert(payload.record.token_handle.clone());
            projection
                .sessions
                .entry(payload.record.token_handle.clone())
                .or_insert_with(|| ProjectedSession {
                    record: payload.record,
                    expired: false,
                    created_event_id: event_id,
                });
        }
        SESSION_TOUCHED_KIND => {
            let payload: SessionTouchedPayload = serde_json::from_value(event.payload)?;
            if let Some(entry) = projection.sessions.get_mut(&payload.token_handle) {
                if !entry.expired {
                    entry.record.last_seen_at = entry.record.last_seen_at.max(payload.last_seen_at);
                    if let Some(expires_at) = payload.expires_at {
                        entry.record.expires_at = expires_at;
                    }
                    if let Some(attributes) = payload.attributes {
                        entry.record.attributes = attributes;
                    }
                }
            }
        }
        SESSION_EXPIRED_KIND => {
            let payload: SessionExpiredPayload = serde_json::from_value(event.payload)?;
            projection.seen_handles.insert(payload.token_handle.clone());
            if let Some(entry) = projection.sessions.get_mut(&payload.token_handle) {
                entry.expired = true;
                entry.record.expires_at = entry.record.expires_at.min(payload.expired_at);
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_session_id(id: impl Into<String>) -> Result<String, SessionError> {
    let id = id.into();
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return Err(SessionError::Invalid(
            "session id cannot be empty".to_string(),
        ));
    }
    if trimmed.chars().count() < MIN_SESSION_TOKEN_CHARS {
        return Err(SessionError::Invalid(format!(
            "session id must be at least {MIN_SESSION_TOKEN_CHARS} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn validate_principal(principal: impl Into<String>) -> Result<String, SessionError> {
    let principal = principal.into();
    let trimmed = principal.trim();
    if trimmed.is_empty() {
        return Err(SessionError::Invalid(
            "session principal cannot be empty".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

fn generate_session_token() -> String {
    let random: [u8; TOKEN_RANDOM_BYTES] = rand::random();
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
    format!("{GENERATED_TOKEN_PREFIX}{token}")
}

fn session_token_handle(id: &str) -> Result<String, SessionError> {
    let id = validate_session_id(id)?;
    let mut hasher = Sha256::new();
    hasher.update(b"harn.orchestrator.session-token.v1\0");
    hasher.update(id.as_bytes());
    Ok(format!(
        "{TOKEN_HANDLE_PREFIX}{}",
        hex::encode(hasher.finalize())
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::tempdir;
    use time::format_description::well_known::Rfc3339;

    use super::*;
    use crate::event_log::{FileEventLog, MemoryEventLog};

    const SESSION_ID: &str = "harn_sess_test_abcdefghijklmnopqrstuvwxyz0123456789";

    fn at(raw: &str) -> OffsetDateTime {
        OffsetDateTime::parse(raw, &Rfc3339).expect("valid rfc3339 timestamp")
    }

    fn memory_log() -> Arc<AnyEventLog> {
        Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)))
    }

    fn memory_store() -> SessionStore {
        SessionStore::new(memory_log())
    }

    #[tokio::test]
    async fn lifecycle_create_touch_expire_round_trip() {
        let store = memory_store();
        let created_at = at("2026-05-02T12:00:00Z");
        let touched_at = at("2026-05-02T12:30:00Z");
        let expires_at = at("2026-05-02T13:00:00Z");
        let mut attributes = BTreeMap::new();
        attributes.insert("role".to_string(), serde_json::json!("operator"));

        let created = store
            .create(CreateSession {
                id: Some(SESSION_ID.to_string()),
                principal: "user-1".to_string(),
                created_at: Some(created_at),
                expires_at,
                attributes: attributes.clone(),
            })
            .await
            .expect("create session");

        assert_eq!(created.id, SESSION_ID);
        assert_eq!(created.last_seen_at, created_at);
        assert_eq!(created.attributes, attributes);
        assert_eq!(
            store
                .get(SESSION_ID, created_at)
                .await
                .expect("get session")
                .expect("session exists"),
            created
        );

        let touched = store
            .touch(TouchSession::new(SESSION_ID, touched_at))
            .await
            .expect("touch session")
            .expect("active session");
        assert_eq!(touched.last_seen_at, touched_at);
        assert_eq!(touched.expires_at, expires_at);

        let expired = store
            .expire(ExpireSession::new(SESSION_ID, touched_at))
            .await
            .expect("expire session")
            .expect("known session");
        assert!(expired.is_expired_at(touched_at));
        assert!(store
            .get(SESSION_ID, touched_at)
            .await
            .expect("get expired session")
            .is_none());
    }

    #[tokio::test]
    async fn store_projection_replays_persisted_event_log() {
        let dir = tempdir().expect("tempdir");
        {
            let event_log = Arc::new(AnyEventLog::File(
                FileEventLog::open(dir.path().join("events"), 32).expect("open event log"),
            ));
            let first = SessionStore::new(event_log.clone());
            first
                .create(CreateSession {
                    id: Some(SESSION_ID.to_string()),
                    principal: "user-1".to_string(),
                    created_at: Some(at("2026-05-02T12:00:00Z")),
                    expires_at: at("2026-05-02T13:00:00Z"),
                    attributes: BTreeMap::new(),
                })
                .await
                .expect("create session");
        }

        let reopened_log = Arc::new(AnyEventLog::File(
            FileEventLog::open(dir.path().join("events"), 32).expect("reopen event log"),
        ));
        let restored = SessionStore::new(reopened_log);
        let session = restored
            .get(SESSION_ID, at("2026-05-02T12:30:00Z"))
            .await
            .expect("get restored session")
            .expect("session restored");
        assert_eq!(session.principal, "user-1");
    }

    #[tokio::test]
    async fn event_log_payloads_do_not_persist_raw_bearer_tokens() {
        let event_log = memory_log();
        let store = SessionStore::new(event_log.clone());
        store
            .create(CreateSession {
                id: Some(SESSION_ID.to_string()),
                principal: "user-1".to_string(),
                created_at: Some(at("2026-05-02T12:00:00Z")),
                expires_at: at("2026-05-02T13:00:00Z"),
                attributes: BTreeMap::new(),
            })
            .await
            .expect("create session");
        store
            .touch(TouchSession::new(SESSION_ID, at("2026-05-02T12:30:00Z")))
            .await
            .expect("touch session");
        store
            .expire(ExpireSession::new(SESSION_ID, at("2026-05-02T12:45:00Z")))
            .await
            .expect("expire session");

        let events = event_log
            .read_range(
                &Topic::new(SESSIONS_TOPIC).expect("sessions topic"),
                None,
                usize::MAX,
            )
            .await
            .expect("read session log");
        let payloads = events
            .into_iter()
            .map(|(_, event)| event.payload.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!payloads.contains(SESSION_ID));
        assert!(payloads.contains(TOKEN_HANDLE_PREFIX));
    }

    #[tokio::test]
    async fn expired_sessions_cannot_be_touched() {
        let store = memory_store();
        store
            .create(CreateSession {
                id: Some(SESSION_ID.to_string()),
                principal: "user-1".to_string(),
                created_at: Some(at("2026-05-02T12:00:00Z")),
                expires_at: at("2026-05-02T12:05:00Z"),
                attributes: BTreeMap::new(),
            })
            .await
            .expect("create session");

        assert!(store
            .touch(TouchSession::new(SESSION_ID, at("2026-05-02T12:06:00Z")))
            .await
            .expect("touch expired session")
            .is_none());
    }

    #[tokio::test]
    async fn duplicate_session_ids_are_rejected() {
        let store = memory_store();
        let request = CreateSession {
            id: Some(SESSION_ID.to_string()),
            principal: "user-1".to_string(),
            created_at: Some(at("2026-05-02T12:00:00Z")),
            expires_at: at("2026-05-02T13:00:00Z"),
            attributes: BTreeMap::new(),
        };
        store.create(request.clone()).await.expect("first create");

        let err = store.create(request).await.expect_err("duplicate rejected");
        assert_eq!(err, SessionError::Duplicate(SESSION_ID.to_string()));
    }

    #[tokio::test]
    async fn concurrent_duplicate_creates_return_one_winner() {
        let event_log = memory_log();
        let first = SessionStore::new(event_log.clone());
        let second = SessionStore::new(event_log);
        let request = CreateSession {
            id: Some(SESSION_ID.to_string()),
            principal: "user-1".to_string(),
            created_at: Some(at("2026-05-02T12:00:00Z")),
            expires_at: at("2026-05-02T13:00:00Z"),
            attributes: BTreeMap::new(),
        };

        let (left, right) = tokio::join!(first.create(request.clone()), second.create(request));
        #[allow(clippy::tuple_array_conversions)]
        let results = [left, right];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(
                    |result| matches!(result, Err(SessionError::Duplicate(id)) if id == SESSION_ID)
                )
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn generated_session_ids_are_high_entropy_tokens() {
        let store = memory_store();
        let session = store
            .create(CreateSession {
                id: None,
                principal: "user-1".to_string(),
                created_at: Some(at("2026-05-02T12:00:00Z")),
                expires_at: at("2026-05-02T13:00:00Z"),
                attributes: BTreeMap::new(),
            })
            .await
            .expect("create generated session");

        assert!(session.id.starts_with(GENERATED_TOKEN_PREFIX));
        assert!(session.id.len() >= MIN_SESSION_TOKEN_CHARS);
    }
}
