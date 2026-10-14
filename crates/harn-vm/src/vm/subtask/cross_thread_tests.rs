//! Falsifier: a subtask must observe its parent's execution context while it
//! runs on a different OS thread.
//!
//! Moving subtasks onto worker threads only pays off if the context a subtask
//! reads moves with it. Three things are checked here because each broke a
//! different way before the move:
//!
//! - the per-dispatch call budget, which `harn-serve` used to require be
//!   installed "on the same OS thread the VM runs on";
//! - the agent session id, which the filesystem write chokepoint reads to
//!   attribute `files_written`;
//! - the event log handle, which must be the SAME log, not a per-thread copy.
//!
//! The test drives the production seam: [`prepare`] and [`spawn_into`] are the
//! exact functions `parallel`, `parallel each`, `parallel settle`, `spawn`,
//! and pool submit call. A hand-built equivalent would keep passing after the
//! seam broke.

use std::sync::Arc;
use std::thread::ThreadId;

use super::{prepare, scope_placement, spawn_into, SubtaskPlacement};
use crate::call_budget::{charge_mcp_call, install_mcp_call_budget, mcp_calls_spent};
use crate::stdlib::pool::PoolRegistry;

/// What one subtask saw about itself and its inherited context.
struct Observation {
    thread: ThreadId,
    session: Option<String>,
    event_log: Option<usize>,
}

/// One branch is sufficient: `Runtime::block_on` polls the parent on this test
/// thread, while the production worker placement uses `tokio::spawn`.
const BRANCHES: usize = 1;

struct Measured {
    parent_thread: ThreadId,
    parent_session: Option<String>,
    parent_log: usize,
    observations: Vec<Observation>,
    spent: Option<u64>,
}

#[test]
fn subtask_inherits_budget_session_and_event_log_across_threads() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("multi-thread runtime");

    crate::agent_sessions::reset_session_store();
    crate::call_budget::reset_call_budget_state();

    let measured = runtime.block_on(scope_placement(SubtaskPlacement::Worker, async {
        // Parent context: a ceiling with room for one charge per branch, an
        // agent session, and an event log.
        let _budget = install_mcp_call_budget(BRANCHES as u64);
        let _session = crate::agent_sessions::enter_current_session("parent-session");
        let log = crate::event_log::install_memory_for_current_thread(64);

        let parent_thread = std::thread::current().id();
        let parent_session = crate::agent_sessions::current_session_id();
        let parent_log = Arc::as_ptr(&log) as usize;

        let registry = Arc::new(PoolRegistry::default());
        let mut set: tokio::task::JoinSet<Observation> = tokio::task::JoinSet::new();
        for _ in 0..BRANCHES {
            let branch = prepare(Arc::clone(&registry), async {
                charge_mcp_call().expect("branch charge is within the ceiling");
                Observation {
                    thread: std::thread::current().id(),
                    session: crate::agent_sessions::current_session_id(),
                    event_log: crate::event_log::active_event_log()
                        .map(|log| Arc::as_ptr(&log) as usize),
                }
            });
            spawn_into(&mut set, branch);
        }

        let mut observations = Vec::with_capacity(BRANCHES);
        while let Some(joined) = set.join_next().await {
            observations.push(joined.expect("branch joins"));
        }
        Measured {
            parent_thread,
            parent_session,
            parent_log,
            observations,
            spent: mcp_calls_spent(),
        }
    }));

    assert_eq!(measured.observations.len(), BRANCHES);

    // The point of the change: at least one branch really left the parent's
    // thread. Without this the inheritance assertions below would pass
    // vacuously on a same-thread run.
    let migrated = measured
        .observations
        .iter()
        .filter(|observed| observed.thread != measured.parent_thread)
        .count();
    assert!(
        migrated > 0,
        "no branch ran on a thread other than the parent's ({:?}); \
         the inheritance assertions below would prove nothing",
        measured.parent_thread
    );

    for observed in &measured.observations {
        assert_eq!(
            observed.session, measured.parent_session,
            "a branch on {:?} lost the parent's agent session",
            observed.thread
        );
        assert_eq!(
            observed.event_log,
            Some(measured.parent_log),
            "a branch on {:?} wrote to a different event log than its parent",
            observed.thread
        );
    }

    // One shared counter, not one per branch. A per-branch copy would leave
    // the parent's count at 0 and let every branch spend the whole ceiling.
    assert_eq!(
        measured.spent,
        Some(BRANCHES as u64),
        "the parent's call budget did not see its subtasks' charges"
    );
}

/// harn#9816: a host scopes its session's secret provider onto the task that
/// runs the VM. A `parallel` branch is a new task, and without inheriting that
/// scope a `sub_agent_run` inside it resolved provider keys through the
/// configured chain and failed to find one its creator held.
#[test]
fn subtask_inherits_the_active_secret_provider_across_threads() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    let provider: Arc<dyn crate::secrets::SecretProvider> = Arc::new(
        crate::secrets::MemorySecretProvider::new("parent-session-secrets"),
    );

    let observed = runtime.block_on(scope_placement(
        SubtaskPlacement::Worker,
        crate::secrets::with_active_secret_provider(Some(provider), async {
            assert!(crate::secrets::active_secret_provider().is_some());
            let registry = Arc::new(PoolRegistry::default());
            let mut set: tokio::task::JoinSet<Option<String>> = tokio::task::JoinSet::new();
            spawn_into(
                &mut set,
                prepare(registry, async {
                    crate::secrets::active_secret_provider()
                        .map(|provider| provider.namespace().to_string())
                }),
            );
            set.join_next()
                .await
                .expect("one branch")
                .expect("branch joins")
        }),
    ));

    assert_eq!(observed.as_deref(), Some("parent-session-secrets"));
}
