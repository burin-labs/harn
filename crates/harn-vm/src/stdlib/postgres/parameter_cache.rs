use super::*;

type ParameterCache = SyncMutex<BTreeMap<String, Arc<Vec<PgTypeInfo>>>>;

/// Only the first caller statement has the pool's original SQL context.
/// Arbitrary SQL can change search_path or other name-resolution state, so
/// subsequent statements retain the original transaction-local cache scope.
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
