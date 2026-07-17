pub(super) use crate::test_util::process::{run_harn_e2e as run, HarnCliOutput};

pub(super) const LORA_PROMOTION_EVIDENCE_SCHEMA_VERSION: u64 = 5;

pub(super) fn parse_json(s: &str, label: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap_or_else(|err| {
        panic!("{label} stdout is not valid JSON: {err}\n--- payload ---\n{s}")
    })
}

pub(super) fn success_data(value: &serde_json::Value) -> &serde_json::Value {
    assert_eq!(value["ok"], serde_json::Value::Bool(true));
    value["data"].as_object().expect("success envelope data");
    &value["data"]
}
