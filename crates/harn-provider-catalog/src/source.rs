//! Expand one source model registration into catalog and capability projections.

use std::collections::BTreeMap;

use serde::{de::DeserializeOwned, Deserialize};
use toml::{Table, Value};

use crate::{capabilities::ProviderRule, ModelFamilyValueDef};

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;

pub struct ModelRegistrationProjection {
    pub catalog_source: String,
    pub capability_source: String,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    request: Option<Table>,
    #[serde(default)]
    selection: Option<Selection>,
    #[serde(default)]
    routes: BTreeMap<String, Route>,
    #[serde(default)]
    supersedes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Route {
    provider: String,
    #[serde(default)]
    model: Table,
    #[serde(default)]
    request: Option<Table>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    supersedes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    family: String,
    dimension: String,
    #[serde(default)]
    before: Option<String>,
    value: ModelFamilyValueDef,
}

/// Source-only `registration` fields never enter runtime overlays or artifacts.
/// Route overrides replace whole model fields, including complete pricing cards.
pub fn expand_model_registrations(
    source: &str,
) -> Result<Option<ModelRegistrationProjection>, String> {
    let mut catalog: Value = toml::from_str(source).map_err(|error| error.to_string())?;
    let Some(models) = catalog.get_mut("models").and_then(Value::as_table_mut) else {
        return Ok(None);
    };
    let mut registrations = BTreeMap::new();
    for (id, model) in models.iter_mut() {
        if let Some(value) = model
            .as_table_mut()
            .and_then(|row| row.remove("registration"))
        {
            registrations.insert(id.clone(), decode::<Registration>(value, id)?);
        }
    }
    if registrations.is_empty() {
        return Ok(None);
    }
    let mut rules: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for (id, registration) in registrations {
        let base = catalog["models"][&id]
            .as_table()
            .ok_or_else(|| format!("model {id} must be a table"))?
            .clone();
        register_request(&id, &base, registration.request.clone(), &mut rules)?;
        register_aliases(&mut catalog, &id, &base, registration.aliases)?;
        if let Some(selection) = registration.selection {
            register_selection(&mut catalog, &id, selection)?;
        }
        register_supersedes(&mut catalog, &id, registration.supersedes)?;
        for (route_id, route) in registration.routes {
            if catalog["models"].get(&route_id).is_some() {
                return Err(format!(
                    "registered route {route_id} already has a model row"
                ));
            }
            if route.model.contains_key("provider") || route.model.contains_key("registration") {
                return Err(format!(
                    "route {route_id} must declare its provider and registration once"
                ));
            }
            let mut model = base.clone();
            model.extend(route.model);
            model.insert("provider".into(), Value::String(route.provider));
            let request = match (registration.request.as_ref(), route.request) {
                (Some(base), overrides) => {
                    let mut request = base.clone();
                    request.extend(overrides.unwrap_or_default());
                    Some(request)
                }
                (None, request) => request,
            };
            register_request(&route_id, &model, request, &mut rules)?;
            register_aliases(&mut catalog, &route_id, &model, route.aliases)?;
            register_supersedes(&mut catalog, &route_id, route.supersedes)?;
            catalog["models"]
                .as_table_mut()
                .expect("models table was read above")
                .insert(route_id, Value::Table(model));
        }
    }
    let providers: Table = rules
        .into_iter()
        .map(|(provider, rules)| (provider, Value::Array(rules)))
        .collect();
    let capabilities = Value::Table(Table::from_iter([(
        "provider".into(),
        Value::Table(providers),
    )]));
    Ok(Some(ModelRegistrationProjection {
        catalog_source: toml::to_string_pretty(&catalog).map_err(|error| error.to_string())?,
        capability_source: toml::to_string_pretty(&capabilities)
            .map_err(|error| error.to_string())?,
    }))
}

fn register_supersedes(catalog: &mut Value, id: &str, replaced: Vec<String>) -> Result<(), String> {
    for replaced in replaced {
        let row = catalog["models"]
            .get_mut(&replaced)
            .and_then(Value::as_table_mut)
            .ok_or_else(|| format!("{id} supersedes unknown model {replaced}"))?;
        if replaced == id || row.contains_key("superseded_by") {
            return Err(format!(
                "{id} must be the sole source of {replaced}'s replacement"
            ));
        }
        row.insert("superseded_by".into(), Value::String(id.into()));
    }
    Ok(())
}

fn decode<T: DeserializeOwned>(value: Value, id: &str) -> Result<T, String> {
    let body = toml::to_string(&value).map_err(|error| error.to_string())?;
    let deserializer = toml::Deserializer::parse(&body).map_err(|error| error.to_string())?;
    let mut ignored = Vec::new();
    let result = serde_ignored::deserialize(deserializer, |path| ignored.push(path.to_string()))
        .map_err(|error| format!("model {id} registration: {error}"))?;
    if !ignored.is_empty() {
        return Err(format!(
            "model {id} registration has unknown fields: {}",
            ignored.join(", ")
        ));
    }
    Ok(result)
}

fn provider<'a>(id: &str, model: &'a Table) -> Result<&'a str, String> {
    model
        .get("provider")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("model {id} registration requires a provider"))
}

fn register_request(
    id: &str,
    model: &Table,
    request: Option<Table>,
    rules: &mut BTreeMap<String, Vec<Value>>,
) -> Result<(), String> {
    let Some(mut request) = request else {
        return Ok(());
    };
    if request.contains_key("model_match") || request.contains_key("extends") {
        return Err(format!(
            "model {id} registration owns its exact match and fall-through"
        ));
    }
    let wire_model = model
        .get("wire_model")
        .and_then(Value::as_str)
        .unwrap_or(id);
    if [id, wire_model]
        .iter()
        .any(|name| name.contains(['*', '?', '[', ']']))
    {
        return Err(format!("registered request {id} must name an exact model"));
    }
    let patterns = if wire_model == id {
        Value::String(id.into())
    } else {
        Value::Array(vec![
            Value::String(id.into()),
            Value::String(wire_model.into()),
        ])
    };
    request.insert("model_match".into(), patterns);
    request.insert("extends".into(), Value::Boolean(true));
    let value = Value::Table(request);
    let _: ProviderRule = decode(value.clone(), id)?;
    rules
        .entry(provider(id, model)?.into())
        .or_default()
        .push(value);
    Ok(())
}

fn register_aliases(
    catalog: &mut Value,
    id: &str,
    model: &Table,
    aliases: Vec<String>,
) -> Result<(), String> {
    if aliases.is_empty() {
        return Ok(());
    }
    let provider = provider(id, model)?;
    let root = catalog.as_table_mut().expect("catalog is a table");
    let table = root
        .entry("aliases")
        .or_insert_with(|| Value::Table(Table::new()));
    let table = table.as_table_mut().ok_or("aliases must be a table")?;
    for alias in aliases {
        if alias.trim().is_empty() || alias.trim() != alias || table.contains_key(&alias) {
            return Err(format!(
                "model {id} has an empty, padded or duplicate alias {alias:?}"
            ));
        }
        table.insert(
            alias,
            Value::Table(Table::from_iter([
                ("id".into(), Value::String(id.into())),
                ("provider".into(), Value::String(provider.into())),
            ])),
        );
    }
    Ok(())
}

fn register_selection(
    catalog: &mut Value,
    id: &str,
    mut selection: Selection,
) -> Result<(), String> {
    if selection.value.model_id.is_some() {
        return Err(format!("model {id} registration owns its picker model id"));
    }
    let dimensions = catalog
        .get_mut("presentation")
        .and_then(|value| value.get_mut("families"))
        .and_then(|value| value.get_mut(&selection.family))
        .and_then(|value| value.get_mut("dimensions"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| {
            format!(
                "model {id} names unknown picker family {}",
                selection.family
            )
        })?;
    let values = dimensions
        .iter_mut()
        .find(|dimension| {
            dimension.get("key").and_then(Value::as_str) == Some(selection.dimension.as_str())
        })
        .filter(|dimension| dimension.get("kind").and_then(Value::as_str) == Some("model"))
        .and_then(|dimension| dimension.get_mut("ordered_values"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| format!("model {id} names an absent or non-model picker dimension"))?;
    if values.iter().any(|value| {
        value.get("value").and_then(Value::as_str) == Some(selection.value.value.as_str())
    }) {
        return Err(format!(
            "model {id} duplicates picker value {}",
            selection.value.value
        ));
    }
    let index = match selection.before {
        Some(before) => values
            .iter()
            .position(|value| value.get("value").and_then(Value::as_str) == Some(before.as_str()))
            .ok_or_else(|| format!("model {id} names unknown preceding picker value {before}"))?,
        None => values.len(),
    };
    selection.value.model_id = Some(id.into());
    let value = Value::try_from(selection.value).map_err(|error| error.to_string())?;
    values.insert(index, value);
    Ok(())
}
