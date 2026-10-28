//! One host-owned policy-file contract for CLI and embedded runtimes.
use std::path::{Path, PathBuf};

use super::{error, unavailable, DenialKind, MachineSpendPolicy, MachineSpendQuota};
use crate::value::{ErrorCategory, VmError};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    ledger_path: PathBuf,
    scope: String,
    #[serde(default)]
    stop_on_unknown_usage: bool,
    limits: MachineSpendPolicy,
}

/// A validated, immutable host selection that has not opened its ledger yet.
pub struct MachineSpendPolicyFile {
    inner: PolicyFile,
}

impl MachineSpendPolicyFile {
    /// Parse an explicitly selected host file without opening accounting storage.
    /// Hosts can check the ledger's isolation before any creation or migration.
    pub fn read(path: impl AsRef<Path>) -> Result<Self, VmError> {
        let source = std::fs::read_to_string(path).map_err(|cause| {
            unavailable(
                ErrorCategory::Environment,
                &format!(
                    "cannot read machine spend policy ({})",
                    harn_sqlite::failure_diagnostic("spend_policy_read", &cause)
                ),
            )
        })?;
        let mut policy: PolicyFile = toml::from_str(&source).map_err(|_| {
            error(
                DenialKind::InvalidBudget,
                "invalid machine spend policy (stage=spend_policy_parse)",
            )
        })?;
        MachineSpendQuota::validate_policy(&policy.ledger_path, &policy.scope, &policy.limits)?;
        // Freeze the same resolved identity the host's path checks inspect.
        // Keeping a workspace-controlled alias would let its replacement
        // redirect later SQLite connections after the protection check.
        policy.ledger_path = crate::process_sandbox::render_policy_root(
            policy.ledger_path.to_str().expect("TOML paths are Unicode"),
        );
        Ok(Self { inner: policy })
    }

    /// Host-only storage identity; never grants evaluated code write access.
    pub fn ledger_path(&self) -> &Path {
        &self.inner.ledger_path
    }

    /// Open the already parsed selection, retaining the immutable durable policy.
    /// The host must first exclude it from the evaluated program's write authority.
    pub fn open(self) -> Result<MachineSpendQuota, VmError> {
        MachineSpendQuota::open_with_usage_policy(
            self.inner.ledger_path,
            self.inner.scope,
            self.inner.limits,
            self.inner.stop_on_unknown_usage,
        )
    }
}

impl MachineSpendQuota {
    /// Open an explicitly selected, trusted host policy before provider startup.
    ///
    /// This uses the same durable scope and immutable policy checks as [`Self::open`].
    /// It never discovers configuration from the workspace or environment, changes
    /// permissions, or grants the evaluated program access to the policy or ledger.
    /// The host must keep both outside the program's writable authority.
    pub fn from_policy_file(path: impl AsRef<Path>) -> Result<Self, VmError> {
        MachineSpendPolicyFile::read(path)?.open()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    fn policy_source(ledger: &Path, limit: i64) -> String {
        format!(
            "ledger_path = {}\nscope = 'fixture'\n[limits]\nlifetime_limit_microusd = {limit}\n",
            toml::Value::String(ledger.to_string_lossy().into_owned())
        )
    }

    fn assert_unresolved_usage(error: VmError, unsettled: u64, unpriced: u64) {
        let VmError::Thrown(receipt) = error else {
            panic!("expected a structured unresolved-usage refusal");
        };
        let receipt = crate::value::vm_to_storage_json(&receipt).unwrap();
        assert_eq!(receipt["admission_reason"], "unresolved_usage");
        assert_eq!(
            receipt["message"],
            format!(
                "machine spend scope has {unsettled} unsettled and {unpriced} unpriced attempts"
            )
        );
    }

    #[test]
    fn host_policy_file_reopens_same_scope_without_releasing_unknown_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.toml");
        let ledger = dir.path().join("spend.sqlite");
        std::fs::write(&path, policy_source(&ledger, 1_000_000)).unwrap();
        let first = MachineSpendQuota::from_policy_file(&path).unwrap();
        drop(first.reserve(Decimal::new(6, 1)).unwrap());
        drop(first);
        let second = MachineSpendQuota::from_policy_file(&path).unwrap();
        let receipt = second.receipt().unwrap();
        assert_eq!(receipt.lifetime_reserved_microusd, 600_000);
        assert_eq!(receipt.lifetime_usage_unknown_attempts, 1);
        assert_eq!(receipt.lifetime_remaining_microusd, Some(400_000));
        assert_eq!(second.remaining_microusd().unwrap(), 400_000);
        assert!(second.reserve(Decimal::new(5, 1)).is_err());
        std::fs::write(&path, policy_source(&ledger, 2_000_000)).unwrap();
        assert!(MachineSpendQuota::from_policy_file(&path).is_err());
        assert_eq!(second.receipt().unwrap(), receipt);
    }

    #[test]
    fn strict_policy_stops_at_first_unknown_across_reopened_handles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.toml");
        let ledger = dir.path().join("spend.sqlite");
        let source = policy_source(&ledger, 1_000_000)
            .replace("[limits]", "stop_on_unknown_usage = true\n[limits]");
        std::fs::write(&path, &source).unwrap();
        let actor = MachineSpendQuota::from_policy_file(&path).unwrap();
        let judge = MachineSpendQuota::from_policy_file(&path).unwrap();
        assert_eq!(judge.remaining_microusd().unwrap(), 1_000_000);
        let first = actor.reserve(Decimal::new(1, 1)).unwrap();
        // A second process must not dispatch while the first is in flight.
        assert_unresolved_usage(judge.reserve(Decimal::new(1, 1)).unwrap_err(), 1, 0);
        assert_eq!(judge.remaining_microusd().unwrap(), 0);
        let in_flight = judge.receipt().unwrap();
        assert_eq!(in_flight.lifetime_unsettled_attempts, 1);
        assert_eq!(in_flight.lifetime_unpriced_attempts, 0);
        first
            .settle(Decimal::new(1, 1), Some(Decimal::ZERO))
            .unwrap();
        let measured_zero = judge.receipt().unwrap();
        assert_eq!(measured_zero.lifetime_usage_unknown_attempts, 0);
        assert_eq!(measured_zero.lifetime_unsettled_attempts, 0);
        assert_eq!(measured_zero.lifetime_unpriced_attempts, 0);
        // The usage is measured, but the conservative upper bound remains.
        assert_eq!(judge.remaining_microusd().unwrap(), 900_000);
        let second = judge.reserve(Decimal::new(1, 1)).unwrap();
        second.settle(Decimal::new(1, 1), None).unwrap();
        drop(actor);
        drop(judge);
        // A version 3 runtime must not open the ledger and bypass this policy.
        let old_schema =
            harn_sqlite::schema::RuntimeSqliteSchema::new("machine_spend_quota", 3, "");
        let old_migration_ran = std::cell::Cell::new(false);
        let old_open = harn_sqlite::schema::initialize_runtime_sqlite_migrating(
            &rusqlite::Connection::open(&ledger).unwrap(),
            std::time::Duration::from_secs(5),
            &old_schema,
            |_| {
                old_migration_ran.set(true);
                Ok(())
            },
        );
        assert!(matches!(
            old_open,
            Err(harn_sqlite::InitializationError::NewerSchemaVersion {
                name: "machine_spend_quota",
                stored: 4,
                supported: 3,
            })
        ));
        assert!(!old_migration_ran.get());
        let restarted = MachineSpendQuota::from_policy_file(&path).unwrap();
        assert_unresolved_usage(restarted.reserve(Decimal::new(1, 1)).unwrap_err(), 0, 1);
        assert_eq!(restarted.remaining_microusd().unwrap(), 0);
        assert_eq!(
            restarted.receipt().unwrap().lifetime_usage_unknown_attempts,
            1
        );
        assert_eq!(restarted.receipt().unwrap().lifetime_unsettled_attempts, 0);
        assert_eq!(restarted.receipt().unwrap().lifetime_unpriced_attempts, 1);
        // A different host policy cannot silently weaken the same scope.
        std::fs::write(&path, policy_source(&ledger, 1_000_000)).unwrap();
        assert!(MachineSpendQuota::from_policy_file(&path).is_err());
    }

    #[test]
    fn strict_policy_keeps_an_interrupted_reservation_closed_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.toml");
        let ledger = dir.path().join("spend.sqlite");
        let source = policy_source(&ledger, 1_000_000)
            .replace("[limits]", "stop_on_unknown_usage = true\n[limits]");
        std::fs::write(&path, source).unwrap();
        let quota = MachineSpendQuota::from_policy_file(&path).unwrap();
        drop(quota.reserve(Decimal::new(1, 1)).unwrap());
        drop(quota);
        let restarted = MachineSpendQuota::from_policy_file(&path).unwrap();
        assert_unresolved_usage(restarted.reserve(Decimal::new(1, 1)).unwrap_err(), 1, 0);
        assert_eq!(
            restarted.receipt().unwrap().lifetime_reserved_microusd,
            100_000
        );
        assert_eq!(restarted.receipt().unwrap().lifetime_unsettled_attempts, 1);
        assert_eq!(restarted.receipt().unwrap().lifetime_unpriced_attempts, 0);
    }

    #[test]
    fn strict_policy_atomically_allows_only_one_concurrent_reservation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.toml");
        let ledger = dir.path().join("spend.sqlite");
        let source = policy_source(&ledger, 1_000_000)
            .replace("[limits]", "stop_on_unknown_usage = true\n[limits]");
        std::fs::write(&path, source).unwrap();
        let first = MachineSpendQuota::from_policy_file(&path).unwrap();
        let second = MachineSpendQuota::from_policy_file(&path).unwrap();
        let finish = std::sync::Arc::new(std::sync::Barrier::new(3));
        let (sent, received) = std::sync::mpsc::channel();
        crate::runtime_stack::scope(|threads| {
            for quota in [first, second] {
                let finish = finish.clone();
                let sent = sent.clone();
                threads.spawn(move || {
                    let reservation = quota.reserve(Decimal::new(1, 1));
                    sent.send(reservation.is_ok()).unwrap();
                    finish.wait();
                    drop(reservation);
                });
            }
            let admitted =
                usize::from(received.recv().unwrap()) + usize::from(received.recv().unwrap());
            finish.wait();
            assert_eq!(admitted, 1);
        });
    }

    #[test]
    fn host_policy_file_rejects_invalid_configuration_before_creating_a_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.toml");
        let ledger = dir.path().join("spend.sqlite");
        let source = policy_source(&ledger, 1_000_000);
        for invalid in [
            source.replace("scope = 'fixture'", "scope = ''"),
            source.replace("[limits]", "unknown = 'fixture-private-value'\n[limits]"),
            format!("{source}unknown = 'fixture-private-value'\n"),
            policy_source(Path::new("relative.sqlite"), 1_000_000),
            policy_source(&ledger, -1),
            source.replace("lifetime_limit_microusd = 1000000", ""),
            "ledger_path = 'fixture-private-value'\nmalformed = [".into(),
        ] {
            std::fs::write(&path, invalid).unwrap();
            let failure = MachineSpendQuota::from_policy_file(&path).unwrap_err();
            assert!(!failure.to_string().contains("fixture-private-value"));
            assert!(!ledger.exists());
        }
    }

    #[test]
    fn host_policy_file_read_failure_reports_typed_cause_without_source_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture-private-path.toml");
        let failure = MachineSpendQuota::from_policy_file(&path)
            .unwrap_err()
            .to_string();
        assert!(failure.contains("spend_policy_read"));
        assert!(failure.contains("NotFound"));
        assert!(!failure.contains("fixture-private-path"));
    }

    #[test]
    fn host_policy_file_never_returns_an_unmetered_fallback_when_storage_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.toml");
        // An existing directory cannot be opened as a SQLite database.
        std::fs::write(&path, policy_source(dir.path(), 1_000_000)).unwrap();
        let failure = MachineSpendQuota::from_policy_file(&path).unwrap_err();
        let VmError::Thrown(value) = &failure else {
            panic!("missing typed storage failure")
        };
        let receipt = crate::value::vm_to_storage_json(value).unwrap();
        assert_eq!(receipt["category"], "environment");
        let failure = failure.to_string();
        assert!(failure.contains("sqlite_primary_code=14"));
        assert!(!failure.contains(&dir.path().to_string_lossy().to_string()));
    }

    #[test]
    fn host_policy_file_can_be_checked_without_creating_storage_or_re_reading_selection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.toml");
        let selected = dir.path().join("selected.sqlite");
        let changed = dir.path().join("changed.sqlite");
        std::fs::write(&path, policy_source(&selected, 1_000_000)).unwrap();
        let parsed = MachineSpendPolicyFile::read(&path).unwrap();
        assert_eq!(
            parsed.ledger_path(),
            crate::process_sandbox::render_policy_root(selected.to_str().unwrap())
        );
        assert!(!selected.exists());
        std::fs::write(&path, policy_source(&changed, 2_000_000)).unwrap();
        let quota = parsed.open().unwrap();
        assert!(selected.exists());
        assert!(!changed.exists());
        assert_eq!(
            quota.receipt().unwrap().lifetime_remaining_microusd,
            Some(1_000_000)
        );
    }

    #[cfg(unix)]
    #[test]
    fn host_policy_file_freezes_storage_before_a_writable_alias_is_retargeted() {
        let dir = tempfile::tempdir().unwrap();
        let selected = dir.path().join("selected");
        let replacement = dir.path().join("replacement");
        std::fs::create_dir_all(&selected).unwrap();
        std::fs::create_dir_all(&replacement).unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&selected, &alias).unwrap();
        let path = dir.path().join("policy.toml");
        std::fs::write(&path, policy_source(&alias.join("spend.sqlite"), 1_000_000)).unwrap();
        let parsed = MachineSpendPolicyFile::read(&path).unwrap();
        assert!(!selected.join("spend.sqlite").exists());
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&replacement, &alias).unwrap();
        let quota = parsed.open().unwrap();
        assert!(selected.join("spend.sqlite").exists());
        assert!(!replacement.join("spend.sqlite").exists());
        assert_eq!(
            quota.receipt().unwrap().lifetime_remaining_microusd,
            Some(1_000_000)
        );
    }
}
