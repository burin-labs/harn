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

/// What a provider lets a caller decide on a per-request basis.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DataControlScope {
    /// The provider documents a request header or body field the caller sets
    /// on each call. Harn can act.
    PerRequest,
    /// The posture is selected on the account, organization, project, or
    /// contract. Harn cannot change it from a request, and must not pretend
    /// to.
    Account,
    /// The provider documents no control at either level. For a local
    /// endpoint this is the honest answer, not a gap.
    None,
}

/// Where a per-request control rides on the wire.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DataControlLocation {
    /// A JSON request-body field. `name` is a dotted path; intermediate
    /// objects are created if absent.
    Body,
    /// An HTTP request header. `name` is the header name.
    Header,
}

/// What the control governs. A provider may expose one, both, or neither.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DataControlEffect {
    /// Whether the provider stores the request/response server-side.
    Retention,
    /// Whether the provider may train on the request/response.
    Training,
}

/// What happens to the data when Harn sets nothing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RetentionDefault {
    /// Request/response is stored as application state by default.
    Retained,
    /// Nothing is stored beyond serving the response.
    NotRetained,
    /// Not stored as application state, but held for a bounded period for
    /// abuse monitoring or troubleshooting. This is the common hosted answer
    /// and is deliberately distinct from `not_retained`.
    AbuseMonitoringOnly,
    /// The provider publishes no answer. Not the same claim as
    /// `not_retained`.
    Unspecified,
}

/// Whether the provider trains on API data absent a contract saying otherwise.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TrainingDefault {
    /// The provider trains on API traffic unless the caller opts out.
    Trains,
    /// The provider commits not to train on API traffic.
    DoesNotTrain,
    /// The provider publishes no answer.
    Unspecified,
}

/// A per-model override of the provider's training posture.
///
/// Some providers do not sell one posture per account or per request. They
/// sell it per model id: the same weights are published twice, once at list
/// price with no training, and once heavily discounted in exchange for
/// permission to train on the traffic. Meta's `-contributor` routes are the
/// first of these in the catalog.
///
/// That fact cannot live on [`DataControlsDef`], because it varies *within*
/// one provider, and it cannot be expressed as a [`DataControlScope`] either:
/// there is no header or body field to set (`per_request` would force us to
/// invent an undocumented one), the posture is not chosen on the account
/// (`account`), and a control plainly does exist (`none` would hide it).
///
/// So it lives here, on the model row, and it overrides its provider. A row
/// that says nothing inherits the provider's declaration, which keeps the
/// common case, where a provider's posture is uniform, a single declaration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelDataControlsDef {
    /// Whether this specific route trains on API traffic. Overrides the
    /// provider's `training_default`.
    pub training_default: TrainingDefault,
    /// Overrides the provider's `retention_default` when the route differs.
    /// Usually absent: the discount is normally about training, not storage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_default: Option<RetentionDefault>,
    /// ISO date this row was last read against the provider's own docs.
    pub checked_on: String,
    /// The provider's published page for this route's data terms. At least one
    /// HTTPS source, same rule as the provider-level declaration: a privacy
    /// claim in this catalog always cites where it came from.
    pub sources: Vec<String>,
    /// One line a host may surface verbatim, e.g. the provider's own wording
    /// for what it does with the traffic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The wire dialect a control is documented for.
///
/// Some providers expose a retention field on one of their APIs and not
/// another — Gemini's `store` is an Interactions-API field, absent from
/// `generateContent`. A control that names dialects is applied only on those,
/// so Harn never invents an undocumented field on a sibling wire. An empty
/// `applies_to` means every dialect this provider serves.
///
/// `harn_vm::llm::api::data_controls` maps `StreamProtocol` onto this enum in
/// an exhaustive match, so a new dialect fails to compile until it is
/// classified here.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DataControlDialect {
    AnthropicSse,
    OpenAiSse,
    OllamaNdjson,
    GeminiJson,
    GeminiInteractionsSse,
}

/// One documented per-request control.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DataControlDef {
    pub location: DataControlLocation,
    /// Dotted JSON path (body) or header name (header).
    pub name: String,
    /// The value that means "do not retain / do not train", as the provider
    /// spells it. Booleans and strings both occur on real wires.
    pub value: DataControlValue,
    pub effect: DataControlEffect,
    /// Dialects this control is documented for. Empty means every dialect
    /// this provider serves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applies_to: Vec<DataControlDialect>,
    /// Free-form caveat carried onto the receipt's provider notes. Use it for
    /// facts a consumer must not lose, e.g. that a provider documents this
    /// field as insufficient on its own for guaranteed zero retention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caveat: Option<String>,
}

impl DataControlDef {
    pub fn applies_to_dialect(&self, dialect: DataControlDialect) -> bool {
        self.applies_to.is_empty() || self.applies_to.contains(&dialect)
    }
}

/// The value a control is set to. Kept typed so the catalog projection and
/// the wire agree on `false` vs `"false"`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum DataControlValue {
    Bool(bool),
    Text(String),
}

impl DataControlValue {
    pub fn as_json(&self) -> serde_json::Value {
        match self {
            Self::Bool(value) => serde_json::Value::Bool(*value),
            Self::Text(value) => serde_json::Value::String(value.clone()),
        }
    }

    /// Header-safe rendering. Headers carry text, so a bool becomes its
    /// lowercase spelling rather than a JSON literal with quotes.
    pub fn as_header_value(&self) -> String {
        match self {
            Self::Bool(value) => value.to_string(),
            Self::Text(value) => value.clone(),
        }
    }
}

/// A researched retention/training posture for one provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DataControlsDef {
    pub control_scope: DataControlScope,
    /// Controls Harn applies when the caller requests the strictest available
    /// posture. Empty for every scope but `per_request`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_controls: Vec<DataControlDef>,
    pub retention_default: RetentionDefault,
    pub training_default: TrainingDefault,
    /// ISO date the declaration was checked against provider documentation.
    pub checked_on: String,
    /// HTTPS provider-documentation URLs backing every field above.
    pub sources: Vec<String>,
    /// One-line note a consumer should surface with the row, e.g. that the
    /// strict posture is available but only by contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl DataControlsDef {
    /// Controls Harn may send on `dialect`.
    pub fn controls_for_dialect(
        &self,
        dialect: DataControlDialect,
    ) -> impl Iterator<Item = &DataControlDef> {
        self.request_controls
            .iter()
            .filter(move |control| control.applies_to_dialect(dialect))
    }

    pub fn offers_per_request_control(&self) -> bool {
        self.control_scope == DataControlScope::PerRequest && !self.request_controls.is_empty()
    }
}

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
