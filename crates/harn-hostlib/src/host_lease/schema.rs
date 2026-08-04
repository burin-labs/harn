use rusqlite::Transaction;

use super::HostLeaseError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LeaseTableLayout {
    Missing,
    LegacyWholeMachine,
    ResourceClassWithoutExecutionContext,
    CurrentWithoutDomain,
    Current,
}

pub(super) fn lease_table_layout(tx: &Transaction<'_>) -> Result<LeaseTableLayout, HostLeaseError> {
    let mut statement = tx.prepare("PRAGMA table_info(host_leases)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if columns.is_empty() {
        return Ok(LeaseTableLayout::Missing);
    }
    let has_resource_class = columns.iter().any(|column| column == "resource_class");
    let has_execution_context = columns
        .iter()
        .any(|column| column == "execution_context_json");
    let has_domain = columns.iter().any(|column| column == "domain");
    if has_resource_class && has_execution_context && has_domain {
        return Ok(LeaseTableLayout::Current);
    }
    if has_resource_class && has_execution_context {
        return Ok(LeaseTableLayout::CurrentWithoutDomain);
    }
    if has_resource_class {
        return Ok(LeaseTableLayout::ResourceClassWithoutExecutionContext);
    }
    Ok(LeaseTableLayout::LegacyWholeMachine)
}

pub(super) fn create_current_lease_table(tx: &Transaction<'_>) -> Result<(), HostLeaseError> {
    tx.execute_batch(
        "CREATE TABLE host_leases (
            host TEXT NOT NULL,
            resource_class TEXT NOT NULL,
            domain TEXT NOT NULL,
            lease_id TEXT NOT NULL,
            owner TEXT NOT NULL,
            priority_class TEXT NOT NULL,
            acquired_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            expires_at_ms INTEGER,
            owner_pid INTEGER,
            owner_process_identity INTEGER,
            reason TEXT,
            metadata_json TEXT NOT NULL,
            execution_context_json TEXT,
            PRIMARY KEY (host, resource_class, domain)
        );",
    )?;
    Ok(())
}

pub(super) fn migrate_legacy_lease_table(tx: &Transaction<'_>) -> Result<(), HostLeaseError> {
    tx.execute_batch("ALTER TABLE host_leases RENAME TO host_leases_v1;")?;
    create_current_lease_table(tx)?;
    tx.execute_batch(
        "INSERT INTO host_leases (
            host, resource_class, domain, lease_id, owner, priority_class, acquired_at_ms,
            updated_at_ms, expires_at_ms, owner_pid, owner_process_identity, reason, metadata_json,
            execution_context_json
         )
         SELECT host, 'whole-machine', 'default', lease_id, owner, priority_class, acquired_at_ms,
            updated_at_ms, expires_at_ms, owner_pid, owner_process_identity, reason, metadata_json,
            NULL
         FROM host_leases_v1;
         DROP TABLE host_leases_v1;",
    )?;
    Ok(())
}

pub(super) fn add_execution_context_column(tx: &Transaction<'_>) -> Result<(), HostLeaseError> {
    tx.execute_batch("ALTER TABLE host_leases ADD COLUMN execution_context_json TEXT;")?;
    Ok(())
}

pub(super) fn add_domain_key(tx: &Transaction<'_>) -> Result<(), HostLeaseError> {
    tx.execute_batch("ALTER TABLE host_leases RENAME TO host_leases_v2;")?;
    create_current_lease_table(tx)?;
    tx.execute_batch(
        "INSERT INTO host_leases (
            host, resource_class, domain, lease_id, owner, priority_class, acquired_at_ms,
            updated_at_ms, expires_at_ms, owner_pid, owner_process_identity, reason, metadata_json,
            execution_context_json
         )
         SELECT host, resource_class, 'default', lease_id, owner, priority_class, acquired_at_ms,
            updated_at_ms, expires_at_ms, owner_pid, owner_process_identity, reason, metadata_json,
            execution_context_json
         FROM host_leases_v2;
         DROP TABLE host_leases_v2;",
    )?;
    Ok(())
}
