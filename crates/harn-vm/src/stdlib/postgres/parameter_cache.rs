use super::*;

type ParameterCache = SyncMutex<BTreeMap<String, Arc<Vec<PgTypeInfo>>>>;

// Actual uncached probes and their savepoint commands. Thread-local counters
// keep current-thread VM fixtures independent; none exist in release builds.
#[cfg(test)]
thread_local! {
    static DESCRIBE_ROUND_TRIPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static DESCRIBE_SAVEPOINT_ROUND_TRIPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn describe_round_trips() -> u64 {
    DESCRIBE_ROUND_TRIPS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(super) fn describe_savepoint_round_trips() -> u64 {
    DESCRIBE_SAVEPOINT_ROUND_TRIPS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(super) fn reset_describe_round_trips() {
    DESCRIBE_ROUND_TRIPS.with(|c| c.set(0));
    DESCRIBE_SAVEPOINT_ROUND_TRIPS.with(|c| c.set(0));
}

#[cfg(test)]
pub(super) fn bump_describe_round_trips() {
    DESCRIBE_ROUND_TRIPS.with(|c| c.set(c.get() + 1));
}

#[cfg(test)]
pub(super) fn bump_describe_savepoint_round_trips() {
    DESCRIBE_SAVEPOINT_ROUND_TRIPS.with(|c| c.set(c.get() + 1));
}

/// Only the first caller statement has the pool's original SQL context.
/// Arbitrary SQL can change search_path or other name-resolution state, so
/// every executed caller statement consumes that known context. Even repeating
/// identical SQL can change name resolution through a called function, so later
/// nil statements infer types afresh instead of retaining context-dependent OIDs.
pub(super) struct TransactionParameterCache {
    initial: SyncMutex<Option<Arc<ParameterCache>>>,
}

impl TransactionParameterCache {
    pub(super) fn new(pool: &Arc<ParameterCache>) -> Self {
        Self {
            initial: SyncMutex::new(Some(Arc::clone(pool))),
        }
    }

    /// Called for every caller statement, including statements without nils.
    /// The transaction connection lock serializes this transition and execution.
    pub(super) fn begin_statement(&self) -> Option<Arc<ParameterCache>> {
        self.initial.lock().take()
    }

    pub(super) async fn describe(
        &self,
        initial: Option<Arc<ParameterCache>>,
        conn: &mut sqlx_postgres::PgConnection,
        sql: &str,
        builtin: &str,
    ) -> Result<Arc<Vec<PgTypeInfo>>, VmError> {
        if let Some(cache) = initial {
            described_param_oids(&cache, conn, sql, builtin, true).await
        } else {
            describe_param_oids_uncached(conn, sql, builtin, true)
                .await
                .map(Arc::new)
        }
    }
}
