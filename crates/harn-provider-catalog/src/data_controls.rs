use serde::{Deserialize, Serialize};

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
