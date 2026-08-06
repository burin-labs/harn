use crate::value::VmValue;

pub(super) fn string_list_to_vm_value(items: Vec<String>) -> VmValue {
    VmValue::List(std::sync::Arc::new(
        items
            .into_iter()
            .map(|item| VmValue::String(arcstr::ArcStr::from(item)))
            .collect(),
    ))
}

pub(super) fn optional_batch_string(batch_api: bool, value: Option<&str>) -> VmValue {
    batch_api
        .then_some(value)
        .flatten()
        .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
        .unwrap_or(VmValue::Nil)
}

pub(super) fn optional_batch_u32(batch_api: bool, value: Option<u32>) -> VmValue {
    batch_api
        .then_some(value)
        .flatten()
        .map(|value| VmValue::Int(value as i64))
        .unwrap_or(VmValue::Nil)
}

pub(super) fn optional_batch_u64(batch_api: bool, value: Option<u64>) -> VmValue {
    batch_api
        .then_some(value)
        .flatten()
        .and_then(|value| i64::try_from(value).ok())
        .map(VmValue::Int)
        .unwrap_or(VmValue::Nil)
}

pub(super) fn optional_batch_string_list(batch_api: bool, items: &[String]) -> VmValue {
    if batch_api && !items.is_empty() {
        string_list_to_vm_value(items.to_vec())
    } else {
        VmValue::Nil
    }
}
