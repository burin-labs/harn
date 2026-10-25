//! Build-time projection of pinned Models.dev facts into Harn's catalog.
//! Request dialects, tool behavior and selection policy remain Harn-owned.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(test)]
mod tests;

pub const SOURCE_URL: &str = "https://models.dev/experimental/v2.0/api.json";
pub const SNAPSHOT_SCHEMA: &str = "harn.models-dev.v1";

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum Field {
    Name,
    ContextWindow,
    OpenWeight,
    Released,
    Pricing,
}

impl Field {
    fn target(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::ContextWindow => "context_window",
            Self::OpenWeight => "open_weight",
            Self::Released => "released",
            Self::Pricing => "pricing",
        }
    }

    fn source(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::ContextWindow => "limit",
            Self::OpenWeight => "open_weights",
            Self::Released => "release_date",
            Self::Pricing => "cost",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    provider: String,
    model: String,
    fields: Vec<Field>,
}

/// A checked-in projection of one upstream response. The source digest identifies
/// that full response; Git pins this smaller, reviewable cache independently.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    schema: String,
    source_url: String,
    upstream_sha256: String,
    models: BTreeMap<String, BTreeMap<String, Value>>,
}

impl Snapshot {
    pub fn parse(body: &str) -> Result<Self, String> {
        let snapshot: Self = serde_json::from_str(body)
            .map_err(|error| format!("invalid Models.dev snapshot: {error}"))?;
        if snapshot.schema != SNAPSHOT_SCHEMA || snapshot.source_url != SOURCE_URL {
            return Err("unsupported Models.dev snapshot schema or source URL".to_string());
        }
        if snapshot.upstream_sha256.len() != 64
            || !snapshot
                .upstream_sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        {
            return Err("Models.dev upstream digest must be a SHA-256 hex string".to_string());
        }
        Ok(snapshot)
    }

    /// Capture only facts explicitly owned by upstream, and validate every
    /// selected route before the caller replaces its last known good snapshot.
    pub fn capture(upstream: &Value, config: &toml::Value, digest: String) -> Result<Self, String> {
        let bindings = bindings(config)?;
        if bindings.is_empty() {
            return Err("no Models.dev source bindings were declared".to_string());
        }
        let mut models: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
        for (id, binding) in &bindings {
            let model = upstream
                .get(&binding.provider)
                .and_then(|provider| provider.get("models"))
                .and_then(|models| models.get(&binding.model))
                .ok_or_else(|| missing_route(id, binding))?;
            let selected = models
                .entry(binding.provider.clone())
                .or_default()
                .entry(binding.model.clone())
                .or_insert_with(|| Value::Object(Default::default()));
            for field in &binding.fields {
                let raw = model
                    .get(field.source())
                    .ok_or_else(|| format!("Models.dev route {id} has no {}", field.source()))?;
                selected[field.source()] = raw.clone();
            }
        }
        let snapshot = Self {
            schema: SNAPSHOT_SCHEMA.to_string(),
            source_url: SOURCE_URL.to_string(),
            upstream_sha256: digest,
            models,
        };
        snapshot.apply(&mut config.clone())?;
        Ok(snapshot)
    }

    /// Resolve source directives once, before normal catalog parsing. A fact
    /// has exactly one owner: an imported field cannot also have a local value.
    pub fn apply(&self, config: &mut toml::Value) -> Result<usize, String> {
        let mut resolved = Vec::new();
        for (id, binding) in bindings(config)? {
            let source = self
                .models
                .get(&binding.provider)
                .and_then(|models| models.get(&binding.model))
                .ok_or_else(|| missing_route(&id, &binding))?;
            for field in binding.fields {
                if config["models"][&id].get(field.target()).is_some() {
                    return Err(format!(
                        "model {id} declares {} both locally and in models_dev",
                        field.target()
                    ));
                }
                let value = normalize(field, source).map_err(|error| {
                    format!(
                        "Models.dev {}/{} {}: {error}",
                        binding.provider,
                        binding.model,
                        field.target()
                    )
                })?;
                resolved.push((id.clone(), field.target(), value));
            }
        }
        let count = resolved.len();
        for (id, field, value) in resolved {
            let model = config["models"][&id]
                .as_table_mut()
                .expect("binding has model table");
            model.insert(field.to_string(), value);
            model.remove("models_dev");
        }
        Ok(count)
    }
}

pub fn has_bindings(config: &toml::Value) -> Result<bool, String> {
    Ok(!bindings(config)?.is_empty())
}

fn bindings(config: &toml::Value) -> Result<Vec<(String, Binding)>, String> {
    let mut result = Vec::new();
    if let Some(models) = config.get("models").and_then(toml::Value::as_table) {
        for (id, model) in models {
            if let Some(raw) = model.get("models_dev") {
                let binding: Binding = raw
                    .clone()
                    .try_into()
                    .map_err(|error| format!("invalid models_dev binding for {id}: {error}"))?;
                if binding.provider.is_empty()
                    || binding.model.is_empty()
                    || binding.fields.is_empty()
                    || binding.fields.iter().collect::<BTreeSet<_>>().len() != binding.fields.len()
                {
                    return Err(format!("model {id} needs a nonempty, unique Models.dev field selection and exact route"));
                }
                result.push((id.clone(), binding));
            }
        }
    }
    Ok(result)
}

fn missing_route(id: &str, binding: &Binding) -> String {
    format!("Models.dev route {}/{} required by {id} is missing; keep the pinned snapshot until reviewed", binding.provider, binding.model)
}

fn normalize(field: Field, model: &Value) -> Result<toml::Value, String> {
    let value = match field {
        Field::Name => Value::String(
            model
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or("name must be nonempty")?
                .to_string(),
        ),
        Field::ContextWindow => {
            let context = model
                .pointer("/limit/context")
                .and_then(Value::as_u64)
                .filter(|v| *v > 0 && i64::try_from(*v).is_ok())
                .ok_or("context must be a positive integer")?;
            Value::from(context)
        }
        Field::OpenWeight => Value::Bool(
            model
                .get("open_weights")
                .and_then(Value::as_bool)
                .ok_or("open_weights must be boolean")?,
        ),
        Field::Released => {
            let date = model
                .get("release_date")
                .and_then(Value::as_str)
                .ok_or("release_date must be a date")?;
            chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .map_err(|_| "invalid release_date")?;
            Value::String(date.to_string())
        }
        Field::Pricing => {
            return toml::Value::try_from(simple_pricing(
                model.get("cost").ok_or("missing cost")?,
            )?)
            .map_err(|error| error.to_string());
        }
    };
    toml::Value::try_from(&value).map_err(|error| error.to_string())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cost {
    text: TextCost,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextCost {
    input: InputCost,
    output: OutputCost,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputCost {
    price: f64,
    cache: Option<CacheCost>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputCost {
    price: f64,
    reasoning: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheCost {
    read: Option<f64>,
    write: Option<WriteCost>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(untagged)]
enum WriteCost {
    Default(f64),
    ByTtl(WriteTtlCost),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteTtlCost {
    #[serde(rename = "5m")]
    five_minutes: Option<f64>,
    #[serde(rename = "1h")]
    one_hour: Option<f64>,
}

fn simple_pricing(raw: &Value) -> Result<crate::ModelPricing, String> {
    let cost: Cost = serde_json::from_value(raw.clone()).map_err(|error|
        format!("pricing needs an explicit Harn card; unsupported conditional/modality/fee semantics: {error}"))?;
    if cost
        .text
        .output
        .reasoning
        .is_some_and(|rate| rate != cost.text.output.price)
    {
        return Err("separate reasoning rates need an explicit Harn pricing card".to_string());
    }
    let cache = cost.text.input.cache;
    let read = cache.as_ref().and_then(|c| c.read);
    let write = cache.as_ref().and_then(|c| c.write.as_ref());
    let five_minutes = write.and_then(|w| match w {
        WriteCost::Default(rate) => Some(*rate),
        WriteCost::ByTtl(rates) => rates.five_minutes,
    });
    let one_hour = write.and_then(|w| match w {
        WriteCost::Default(_) => None,
        WriteCost::ByTtl(rates) => rates.one_hour,
    });
    for rate in [
        Some(cost.text.input.price),
        Some(cost.text.output.price),
        read,
        five_minutes,
        one_hour,
    ]
    .into_iter()
    .flatten()
    {
        if !rate.is_finite() || rate < 0.0 {
            return Err("prices must be finite and nonnegative".to_string());
        }
    }
    Ok(crate::ModelPricing {
        input_per_mtok: cost.text.input.price,
        output_per_mtok: cost.text.output.price,
        cache_read_per_mtok: read,
        cache_write_per_mtok: five_minutes,
        cache_write_1h_per_mtok: one_hour,
        input_token_bands: Vec::new(),
        promotions: Vec::new(),
        schedules: Vec::new(),
        hosted_tool_fees: BTreeMap::new(),
        modality_rates: None,
    })
}
