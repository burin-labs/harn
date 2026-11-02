//! Project the VM's live provider-usage schema into protocol bindings.

use super::records::{Record, Target};
use super::schema_records::SchemaRecords;

pub(super) fn records() -> Vec<Record> {
    let schema = harn_vm::llm::usage::provider_usage_schema();
    let names = [
        ("", "HarnProviderUsage".to_owned()),
        ("ReportedCacheTotals", "HarnReportedCacheTotals".to_owned()),
    ];
    SchemaRecords {
        schema: &schema,
        names: &names,
        label: "provider usage",
        require_all: false,
        metadata: |_, _, _| Ok(None),
    }
    .load_extensible()
    .expect("provider-usage schema projects to host records")
}

pub(super) fn append(out: &mut String, target: Target) {
    for record in records() {
        record.append(out, target);
    }
}

pub(super) fn round_trip_fixtures() -> Result<Vec<serde_json::Value>, String> {
    use harn_vm::llm::usage::{ProviderUsage, ReportedCacheTotals};

    let mut usage = ProviderUsage {
        run_id: "usage-fixture".into(),
        parent_session_id: String::new(),
        lineage_known: true,
        input_tokens: 32,
        output_tokens: 3,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reported_cache: None,
        provider_call_count: 1,
        unpriced_calls: 1,
        usage_unknown_calls: 0,
        known_cost_usd: 0.0,
        cost_usd: None,
    };
    let mut cases = vec![usage.clone()];
    usage.reported_cache = Some(Box::new(ReportedCacheTotals {
        reported_calls: 1,
        read_reported_calls: 1,
        write_reported_calls: 1,
        read_tokens: 0,
        write_tokens: 0,
    }));
    cases.push(usage.clone());
    usage.cache_read_tokens = 25;
    let cache = usage.reported_cache.as_mut().expect("measured cache");
    cache.read_tokens = 25;
    cache.write_reported_calls = 0;
    cases.push(usage);
    cases
        .into_iter()
        .map(|case| serde_json::to_value(case).map_err(|error| error.to_string()))
        .collect()
}
