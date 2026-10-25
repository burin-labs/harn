//! Choosing the session that should hear about a signal.

use crate::model::{Session, Signal};

/// Picks a recipient for a pending signal from the live sessions, or `None`
/// to hold it. Implementations must be deterministic for a given input: two
/// routing passes over the same state must agree, or a reminder could be
/// assigned twice. The inbox applies the configured [`crate::Policy`] before
/// calling a router, so a router never sees a muted or quiet-hours signal.
pub trait Router: Send + Sync {
    fn route(&self, signal: &Signal, live_sessions: &[&Session]) -> Option<String>;
}

/// The default rules: the session that heard the fired reminder, then a
/// repository or working-directory match, then the fallback role, else hold.
#[derive(Clone, Debug)]
pub struct RuleRouter {
    pub fallback_role: String,
}

impl Default for RuleRouter {
    fn default() -> Self {
        Self {
            fallback_role: "control".to_string(),
        }
    }
}

impl Router for RuleRouter {
    fn route(&self, signal: &Signal, live_sessions: &[&Session]) -> Option<String> {
        if let Some(recipient) = &signal.fired_recipient {
            if live_sessions.iter().any(|session| &session.id == recipient) {
                return Some(recipient.clone());
            }
        }
        let matching = best(
            live_sessions
                .iter()
                .copied()
                .filter(|session| matches_repo(signal, session) || matches_cwd(signal, session)),
        );
        if matching.is_some() {
            return matching;
        }
        best(
            live_sessions
                .iter()
                .copied()
                .filter(|session| session.role.as_deref() == Some(self.fallback_role.as_str())),
        )
    }
}

fn matches_repo(signal: &Signal, session: &Session) -> bool {
    match (&signal.routing.repo, &session.repo) {
        (Some(wanted), Some(have)) => wanted.eq_ignore_ascii_case(have),
        _ => false,
    }
}

fn matches_cwd(signal: &Signal, session: &Session) -> bool {
    signal
        .routing
        .cwd
        .as_ref()
        .is_some_and(|dir| session.cwd.starts_with(dir))
}

/// Most recently seen wins; the smallest id breaks a tie.
fn best<'a>(sessions: impl Iterator<Item = &'a Session>) -> Option<String> {
    sessions
        .max_by(|left, right| {
            left.last_seen
                .cmp(&right.last_seen)
                .then_with(|| right.id.cmp(&left.id))
        })
        .map(|session| session.id.clone())
}
