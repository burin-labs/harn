//! Shared helpers for the config-builtin tests.

use crate::value::VmValue;

pub(super) fn build_dict(entries: Vec<(&str, VmValue)>) -> VmValue {
    let mut map = crate::value::DictMap::new();
    for (k, v) in entries {
        map.insert(crate::value::intern_key(k), v);
    }
    VmValue::dict(map)
}

pub(super) fn expect_int(dict: &crate::value::DictMap, key: &str, want: i64) {
    match dict.get(key) {
        Some(VmValue::Int(value)) => assert_eq!(*value, want, "{key}"),
        other => panic!("expected Int({want}) for {key}, got {other:?}"),
    }
}

pub(super) fn credential_status_for(entries: &VmValue, provider: &str) -> String {
    let VmValue::List(list) = entries else {
        panic!("expected a list of provider status dicts");
    };
    for entry in list.iter() {
        let VmValue::Dict(dict) = entry else {
            continue;
        };
        if dict.get("name").map(|v| v.display()) == Some(provider.to_string()) {
            return dict
                .get("credential_status")
                .map(|v| v.display())
                .expect("credential_status present");
        }
    }
    panic!("provider {provider} not present in status list");
}
