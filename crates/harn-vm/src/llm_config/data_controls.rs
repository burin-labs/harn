//! Typed per-provider retention/training data-control declarations.
//!
//! Harn dials provider endpoints with the embedder's own credentials. Whether
//! the provider retains that request, and whether it trains on it, is a
//! provider fact with a provider-specific wire spelling: a body field on one,
//! a routing object on another, nothing at all on a third. Encoding that in
//! each embedding host means every host re-derives it, gets a subset right,
//! and drifts. It belongs beside `base_url` and `auth_style`, in the registry
//! that already owns provider quirks.
//!
//! Two rules make the registry readable as evidence rather than as vibes:
//!
//! 1. **Absence is impossible.** Every provider in the catalog either carries
//!    a researched [`DataControlsDef`] or is named in the audit registry's
//!    expiring `unverified` queue. A provider nobody has looked at is visible
//!    as unresearched instead of silently reading as "offers nothing".
//! 2. **The classification has a direction, and the direction is pinned.**
//!    A totality gate that only asks "is every provider classified?" passes
//!    green when a provider is classified backwards. The validator therefore
//!    also checks structural coherence per row — a `per_request` scope must
//!    name at least one control, and an `account`/`none` scope must name
//!    none — and `data_controls_tests.rs` pins the specific direction of the
//!    load-bearing rows against controls that fail when a row is flipped.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::ProvidersConfig;

pub use harn_provider_catalog::{
    DataControlDef, DataControlDialect, DataControlEffect, DataControlLocation, DataControlScope,
    DataControlValue, DataControlsDef, ModelDataControlsDef, RetentionDefault, TrainingDefault,
};

/// The registry's own default posture.
///
/// Making the strictest posture the default is a product decision for each
/// embedder, and making it silently inside the runtime would repeat the
/// mistake this registry exists to fix — a privacy-relevant default chosen by
/// omission. So the shipped value is `default`, and an embedder flips it in
/// one place, in config, without patching Harn.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DataPosture {
    /// Send no data-control fields. The provider's server-side default
    /// applies.
    #[default]
    Default,
    /// Apply every declared per-request control for the resolved provider.
    StrictestAvailable,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct DataControlsPolicy {
    #[serde(default)]
    pub default_posture: DataPosture,
}

/// Evidence and the explicit unresearched queue for [`DataControlsDef`].
///
/// Mirrors `usage_accounting_audit`: a finite, dated, tracked list of rows
/// nobody has verified, which expires. A provider is never merely missing.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DataControlsAuditRegistry {
    pub reviewed_on: String,
    pub expires_on: String,
    pub tracking_issue: u64,
    #[serde(default)]
    pub unverified: Vec<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct DataControlsAuditValidation {
    /// Providers the gate actually reached. A zero here means the gate
    /// measured nothing, which is not the same as measuring a clean catalog.
    pub provider_count: usize,
    /// Providers carrying a researched declaration.
    pub declared_count: usize,
    /// Providers still in the unresearched queue.
    pub unverified_count: usize,
    pub errors: Vec<String>,
}

impl DataControlsAuditValidation {
    pub fn is_clean(&self) -> bool {
        self.errors.is_empty()
    }
}

pub fn validate_data_controls_audit(
    config: &ProvidersConfig,
    as_of: &str,
) -> DataControlsAuditValidation {
    let providers: BTreeSet<&str> = config.providers.keys().map(String::as_str).collect();
    let mut result = DataControlsAuditValidation {
        provider_count: providers.len(),
        ..DataControlsAuditValidation::default()
    };
    if providers.is_empty() {
        result
            .errors
            .push("data-controls audit reached no providers".to_string());
        return result;
    }

    let Some(registry) = &config.data_controls_audit else {
        result
            .errors
            .push("data_controls_audit registry is missing".to_string());
        return result;
    };

    let as_of = parse_date("data-controls audit check date", as_of, &mut result);
    let reviewed_on = parse_date(
        "data_controls_audit.reviewed_on",
        &registry.reviewed_on,
        &mut result,
    );
    let expires_on = parse_date(
        "data_controls_audit.expires_on",
        &registry.expires_on,
        &mut result,
    );
    if registry.tracking_issue == 0 {
        result
            .errors
            .push("data_controls_audit.tracking_issue must be positive".to_string());
    }
    if let (Some(reviewed_on), Some(expires_on)) = (reviewed_on, expires_on) {
        if expires_on < reviewed_on {
            result.errors.push(format!(
                "data-controls audit expires_on {} precedes reviewed_on {}",
                registry.expires_on, registry.reviewed_on
            ));
        }
        if !registry.unverified.is_empty() && as_of.is_some_and(|as_of| expires_on < as_of) {
            result.errors.push(format!(
                "data-controls unverified queue expired on {} (tracking issue #{})",
                registry.expires_on, registry.tracking_issue
            ));
        }
    }

    let mut queued = BTreeSet::new();
    for provider in &registry.unverified {
        if !queued.insert(provider.as_str()) {
            result
                .errors
                .push(format!("duplicate data-controls audit row for {provider}"));
        }
        if !providers.contains(provider.as_str()) {
            result.errors.push(format!(
                "data-controls audit row {provider} does not name a catalog provider"
            ));
            continue;
        }
        if config
            .providers
            .get(provider)
            .is_some_and(|definition| definition.data_controls.is_some())
        {
            result.errors.push(format!(
                "provider {provider} declares data_controls but is queued as unverified"
            ));
        }
    }
    result.unverified_count = queued.len();

    for (id, provider) in &config.providers {
        let Some(controls) = &provider.data_controls else {
            if !queued.contains(id.as_str()) {
                result.errors.push(format!(
                    "provider {id} has neither a data_controls declaration nor an unverified audit entry"
                ));
            }
            continue;
        };
        result.declared_count += 1;
        validate_declaration(id, controls, &mut result);
    }

    result
}

fn validate_declaration(
    id: &str,
    controls: &DataControlsDef,
    result: &mut DataControlsAuditValidation,
) {
    parse_date(
        &format!("providers.{id}.data_controls.checked_on"),
        &controls.checked_on,
        result,
    );
    if controls.sources.is_empty()
        || controls
            .sources
            .iter()
            .any(|source| !source.starts_with("https://"))
    {
        result.errors.push(format!(
            "provider {id} data_controls must cite at least one HTTPS documentation source"
        ));
    }

    // Direction coherence. A row that claims a per-request control must name
    // one, and a row that claims none must not smuggle one in. Without this
    // the totality gate is satisfied by a row classified backwards.
    match controls.control_scope {
        DataControlScope::PerRequest => {
            if controls.request_controls.is_empty() {
                result.errors.push(format!(
                    "provider {id} declares control_scope per_request but names no request control"
                ));
            }
        }
        DataControlScope::Account | DataControlScope::None => {
            if !controls.request_controls.is_empty() {
                result.errors.push(format!(
                    "provider {id} names a request control but declares control_scope {:?}",
                    controls.control_scope
                ));
            }
        }
    }

    let mut seen = BTreeSet::new();
    for control in &controls.request_controls {
        if control.name.trim().is_empty() {
            result.errors.push(format!(
                "provider {id} data_controls names an empty control"
            ));
        }
        if !seen.insert((control.location, control.name.as_str())) {
            result.errors.push(format!(
                "provider {id} repeats data control {}",
                control.name
            ));
        }
    }
}

fn parse_date(
    field: &str,
    value: &str,
    result: &mut DataControlsAuditValidation,
) -> Option<NaiveDate> {
    match NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        Ok(date) => Some(date),
        Err(_) => {
            result
                .errors
                .push(format!("{field} must be an ISO date, got {value:?}"));
            None
        }
    }
}
