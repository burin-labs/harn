//! Transactional status for active ownership and admission queues.

use std::collections::BTreeSet;

use super::*;

const STATUS_SCHEMA_VERSION: u32 = 5;

/// A request still waiting in the scheduler's authoritative admission queue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLeasePendingRequest {
    /// Stable queue identity; supervised workloads reuse their run receipt id.
    pub waiter_id: String,
    /// Scheduling priority used by admission.
    pub priority_class: HostLeasePriorityClass,
    /// Original request time in Unix milliseconds.
    pub requested_at_ms: i64,
    /// Admission deadline in Unix milliseconds.
    pub deadline_at_ms: i64,
    /// Live wrapper process, if one has attached to this request.
    pub owner_pid: Option<u32>,
    /// Whether a supervised worker can resume this queue position.
    pub recoverable: bool,
}

/// Current authoritative state of one resource and coordination domain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLeaseState {
    /// Status contract schema version.
    pub schema_version: u32,
    /// Machine resource name.
    pub host: String,
    #[serde(default)]
    /// Resource class inspected on this host.
    pub resource_class: HostLeaseResourceClass,
    #[serde(default = "default_host_lease_domain")]
    /// Coordination domain inspected on this host.
    pub domain: String,
    /// Observation timestamp in Unix milliseconds.
    pub observed_at_ms: i64,
    #[serde(default)]
    /// Current lease owner. Absence alone does not imply an idle scheduler.
    pub active: Option<HostLeaseHandle>,
    /// Pending requests in the same priority/FIFO order used by admission.
    pub pending: Vec<HostLeasePendingRequest>,
    /// True when this read removed an expired or dead-owner lease.
    pub recovered_stale_lease: bool,
    #[serde(default)]
    /// Exact stale or dead-owner lease removed by this observation.
    pub recovered: Option<HostLeaseHandle>,
}

/// All resource states observed in one host-registry transaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLeaseOverview {
    /// Status contract schema version.
    pub schema_version: u32,
    /// Machine resource name.
    pub host: String,
    /// Shared observation timestamp in Unix milliseconds.
    pub observed_at_ms: i64,
    /// Every registered class in the default domain plus discovered domains.
    /// With a domain filter, every registered class in that domain is included.
    pub resources: Vec<HostLeaseState>,
}

impl HostLeaseStore {
    /// Inspect all resource classes, optionally restricted to one domain.
    pub fn status_overview(
        &self,
        host: &str,
        domain: Option<&str>,
    ) -> Result<HostLeaseOverview, HostLeaseError> {
        let host = normalize_component("host", host)?;
        let domain = domain.map(normalize_domain).transpose()?;
        let mut conn = self.connection(SQLITE_MUTATION_BUSY_TIMEOUT)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = unix_now_ms()?;
        let removed = admission::cleanup_waiters(&tx, now, self.process_inspector.as_ref())?;
        let keys = resource_keys(&tx, &host, domain.as_deref())?;
        let resources = keys
            .into_iter()
            .map(|resource| self.observe_resource(&tx, &resource, now))
            .collect::<Result<Vec<_>, _>>()?;
        let recovered = resources.iter().any(|state| state.recovered.is_some());
        tx.commit()?;
        if removed || recovered {
            self.signal_waiters();
        }
        Ok(HostLeaseOverview {
            schema_version: STATUS_SCHEMA_VERSION,
            host,
            observed_at_ms: now,
            resources,
        })
    }

    pub(super) fn status_in_transaction(
        &self,
        tx: Transaction<'_>,
        host: &str,
        resource_class: HostLeaseResourceClass,
        domain: &str,
        now: i64,
    ) -> Result<HostLeaseState, HostLeaseError> {
        let removed = admission::cleanup_waiters(&tx, now, self.process_inspector.as_ref())?;
        let resource = HostLeaseResourceKey {
            machine: host.to_string(),
            resource_class,
            domain: domain.to_string(),
        };
        let state = self.observe_resource(&tx, &resource, now)?;
        tx.commit()?;
        if removed || state.recovered.is_some() {
            self.signal_waiters();
        }
        Ok(state)
    }

    fn observe_resource(
        &self,
        tx: &Transaction<'_>,
        resource: &HostLeaseResourceKey,
        now: i64,
    ) -> Result<HostLeaseState, HostLeaseError> {
        let (active, recovered) = active_handle(
            tx,
            &resource.machine,
            resource.resource_class,
            &resource.domain,
            now,
            self.process_inspector.as_ref(),
        )?;
        Ok(HostLeaseState {
            schema_version: STATUS_SCHEMA_VERSION,
            host: resource.machine.clone(),
            resource_class: resource.resource_class,
            domain: resource.domain.clone(),
            observed_at_ms: now,
            active,
            pending: admission::pending_requests(tx, resource)?,
            recovered_stale_lease: recovered.is_some(),
            recovered,
        })
    }
}

fn resource_keys(
    tx: &Transaction<'_>,
    host: &str,
    domain: Option<&str>,
) -> Result<Vec<HostLeaseResourceKey>, HostLeaseError> {
    let default_domain = domain.unwrap_or(DEFAULT_HOST_LEASE_DOMAIN);
    let mut keys: BTreeSet<(String, String)> = HOST_LEASE_RESOURCE_DEFINITIONS
        .iter()
        .map(|definition| (definition.name.to_string(), default_domain.to_string()))
        .collect();
    let mut statement = tx.prepare(
        "SELECT resource_class, domain FROM host_leases WHERE host = ?1
         UNION SELECT resource_class, domain FROM host_lease_waiters WHERE host = ?1",
    )?;
    for row in statement.query_map(params![host], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (class, stored_domain) = row?;
        if domain.is_none_or(|domain| domain == stored_domain) {
            keys.insert((class, stored_domain));
        }
    }
    keys.into_iter()
        .map(|(class, domain)| {
            Ok(HostLeaseResourceKey {
                machine: host.to_string(),
                resource_class: HostLeaseResourceClass::parse(&class)?,
                domain,
            })
        })
        .collect()
}
