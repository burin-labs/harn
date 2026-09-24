//! Machine-scoped monetary allowance. Every attempted provider call reserves
//! its catalog upper bound in one SQLite transaction before transport. A lost
//! process leaves its reservation in place: uncertainty is not a free call.
use std::future::{poll_fn, Future};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use super::{error, DenialKind};
use crate::runtime_sqlite::{initialize_runtime_sqlite, RuntimeSqliteSchema};
use crate::value::VmError;

const SCALE: i64 = 1_000_000;
const SQLITE_SCHEMA: RuntimeSqliteSchema = RuntimeSqliteSchema::new(
    "machine_spend_quota",
    1,
    "CREATE TABLE IF NOT EXISTS spend_policy (scope TEXT PRIMARY KEY, daily_limit INTEGER, monthly_limit INTEGER, contract_broken INTEGER NOT NULL DEFAULT 0);
     CREATE TABLE IF NOT EXISTS spend_period (scope TEXT NOT NULL, period TEXT NOT NULL, reserved INTEGER NOT NULL CHECK(reserved >= 0), PRIMARY KEY(scope, period));
     CREATE TABLE IF NOT EXISTS spend_attempt (id TEXT PRIMARY KEY, scope TEXT NOT NULL, day TEXT NOT NULL, month TEXT NOT NULL, reserved INTEGER NOT NULL CHECK(reserved >= 0), actual INTEGER, settled INTEGER NOT NULL DEFAULT 0, created_ms INTEGER NOT NULL);
     CREATE TABLE IF NOT EXISTS spend_policy_audit (id INTEGER PRIMARY KEY AUTOINCREMENT, scope TEXT NOT NULL, at_ms INTEGER NOT NULL, approved_by TEXT NOT NULL, old_daily INTEGER, old_monthly INTEGER, new_daily INTEGER, new_monthly INTEGER);",
);

#[derive(Clone, Debug)]
pub struct MachineSpendQuota {
    inner: Arc<QuotaConfig>,
}

#[derive(Debug)]
struct QuotaConfig {
    path: PathBuf,
    scope: String,
    clock: Arc<dyn harn_clock::Clock>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct MachineSpendPolicy {
    pub daily_limit_microusd: Option<i64>,
    pub monthly_limit_microusd: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct MachineSpendReceipt {
    pub reserved_microusd: i64,
    /// Known provider usage only. Missing usage is counted separately.
    pub actual_known_microusd: i64,
    pub usage_unknown_attempts: u64,
    pub contract_broken: bool,
    pub daily_remaining_microusd: Option<i64>,
    pub monthly_remaining_microusd: Option<i64>,
    pub daily_reset_unix_ms: i64,
    pub monthly_reset_unix_ms: i64,
}

#[derive(Debug)]
pub(super) struct DurableReservation {
    config: Arc<QuotaConfig>,
    id: String,
}

impl MachineSpendQuota {
    /// The host supplies one private, stable machine/user database path and
    /// billing scope shared by all its sessions. Limits are integer micro-USD;
    /// no floating-point rounding occurs in admission.
    pub fn open(
        path: impl AsRef<Path>,
        scope: impl Into<String>,
        policy: MachineSpendPolicy,
    ) -> Result<Self, VmError> {
        Self::open_with_clock(path, scope, policy, harn_clock::RealClock::arc())
    }

    /// Supply the clock used for billing periods and audit timestamps.
    pub fn open_with_clock(
        path: impl AsRef<Path>,
        scope: impl Into<String>,
        policy: MachineSpendPolicy,
        clock: Arc<dyn harn_clock::Clock>,
    ) -> Result<Self, VmError> {
        let scope = scope.into();
        if !path.as_ref().is_absolute()
            || scope.trim().is_empty()
            || policy.daily_limit_microusd.is_none() && policy.monthly_limit_microusd.is_none()
            || policy.daily_limit_microusd.is_some_and(|limit| limit < 0)
            || policy.monthly_limit_microusd.is_some_and(|limit| limit < 0)
        {
            return Err(error(
                DenialKind::InvalidBudget,
                "machine spend quota requires an absolute path, a scope, and valid limits",
            ));
        }
        let config = Arc::new(QuotaConfig {
            path: path.as_ref().to_path_buf(),
            scope,
            clock,
        });
        let connection = connect(&config.path)?;
        connection
            .execute(
                "INSERT OR IGNORE INTO spend_policy(scope, daily_limit, monthly_limit) VALUES (?1, ?2, ?3)",
                params![config.scope, policy.daily_limit_microusd, policy.monthly_limit_microusd],
            )
            .map_err(db_error)?;
        let stored = read_policy(&connection, &config.scope)?;
        if stored != policy {
            return Err(error(
                DenialKind::InvalidBudget,
                "machine spend policy differs from the durable policy; use an authorized policy update",
            ));
        }
        Ok(Self { inner: config })
    }

    pub fn policy(&self) -> Result<MachineSpendPolicy, VmError> {
        read_policy(&connect(&self.inner.path)?, &self.inner.scope)
    }

    /// Install this machine allowance for a provider execution tree. A nested
    /// call inherits its parent's machine ledger and cannot replace it with a
    /// different path or billing scope.
    pub async fn scope<F: Future>(&self, inner: F) -> Result<F::Output, VmError> {
        self.validate_parent(true)?;
        let mut admission = super::SCOPE.with(|slot| slot.borrow().clone());
        let parent_ledger = admission.ledger.clone();
        admission.machine = Some(self.clone());
        let mut ambient = crate::orchestration::AmbientExecutionScope::capture_for_inline_subtask();
        ambient.set_llm_admission(admission);
        let mut inner = std::pin::pin!(crate::orchestration::scope_ambient(ambient, inner));
        poll_fn(|context| {
            if let Err(error) = self.validate_parent(false) {
                return Poll::Ready(Err(error));
            }
            let active = super::SCOPE.with(|slot| slot.borrow().clone());
            if (active.host_owned || crate::current_execution_scope().is_some())
                && !Arc::ptr_eq(&active.ledger, &parent_ledger)
            {
                return Poll::Ready(Err(error(
                    DenialKind::ScopeUnavailable,
                    "machine spend scope moved under an unrelated execution budget",
                )));
            }
            inner.as_mut().poll(context).map(Ok)
        })
        .await
    }

    fn validate_parent(&self, check_late_activation: bool) -> Result<(), VmError> {
        let active = super::SCOPE.with(|slot| slot.borrow().clone());
        if active
            .machine
            .as_ref()
            .is_some_and(|parent| !parent.same_scope(self))
        {
            return Err(error(
                DenialKind::ScopeUnavailable,
                "an unrelated machine allowance cannot replace the active budget",
            ));
        }
        if check_late_activation && active.machine.is_none() {
            let ledger = active
                .ledger
                .lock()
                .map_err(|_| error(DenialKind::ScopeUnavailable, "admission ledger poisoned"))?;
            if ledger.prior_unreserved_attempt || ledger.attempts_started > 0 {
                return Err(error(
                    DenialKind::LateActivation,
                    "machine spend quota must start before the first provider attempt",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn same_scope(&self, other: &Self) -> bool {
        self.inner.path == other.inner.path && self.inner.scope == other.inner.scope
    }

    /// An approved host-side budget action changes the durable ceiling and
    /// leaves an audit row. The host must authenticate `approved_by` before
    /// calling; a program in the VM cannot invoke this method.
    pub fn update_policy(
        &self,
        policy: MachineSpendPolicy,
        approved_by: &str,
    ) -> Result<(), VmError> {
        if approved_by.trim().is_empty()
            || policy.daily_limit_microusd.is_none() && policy.monthly_limit_microusd.is_none()
            || policy.daily_limit_microusd.is_some_and(|limit| limit < 0)
            || policy.monthly_limit_microusd.is_some_and(|limit| limit < 0)
        {
            return Err(error(
                DenialKind::InvalidBudget,
                "invalid approved spend policy update",
            ));
        }
        let mut connection = connect(&self.inner.path)?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let old = read_policy(&tx, &self.inner.scope)?;
        tx.execute(
            "UPDATE spend_policy SET daily_limit = ?2, monthly_limit = ?3 WHERE scope = ?1",
            params![
                self.inner.scope,
                policy.daily_limit_microusd,
                policy.monthly_limit_microusd
            ],
        )
        .map_err(db_error)?;
        tx.execute(
            "INSERT INTO spend_policy_audit(scope, at_ms, approved_by, old_daily, old_monthly, new_daily, new_monthly) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![self.inner.scope, harn_clock::now_wall_ms(self.inner.clock.as_ref()), approved_by, old.daily_limit_microusd, old.monthly_limit_microusd, policy.daily_limit_microusd, policy.monthly_limit_microusd],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)
    }

    pub(super) fn reserve(&self, amount: Decimal) -> Result<DurableReservation, VmError> {
        let amount = micros_ceil(amount)?;
        let now = self.inner.clock.now_utc();
        let (day, month, daily_reset, monthly_reset) = periods(now);
        let mut connection = connect(&self.inner.path)?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let policy = read_policy(&tx, &self.inner.scope)?;
        if contract_broken(&tx, &self.inner.scope)? {
            return Err(error(
                DenialKind::ProviderContractViolation,
                "machine spend scope has an unresolved provider contract violation",
            ));
        }
        for (period, limit, reset) in [
            (&day, policy.daily_limit_microusd, daily_reset),
            (&month, policy.monthly_limit_microusd, monthly_reset),
        ] {
            if let Some(limit) = limit {
                let used: i64 = tx
                    .query_row(
                        "SELECT reserved FROM spend_period WHERE scope = ?1 AND period = ?2",
                        params![self.inner.scope, period],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(db_error)?
                    .unwrap_or(0);
                if amount > limit.saturating_sub(used) {
                    return Err(exhausted(period, limit, used, reset));
                }
            }
        }
        let id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO spend_attempt(id, scope, day, month, reserved, created_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, self.inner.scope, day, month, amount, harn_clock::now_wall_ms(self.inner.clock.as_ref())],
        )
        .map_err(db_error)?;
        for period in [&day, &month] {
            tx.execute(
                "INSERT INTO spend_period(scope, period, reserved) VALUES (?1, ?2, ?3) ON CONFLICT(scope, period) DO UPDATE SET reserved = reserved + excluded.reserved",
                params![self.inner.scope, period, amount],
            )
            .map_err(db_error)?;
        }
        tx.commit().map_err(db_error)?;
        Ok(DurableReservation {
            config: self.inner.clone(),
            id,
        })
    }

    pub fn receipt(&self) -> Result<MachineSpendReceipt, VmError> {
        let now = self.inner.clock.now_utc();
        let (day, month, daily_reset_unix_ms, monthly_reset_unix_ms) = periods(now);
        let mut connection = connect(&self.inner.path)?;
        let snapshot = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(db_error)?;
        let policy = read_policy(&snapshot, &self.inner.scope)?;
        let contract_broken = contract_broken(&snapshot, &self.inner.scope)?;
        let used = |period: &str| -> Result<i64, VmError> {
            Ok(snapshot
                .query_row(
                    "SELECT reserved FROM spend_period WHERE scope = ?1 AND period = ?2",
                    params![self.inner.scope, period],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db_error)?
                .unwrap_or(0))
        };
        let day_used = used(&day)?;
        let month_used = used(&month)?;
        let (actual_known, usage_unknown): (i64, i64) = snapshot
            .query_row(
                "SELECT COALESCE(SUM(actual), 0), SUM(CASE WHEN actual IS NULL THEN 1 ELSE 0 END) FROM spend_attempt WHERE scope = ?1 AND month = ?2",
                params![self.inner.scope, month],
                |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
            )
            .map_err(db_error)?;
        snapshot.commit().map_err(db_error)?;
        Ok(MachineSpendReceipt {
            reserved_microusd: month_used,
            actual_known_microusd: actual_known,
            usage_unknown_attempts: usage_unknown.try_into().unwrap_or(u64::MAX),
            contract_broken,
            daily_remaining_microusd: policy
                .daily_limit_microusd
                .map(|limit| limit.saturating_sub(day_used).max(0)),
            monthly_remaining_microusd: policy
                .monthly_limit_microusd
                .map(|limit| limit.saturating_sub(month_used).max(0)),
            daily_reset_unix_ms,
            monthly_reset_unix_ms,
        })
    }
}

impl DurableReservation {
    pub(super) fn invalidate(&self) -> Result<(), VmError> {
        let connection = connect(&self.config.path)?;
        connection
            .execute(
                "UPDATE spend_policy SET contract_broken = 1 WHERE scope = ?1",
                [&self.config.scope],
            )
            .map_err(db_error)?;
        Ok(())
    }

    /// Release only the part the provider's complete usage proves unused.
    /// A failed settlement leaves the full durable hold in place.
    pub(super) fn settle(&self, upper: Decimal, actual: Option<Decimal>) -> Result<(), VmError> {
        let upper = micros_ceil(upper)?;
        let actual = actual.map(micros_ceil).transpose()?;
        let mut connection = connect(&self.config.path)?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let (day, month, reserved): (String, String, i64) = tx
            .query_row(
                "SELECT day, month, reserved FROM spend_attempt WHERE id = ?1 AND scope = ?2 AND settled = 0",
                params![self.id, self.config.scope],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(db_error)?;
        if upper > reserved || actual.is_some_and(|actual| actual > upper) {
            tx.execute(
                "UPDATE spend_policy SET contract_broken = 1 WHERE scope = ?1",
                [&self.config.scope],
            )
            .map_err(db_error)?;
            tx.commit().map_err(db_error)?;
            return Err(error(
                DenialKind::ProviderContractViolation,
                "provider usage exceeded the machine spend reservation",
            ));
        }
        tx.execute(
            "UPDATE spend_attempt SET reserved = ?2, actual = ?3, settled = 1 WHERE id = ?1",
            params![self.id, upper, actual],
        )
        .map_err(db_error)?;
        for period in [&day, &month] {
            tx.execute(
                "UPDATE spend_period SET reserved = reserved - ?3 WHERE scope = ?1 AND period = ?2",
                params![self.config.scope, period, reserved - upper],
            )
            .map_err(db_error)?;
        }
        tx.commit().map_err(db_error)
    }
}

fn connect(path: &Path) -> Result<Connection, VmError> {
    let parent = path.parent().ok_or_else(|| {
        error(
            DenialKind::InvalidBudget,
            "spend ledger needs a parent directory",
        )
    })?;
    std::fs::create_dir_all(parent).map_err(db_error)?;
    let connection = Connection::open(path).map_err(db_error)?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(db_error)?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(db_error)?;
    initialize_runtime_sqlite(&connection, Duration::from_secs(5), &SQLITE_SCHEMA)
        .map_err(db_error)?;
    Ok(connection)
}

fn read_policy(connection: &Connection, scope: &str) -> Result<MachineSpendPolicy, VmError> {
    connection
        .query_row(
            "SELECT daily_limit, monthly_limit FROM spend_policy WHERE scope = ?1",
            [scope],
            |row| {
                Ok(MachineSpendPolicy {
                    daily_limit_microusd: row.get(0)?,
                    monthly_limit_microusd: row.get(1)?,
                })
            },
        )
        .map_err(db_error)
}

fn contract_broken(connection: &Connection, scope: &str) -> Result<bool, VmError> {
    connection
        .query_row(
            "SELECT contract_broken FROM spend_policy WHERE scope = ?1",
            [scope],
            |row| row.get::<_, bool>(0),
        )
        .map_err(db_error)
}

fn micros_ceil(amount: Decimal) -> Result<i64, VmError> {
    if amount.is_sign_negative() {
        return Err(error(DenialKind::InvalidBudget, "negative spend amount"));
    }
    (amount * Decimal::from(SCALE))
        .ceil()
        .to_i64()
        .ok_or_else(|| {
            error(
                DenialKind::InvalidBudget,
                "spend amount exceeds ledger range",
            )
        })
}

fn periods(now: OffsetDateTime) -> (String, String, i64, i64) {
    let date = now.date();
    let next_day = date.next_day().expect("UTC date within supported range");
    let first_next_month = if date.month() == time::Month::December {
        Date::from_calendar_date(date.year() + 1, time::Month::January, 1)
    } else {
        Date::from_calendar_date(date.year(), date.month().next(), 1)
    }
    .expect("UTC month within supported range");
    (
        format!("D:{date}"),
        format!("M:{:04}-{:02}", date.year(), date.month() as u8),
        next_day.midnight().assume_utc().unix_timestamp() * 1000,
        first_next_month.midnight().assume_utc().unix_timestamp() * 1000,
    )
}

fn db_error(error_value: impl std::fmt::Display) -> VmError {
    error(
        DenialKind::ScopeUnavailable,
        &format!("machine spend ledger unavailable: {error_value}"),
    )
}

fn exhausted(period: &str, limit: i64, used: i64, reset_unix_ms: i64) -> VmError {
    let period = if period.starts_with("D:") {
        "day"
    } else {
        "month"
    };
    VmError::Thrown(crate::schema::json_to_vm_value(&serde_json::json!({
        "category": "budget_exceeded",
        "kind": "terminal",
        "reason": "budget_exceeded",
        "admission_reason": "insufficient_allowance",
        "quota_scope": "machine",
        "period": period,
        "limit_microusd": limit,
        "reserved_microusd": used,
        "remaining_microusd": limit.saturating_sub(used).max(0),
        "reset_at_unix_ms": reset_unix_ms,
        "message": "machine spend quota exhausted before provider transport"
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(limit: i64) -> MachineSpendPolicy {
        MachineSpendPolicy {
            daily_limit_microusd: Some(limit),
            monthly_limit_microusd: Some(limit),
        }
    }

    #[test]
    fn utc_periods_reset_at_day_month_and_year_boundaries() {
        let leap_day = Date::from_calendar_date(2028, time::Month::February, 29)
            .unwrap()
            .midnight()
            .assume_utc();
        let (day, month, daily_reset, monthly_reset) = periods(leap_day);
        assert_eq!(day, "D:2028-02-29");
        assert_eq!(month, "M:2028-02");
        assert_eq!(daily_reset, monthly_reset);
        assert_eq!(
            monthly_reset,
            Date::from_calendar_date(2028, time::Month::March, 1)
                .unwrap()
                .midnight()
                .assume_utc()
                .unix_timestamp()
                * 1000
        );
        let year_end = Date::from_calendar_date(2028, time::Month::December, 31)
            .unwrap()
            .midnight()
            .assume_utc();
        let (_, month, daily_reset, monthly_reset) = periods(year_end);
        assert_eq!(month, "M:2028-12");
        assert_eq!(daily_reset, monthly_reset);
        assert_eq!(
            monthly_reset,
            Date::from_calendar_date(2029, time::Month::January, 1)
                .unwrap()
                .midnight()
                .assume_utc()
                .unix_timestamp()
                * 1000
        );
    }

    #[test]
    fn fractional_microdollars_round_up_before_admission() {
        assert_eq!(micros_ceil(Decimal::new(1, 7)).unwrap(), 1);
        assert_eq!(micros_ceil(Decimal::new(1, 6)).unwrap(), 1);
        assert_eq!(micros_ceil(Decimal::new(11, 7)).unwrap(), 2);
    }

    #[test]
    fn crashed_attempt_keeps_both_period_reservations_after_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("spend.sqlite");
        let first = MachineSpendQuota::open(&path, "person", policy(1_000_000)).unwrap();
        drop(first.reserve(Decimal::new(6, 1)).unwrap());
        drop(first);
        let restarted = MachineSpendQuota::open(&path, "person", policy(1_000_000)).unwrap();
        assert!(restarted.reserve(Decimal::new(5, 1)).is_err());
        let receipt = restarted.receipt().unwrap();
        assert_eq!(receipt.reserved_microusd, 600_000);
        assert_eq!(receipt.actual_known_microusd, 0);
        assert_eq!(receipt.usage_unknown_attempts, 1);
        assert_eq!(receipt.daily_remaining_microusd, Some(400_000));
        assert_eq!(receipt.monthly_remaining_microusd, Some(400_000));
    }

    #[test]
    fn concurrent_connections_cannot_spend_the_same_remainder() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("spend.sqlite");
        let quota = MachineSpendQuota::open(&path, "person", policy(1_000_000)).unwrap();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let quota = MachineSpendQuota::open(path, "person", policy(1_000_000)).unwrap();
                    quota.reserve(Decimal::new(3, 1)).is_ok()
                })
            })
            .collect();
        let admitted = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(|admitted| *admitted)
            .count();
        assert_eq!(admitted, 3);
        assert_eq!(quota.receipt().unwrap().reserved_microusd, 900_000);
    }

    #[test]
    fn separate_processes_share_one_machine_allowance() {
        if let Some(path) = std::env::var_os("HARN_MACHINE_SPEND_TEST_CHILD_PATH") {
            let quota = MachineSpendQuota::open(path, "person", policy(1_000_000)).unwrap();
            let admitted = quota.reserve(Decimal::new(6, 1)).is_ok();
            println!("MACHINE_SPEND_ADMITTED={admitted}");
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("spend.sqlite");
        let binary = std::env::current_exe().unwrap();
        let children: Vec<_> = (0..2)
            .map(|_| {
                std::process::Command::new(&binary)
                    .arg("--exact")
                    .arg("llm::admission::durable::tests::separate_processes_share_one_machine_allowance")
                    .arg("--nocapture")
                    .env("HARN_MACHINE_SPEND_TEST_CHILD_PATH", &path)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        let admitted = children
            .into_iter()
            .map(|child| child.wait_with_output().unwrap())
            .filter(|child| {
                assert!(
                    child.status.success(),
                    "{}",
                    String::from_utf8_lossy(&child.stderr)
                );
                String::from_utf8_lossy(&child.stdout).contains("MACHINE_SPEND_ADMITTED=true")
            })
            .count();
        assert_eq!(admitted, 1);
        assert_eq!(
            MachineSpendQuota::open(path, "person", policy(1_000_000))
                .unwrap()
                .receipt()
                .unwrap()
                .reserved_microusd,
            600_000
        );
    }

    #[test]
    fn complete_usage_releases_only_proven_unused_amount() {
        let temp = tempfile::tempdir().unwrap();
        let quota = MachineSpendQuota::open(
            temp.path().join("spend.sqlite"),
            "person",
            policy(1_000_000),
        )
        .unwrap();
        let first = quota.reserve(Decimal::new(6, 1)).unwrap();
        first
            .settle(Decimal::new(2, 1), Some(Decimal::new(15, 2)))
            .unwrap();
        assert!(quota.reserve(Decimal::new(7, 1)).is_ok());
        let receipt = quota.receipt().unwrap();
        assert_eq!(receipt.reserved_microusd, 900_000);
        assert_eq!(receipt.actual_known_microusd, 150_000);
        assert_eq!(receipt.usage_unknown_attempts, 1);
    }

    #[test]
    fn provider_contract_violation_denies_future_processes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("spend.sqlite");
        let quota = MachineSpendQuota::open(&path, "person", policy(1_000_000)).unwrap();
        let reservation = quota.reserve(Decimal::new(6, 1)).unwrap();
        assert!(reservation.settle(Decimal::new(7, 1), None).is_err());
        assert!(quota.receipt().unwrap().contract_broken);
        drop(quota);
        let restarted = MachineSpendQuota::open(path, "person", policy(1_000_000)).unwrap();
        assert!(restarted.receipt().unwrap().contract_broken);
        assert!(restarted.reserve(Decimal::new(1, 1)).is_err());
    }

    #[test]
    fn policy_raise_requires_named_approval_and_is_durably_audited() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("spend.sqlite");
        let quota = MachineSpendQuota::open(&path, "person", policy(1_000_000)).unwrap();
        assert!(quota.update_policy(policy(2_000_000), "").is_err());
        quota
            .update_policy(policy(2_000_000), "host-approval-1")
            .unwrap();
        assert_eq!(quota.policy().unwrap(), policy(2_000_000));
        let connection = connect(&path).unwrap();
        let audit_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM spend_policy_audit", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(audit_count, 1);
    }

    #[test]
    fn utc_day_and_month_reset_are_explicit() {
        let now = Date::from_calendar_date(2026, time::Month::September, 30)
            .unwrap()
            .with_hms(23, 59, 0)
            .unwrap()
            .assume_utc();
        let (day, month, day_reset, month_reset) = periods(now);
        assert_eq!(day, "D:2026-09-30");
        assert_eq!(month, "M:2026-09");
        assert_eq!(day_reset, month_reset);
        assert_eq!(
            day_reset,
            Date::from_calendar_date(2026, time::Month::October, 1)
                .unwrap()
                .midnight()
                .assume_utc()
                .unix_timestamp()
                * 1000
        );
    }
}
