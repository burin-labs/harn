pub(super) use crate::test_util::cli::parse_json;

pub(super) use crate::test_util::process::{harn_e2e_binary, run_harn_e2e as run, HarnCliOutput};

pub(super) const LORA_PROMOTION_EVIDENCE_SCHEMA_VERSION: u64 = 5;

pub(super) fn success_data(value: &serde_json::Value) -> &serde_json::Value {
    assert_eq!(value["ok"], serde_json::Value::Bool(true));
    value["data"].as_object().expect("success envelope data");
    &value["data"]
}
