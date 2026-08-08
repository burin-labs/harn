//! `harn --json-schemas` — enumerate the JSON contract every
//! subcommand exposes (or filter to one with `--command <name>`).
//!
//! Output is always wrapped in a [`JsonEnvelope`] so the catalog
//! itself conforms to the contract it advertises.

use std::process;

use crate::json_envelope::{
    catalog, to_string_pretty, JsonEnvelope, SchemaEntry, CATALOG_SCHEMA_VERSION,
};

pub(crate) fn run(filter: Option<&str>) {
    let envelope = build(filter);
    let ok = envelope.ok;
    println!("{}", to_string_pretty(&envelope));
    if !ok {
        process::exit(1);
    }
}

fn build(filter: Option<&str>) -> JsonEnvelope<Vec<SchemaEntry>> {
    let entries = catalog();
    match filter {
        Some(name) => {
            let matched: Vec<SchemaEntry> =
                entries.into_iter().filter(|e| e.command == name).collect();
            if matched.is_empty() {
                JsonEnvelope::err(
                    CATALOG_SCHEMA_VERSION,
                    "schema_not_found",
                    format!("no JSON schema registered for command '{name}'"),
                )
            } else {
                JsonEnvelope::ok(CATALOG_SCHEMA_VERSION, matched)
            }
        }
        None => JsonEnvelope::ok(CATALOG_SCHEMA_VERSION, entries),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::common::json_envelope::assert_envelope;

    #[test]
    fn unfiltered_catalog_returns_all_entries() {
        let envelope = build(None);
        let value = serde_json::to_value(&envelope).unwrap();
        let data = assert_envelope(&value, CATALOG_SCHEMA_VERSION);
        let arr = data.as_array().expect("data is an array");
        assert!(!arr.is_empty(), "catalog must ship with seed entries");
        assert!(arr.iter().any(|e| e["command"] == "doctor"));
    }

    #[test]
    fn known_filter_returns_singleton() {
        let envelope = build(Some("doctor"));
        let value = serde_json::to_value(&envelope).unwrap();
        let data = assert_envelope(&value, CATALOG_SCHEMA_VERSION);
        let arr = data.as_array().expect("data is an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["command"], "doctor");
    }

    #[test]
    fn unknown_filter_returns_error_envelope() {
        let envelope = build(Some("definitely-not-real"));
        let value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(value["schemaVersion"], CATALOG_SCHEMA_VERSION);
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "schema_not_found");
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("definitely-not-real"));
    }
}
