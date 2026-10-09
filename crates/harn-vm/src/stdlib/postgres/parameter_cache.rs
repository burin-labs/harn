use super::*;

type ParameterCache = SyncMutex<BTreeMap<String, Arc<Vec<PgTypeInfo>>>>;

/// Only the first caller statement has the pool's original SQL context.
/// Arbitrary SQL can change search_path or other name-resolution state, so
/// subsequent statements use transaction-local metadata. That metadata is known
/// only while the caller repeats the same described statement without another
/// opaque statement or a rollback changing its SQL context.
pub(super) struct TransactionParameterCache {
    initial: SyncMutex<Option<Arc<ParameterCache>>>,
    local: ParameterCache,
}

impl TransactionParameterCache {
    pub(super) fn new(pool: &Arc<ParameterCache>) -> Self {
        Self {
            initial: SyncMutex::new(Some(Arc::clone(pool))),
            local: SyncMutex::new(BTreeMap::new()),
        }
    }

    /// Called for every caller statement, including statements without nils.
    /// The transaction connection lock serializes this transition and execution.
    pub(super) fn begin_statement(&self, sql: &str, has_nil: bool) -> Option<Arc<ParameterCache>> {
        let initial = self.initial.lock().take();
        let mut local = self.local.lock();
        if !has_nil || !local.contains_key(sql) {
            // Caller SQL is opaque. Another statement may change name
            // resolution, including through functions, without a SQL keyword
            // classification or a context-read round trip.
            local.clear();
        }
        initial
    }

    pub(super) fn invalidate(&self) {
        self.initial.lock().take();
        self.local.lock().clear();
    }

    pub(super) async fn describe(
        &self,
        initial: Option<Arc<ParameterCache>>,
        conn: &mut sqlx_postgres::PgConnection,
        sql: &str,
        builtin: &str,
    ) -> Result<Arc<Vec<PgTypeInfo>>, VmError> {
        let cache = initial.as_deref().unwrap_or(&self.local);
        let oids = described_param_oids(cache, conn, sql, builtin, true).await?;
        if initial.is_some() {
            // Carry only this statement into the transaction, never unrelated
            // pool entries whose context later caller SQL could invalidate.
            self.local.lock().insert(sql.to_owned(), Arc::clone(&oids));
        }
        Ok(oids)
    }
}
