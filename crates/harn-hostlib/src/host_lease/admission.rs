use super::*;

#[derive(Clone, Debug)]
pub(super) struct WaiterIdentity {
    pub(super) waiter_id: String,
    pub(super) requested_at_ms: i64,
    pub(super) recoverable: bool,
}

impl HostLeaseStore {
    pub(super) fn enqueue_waiter(
        &self,
        resource: &HostLeaseResourceKey,
        priority_class: HostLeasePriorityClass,
        identity: &WaiterIdentity,
        deadline_at_ms: i64,
        owner_pid: Option<u32>,
    ) -> Result<HostLeaseQueueEvidence, HostLeaseError> {
        let owner_process_identity = owner_pid
            .map(|pid| match self.process_inspector.observe(pid) {
                ProcessObservation::Alive { identity } => Ok(identity),
                ProcessObservation::Dead => Err(HostLeaseError::InvalidRequest(
                    "waiter owner_pid is not a live local process".to_string(),
                )),
                ProcessObservation::Unknown => Err(HostLeaseError::InvalidRequest(
                    "waiter owner_pid liveness could not be verified".to_string(),
                )),
            })
            .transpose()?;
        let mut conn = self.connection(SQLITE_MUTATION_BUSY_TIMEOUT)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        cleanup_waiters(&tx, unix_now_ms()?, self.process_inspector.as_ref())?;
        upsert_waiter(
            &tx,
            resource,
            priority_class,
            identity,
            deadline_at_ms,
            owner_pid,
            owner_process_identity,
        )?;
        let (evidence, _) = queue_evidence(&tx, resource, &identity.waiter_id)?;
        tx.commit()?;
        self.signal_waiters();
        Ok(evidence)
    }

    pub(super) fn remove_waiter(&self, waiter_id: &str) -> Result<(), HostLeaseError> {
        let waiter_id = normalize_component("waiter_id", waiter_id)?;
        let conn = self.connection(SQLITE_MUTATION_BUSY_TIMEOUT)?;
        let removed = conn.execute(
            "DELETE FROM host_lease_waiters WHERE waiter_id = ?1",
            params![waiter_id],
        )? > 0;
        if removed {
            self.signal_waiters();
        }
        Ok(())
    }

    pub(super) fn try_acquire_once(
        &self,
        request: HostLeaseRequest,
        started_at: Option<Instant>,
        deadline_at_ms: Option<i64>,
        identity: &WaiterIdentity,
    ) -> Result<HostLeaseAcquireReceipt, HostLeaseError> {
        self.try_acquire_once_with_registry_timeout(
            request,
            started_at,
            deadline_at_ms,
            identity,
            SQLITE_MUTATION_BUSY_TIMEOUT,
        )
    }

    pub(super) fn try_acquire_once_with_registry_timeout(
        &self,
        request: HostLeaseRequest,
        started_at: Option<Instant>,
        deadline_at_ms: Option<i64>,
        identity: &WaiterIdentity,
        registry_timeout: Duration,
    ) -> Result<HostLeaseAcquireReceipt, HostLeaseError> {
        let request = normalize_request(request)?;
        let mut conn = self.connection(registry_timeout)?;
        let tx = match conn.transaction_with_behavior(TransactionBehavior::Immediate) {
            Ok(tx) => tx,
            Err(error) if sqlite_is_busy(&error) => {
                let now = unix_now_ms()?;
                return Ok(registry_busy_receipt(
                    request.host,
                    request.resource_class,
                    request.domain,
                    now,
                    started_at.map(|started| duration_ms_u64(started.elapsed())),
                    deadline_at_ms,
                ));
            }
            Err(error) => return Err(error.into()),
        };
        let now = unix_now_ms()?;
        let waited_ms = started_at
            .map(|started| duration_ms_u64(started.elapsed()))
            .unwrap_or(0);
        let host = request.host.clone();
        let resource_class = request.resource_class;
        let domain = request.domain.clone();
        let result = match self.acquire_in_transaction(
            tx,
            request,
            now,
            deadline_at_ms,
            waited_ms,
            identity,
        ) {
            Err(HostLeaseError::Database(error)) if sqlite_is_busy(&error) => {
                Ok(registry_busy_receipt(
                    host,
                    resource_class,
                    domain,
                    now,
                    Some(waited_ms),
                    deadline_at_ms,
                ))
            }
            result => result,
        }?;
        if result.status == HostLeaseAcquireStatus::Acquired {
            self.signal_waiters();
        }
        Ok(result)
    }

    #[cfg(test)]
    pub(super) fn try_acquire_at(
        &self,
        request: HostLeaseRequest,
        now: i64,
        deadline_at_ms: Option<i64>,
        waited_ms: u64,
    ) -> Result<HostLeaseAcquireReceipt, HostLeaseError> {
        let request = normalize_request(request)?;
        let mut conn = self.connection(SQLITE_MUTATION_BUSY_TIMEOUT)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let identity = WaiterIdentity {
            waiter_id: Uuid::now_v7().to_string(),
            requested_at_ms: now,
            recoverable: false,
        };
        let receipt =
            self.acquire_in_transaction(tx, request, now, deadline_at_ms, waited_ms, &identity)?;
        if receipt.status == HostLeaseAcquireStatus::Deferred {
            self.remove_waiter(&identity.waiter_id)?;
        }
        Ok(receipt)
    }

    fn acquire_in_transaction(
        &self,
        tx: Transaction<'_>,
        request: HostLeaseRequest,
        now: i64,
        deadline_at_ms: Option<i64>,
        waited_ms: u64,
        identity: &WaiterIdentity,
    ) -> Result<HostLeaseAcquireReceipt, HostLeaseError> {
        let owner_process_identity = request
            .owner_pid
            .map(|pid| match self.process_inspector.observe(pid) {
                ProcessObservation::Alive { identity } => Ok(identity),
                ProcessObservation::Dead => Err(HostLeaseError::InvalidRequest(
                    "owner_pid is not a live local process".to_string(),
                )),
                ProcessObservation::Unknown => Err(HostLeaseError::InvalidRequest(
                    "owner_pid liveness could not be verified".to_string(),
                )),
            })
            .transpose()?;
        let resource = HostLeaseResourceKey {
            machine: request.host.clone(),
            resource_class: request.resource_class,
            domain: request.domain.clone(),
        };
        cleanup_waiters(&tx, now, self.process_inspector.as_ref())?;
        upsert_waiter(
            &tx,
            &resource,
            request.priority_class,
            identity,
            deadline_at_ms.unwrap_or(i64::MAX),
            request.owner_pid,
            owner_process_identity,
        )?;
        let (queue, predecessor_deadline_at_ms) =
            queue_evidence(&tx, &resource, &identity.waiter_id)?;
        let (active, recovered) = active_handle(
            &tx,
            &request.host,
            request.resource_class,
            &request.domain,
            now,
            self.process_inspector.as_ref(),
        )?;
        if let Some(active) = active {
            let defer = HostLeaseDeferReceipt {
                host: request.host,
                resource_class: request.resource_class,
                domain: request.domain,
                deferred_reason: HostLeaseDeferReason::Contended,
                observed_at_ms: now,
                next_wake_at_ms: Some(next_lease_wake_at(&active, now, deadline_at_ms)),
                deadline_at_ms,
                active: Some(active),
            };
            tx.commit()?;
            return Ok(deferred_receipt(now, waited_ms, defer, recovered, queue));
        }

        if queue.position > 1 {
            let next_wake_at_ms = predecessor_deadline_at_ms
                .into_iter()
                .chain(deadline_at_ms)
                .min();
            let defer = HostLeaseDeferReceipt {
                host: request.host,
                resource_class: request.resource_class,
                domain: request.domain,
                deferred_reason: HostLeaseDeferReason::Queued,
                observed_at_ms: now,
                next_wake_at_ms,
                deadline_at_ms,
                active: None,
            };
            tx.commit()?;
            return Ok(deferred_receipt(now, waited_ms, defer, recovered, queue));
        }

        let handle = HostLeaseHandle {
            schema_version: SCHEMA_VERSION,
            host: request.host,
            resource_class: request.resource_class,
            domain: request.domain,
            execution_context: request.execution_context,
            lease_id: Uuid::now_v7().to_string(),
            owner: request.owner,
            priority_class: request.priority_class,
            acquired_at_ms: now,
            updated_at_ms: now,
            expires_at_ms: request
                .ttl_ms
                .map(|ttl| now.saturating_add(u64_ms_i64(ttl))),
            owner_pid: request.owner_pid,
            owner_process_identity,
            reason: request.reason,
            metadata: request.metadata,
        };
        write_handle(&tx, &handle)?;
        tx.execute(
            "DELETE FROM host_lease_waiters WHERE waiter_id = ?1",
            params![identity.waiter_id],
        )?;
        tx.commit()?;
        Ok(HostLeaseAcquireReceipt {
            schema_version: SCHEMA_VERSION,
            status: HostLeaseAcquireStatus::Acquired,
            observed_at_ms: now,
            waited_ms,
            handle: Some(handle),
            defer: None,
            recovered_stale_lease: recovered.is_some(),
            recovered,
            queue: Some(queue),
        })
    }
}

fn deferred_receipt(
    now: i64,
    waited_ms: u64,
    defer: HostLeaseDeferReceipt,
    recovered: Option<HostLeaseHandle>,
    queue: HostLeaseQueueEvidence,
) -> HostLeaseAcquireReceipt {
    HostLeaseAcquireReceipt {
        schema_version: SCHEMA_VERSION,
        status: HostLeaseAcquireStatus::Deferred,
        observed_at_ms: now,
        waited_ms,
        handle: None,
        defer: Some(defer),
        recovered_stale_lease: recovered.is_some(),
        recovered,
        queue: Some(queue),
    }
}

fn upsert_waiter(
    tx: &Transaction<'_>,
    resource: &HostLeaseResourceKey,
    priority_class: HostLeasePriorityClass,
    identity: &WaiterIdentity,
    deadline_at_ms: i64,
    owner_pid: Option<u32>,
    owner_process_identity: Option<u64>,
) -> Result<(), HostLeaseError> {
    let owner_process_identity = owner_process_identity
        .map(|value| {
            i64::try_from(value).map_err(|_| {
                HostLeaseError::InvalidRequest(
                    "waiter process identity is outside the SQLite integer range".to_string(),
                )
            })
        })
        .transpose()?;
    tx.execute(
        "INSERT OR IGNORE INTO host_lease_waiters (
            waiter_id, host, resource_class, domain, priority_class, priority_rank,
            requested_at_ms, deadline_at_ms, owner_pid, owner_process_identity, recoverable
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            identity.waiter_id,
            resource.machine,
            resource.resource_class.as_str(),
            resource.domain,
            priority_class.as_str(),
            priority_class.queue_rank(),
            identity.requested_at_ms,
            deadline_at_ms,
            owner_pid.map(i64::from),
            owner_process_identity,
            identity.recoverable,
        ],
    )?;
    let stored = tx.query_row(
        "SELECT host, resource_class, domain, priority_class, requested_at_ms, recoverable
         FROM host_lease_waiters WHERE waiter_id = ?1",
        params![identity.waiter_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, bool>(5)?,
            ))
        },
    )?;
    let expected = (
        resource.machine.as_str(),
        resource.resource_class.as_str(),
        resource.domain.as_str(),
        priority_class.as_str(),
        identity.requested_at_ms,
        identity.recoverable,
    );
    if (
        stored.0.as_str(),
        stored.1.as_str(),
        stored.2.as_str(),
        stored.3.as_str(),
        stored.4,
        stored.5,
    ) != expected
    {
        return Err(HostLeaseError::InvalidRequest(
            "waiter_id is already bound to a different lease request".to_string(),
        ));
    }
    tx.execute(
        "UPDATE host_lease_waiters
         SET owner_pid = ?2, owner_process_identity = ?3
         WHERE waiter_id = ?1",
        params![
            identity.waiter_id,
            owner_pid.map(i64::from),
            owner_process_identity,
        ],
    )?;
    Ok(())
}

fn cleanup_waiters(
    tx: &Transaction<'_>,
    now: i64,
    process_inspector: &dyn ProcessInspector,
) -> Result<(), HostLeaseError> {
    tx.execute(
        "DELETE FROM host_lease_waiters WHERE deadline_at_ms <= ?1",
        params![now],
    )?;
    let mut statement = tx.prepare(
        "SELECT waiter_id, owner_pid, owner_process_identity
         FROM host_lease_waiters
         WHERE recoverable = 0 AND owner_pid IS NOT NULL AND owner_process_identity IS NOT NULL",
    )?;
    let candidates = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (waiter_id, pid, identity) in candidates {
        let Ok(pid) = u32::try_from(pid) else {
            continue;
        };
        let Ok(identity) = u64::try_from(identity) else {
            continue;
        };
        let dead = match process_inspector.observe(pid) {
            ProcessObservation::Alive { identity: observed } => observed != identity,
            ProcessObservation::Dead => true,
            ProcessObservation::Unknown => false,
        };
        if dead {
            tx.execute(
                "DELETE FROM host_lease_waiters WHERE waiter_id = ?1",
                params![waiter_id],
            )?;
        }
    }
    Ok(())
}

fn queue_evidence(
    tx: &Transaction<'_>,
    resource: &HostLeaseResourceKey,
    waiter_id: &str,
) -> Result<(HostLeaseQueueEvidence, Option<i64>), HostLeaseError> {
    let mut statement = tx.prepare(
        "SELECT waiter_id, requested_at_ms, deadline_at_ms
         FROM host_lease_waiters
         WHERE host = ?1 AND resource_class = ?2 AND domain = ?3
         ORDER BY priority_rank ASC, requested_at_ms ASC, waiter_id ASC",
    )?;
    let rows = statement
        .query_map(
            params![
                resource.machine,
                resource.resource_class.as_str(),
                resource.domain,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let index = rows
        .iter()
        .position(|row| row.0 == waiter_id)
        .ok_or_else(|| {
            HostLeaseError::InvalidRequest("waiter disappeared during admission".to_string())
        })?;
    let predecessor = index.checked_sub(1).and_then(|value| rows.get(value));
    Ok((
        HostLeaseQueueEvidence {
            waiter_id: waiter_id.to_string(),
            requested_at_ms: rows[index].1,
            position: u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1),
            predecessor_waiter_id: predecessor.map(|row| row.0.clone()),
        },
        predecessor.map(|row| row.2),
    ))
}
