//! Push session-metadata changes to a connected ACP client.
//!
//! A title is written by whoever owns the naming decision — an HTTP `PATCH`,
//! a host's titling pass, a rename. A client that already rendered that name
//! has no way to learn it moved and keeps showing the stale one until
//! something else makes it re-list. Subscribing to the store's committed
//! changes closes that gap for writes made in this process.
//!
//! Scope, stated plainly. Two limits, both deliberate:
//!
//! * Only sessions this client opened are forwarded. Observers are registered
//!   process-wide, so without that filter a client connected for one workspace
//!   would be told the titles of sessions in every other workspace open in the
//!   same process.
//! * Only writes through stores opened by this VM are seen. A write from a
//!   *different* process reaches the same database file but no in-process
//!   observer, so two surfaces running as separate processes do not yet see
//!   each other's renames this way.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use harn_session_store::{SessionChangeObserver, SessionMeta};

use super::bridge::AcpOutput;
use super::session::session_info_update_params;

/// Session ids one ACP server has opened, shared with its notifier.
pub(super) type KnownSessions = Arc<Mutex<HashSet<String>>>;

/// Forwards a committed session-metadata change to one ACP output, for the
/// sessions that output's client actually has.
pub(super) struct SessionInfoNotifier {
    output: AcpOutput,
    known: KnownSessions,
}

impl SessionInfoNotifier {
    pub(super) fn new(output: AcpOutput, known: KnownSessions) -> Self {
        Self { output, known }
    }
}

impl SessionChangeObserver for SessionInfoNotifier {
    fn session_updated(&self, meta: &SessionMeta) {
        let known = match self.known.lock() {
            Ok(known) => known.contains(&meta.id),
            Err(poisoned) => poisoned.into_inner().contains(&meta.id),
        };
        if !known {
            return;
        }
        let params =
            session_info_update_params(&meta.id, meta.title.as_deref(), &serde_json::Map::new());
        let notification = harn_vm::jsonrpc::notification("session/update", params);
        if let Ok(line) = serde_json::to_string(&notification) {
            self.output.write_line(&line);
        }
    }
}

#[cfg(test)]
mod tests {
    use harn_session_store::{CreateSession, SessionStore, UpdateSession};
    use tokio::sync::mpsc;

    use super::super::{AcpServer, AcpServerConfig};
    use super::*;

    fn subscribed_client(
        known: &[&str],
    ) -> (
        mpsc::UnboundedReceiver<String>,
        harn_vm::SessionChangeSubscription,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let sessions: KnownSessions = Arc::new(Mutex::new(
            known.iter().map(|id| (*id).to_string()).collect(),
        ));
        let subscription = harn_vm::subscribe_session_changes(Arc::new(SessionInfoNotifier::new(
            AcpOutput::Channel(tx),
            sessions,
        )));
        (rx, subscription)
    }

    async fn write_title(root: &std::path::Path, session_id: &str, title: &str) {
        let store = harn_vm::open_canonical_store(root).expect("canonical store");
        if store.describe(session_id).await.is_err() {
            store
                .create(CreateSession {
                    id: Some(session_id.to_string()),
                    ..CreateSession::default()
                })
                .await
                .expect("create session");
        }
        store
            .update(
                session_id,
                UpdateSession {
                    title: Some(title.to_string()),
                    ..UpdateSession::default()
                },
            )
            .await
            .expect("title write");
    }

    /// The claim is not "a notifier type exists" — it is that a title written
    /// through the ordinary store path reaches a connected client without the
    /// client asking again. Drive the real write and read the wire.
    #[tokio::test(flavor = "current_thread")]
    async fn a_committed_title_reaches_the_client_as_session_info_update() {
        let workspace = tempfile::tempdir().expect("workspace");
        let (mut rx, _subscription) = subscribed_client(&["s-1"]);

        write_title(workspace.path(), "s-1", "Fix flaky retry backoff").await;

        let line = rx.try_recv().expect("a session/update frame");
        let frame: serde_json::Value = serde_json::from_str(&line).expect("valid json frame");
        assert_eq!(frame["method"], "session/update");
        assert_eq!(frame["params"]["sessionId"], "s-1");
        assert_eq!(
            frame["params"]["update"]["sessionUpdate"],
            "session_info_update"
        );
        assert_eq!(
            frame["params"]["update"]["title"],
            "Fix flaky retry backoff"
        );
    }

    /// Observers are registered process-wide, so without a per-client filter a
    /// client would be told the titles of sessions in every other workspace
    /// open in the same process.
    #[tokio::test(flavor = "current_thread")]
    async fn a_session_this_client_never_opened_is_not_pushed_to_it() {
        let workspace = tempfile::tempdir().expect("workspace");
        let (mut rx, _subscription) = subscribed_client(&["mine"]);

        write_title(
            workspace.path(),
            "someone-elses",
            "a name from another client",
        )
        .await;
        assert!(
            rx.try_recv().is_err(),
            "a session this client never opened must not be pushed to it"
        );

        write_title(workspace.path(), "mine", "a name from my own session").await;
        let line = rx.try_recv().expect("my own session still arrives");
        let frame: serde_json::Value = serde_json::from_str(&line).expect("valid json frame");
        assert_eq!(frame["params"]["sessionId"], "mine");
    }

    /// A server that is gone must stop receiving, or a long-lived process
    /// accumulates sinks writing into transports nobody reads.
    #[tokio::test(flavor = "current_thread")]
    async fn dropping_the_server_unsubscribes_it() {
        let workspace = tempfile::tempdir().expect("workspace");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
        server.track_known_session("s-2");
        drop(server);

        write_title(workspace.path(), "s-2", "after the server went away").await;
        assert!(
            rx.try_recv().is_err(),
            "a dropped server must not still receive session changes"
        );
    }
}
