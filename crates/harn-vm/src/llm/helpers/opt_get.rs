use std::collections::BTreeMap;

use crate::value::VmValue;

pub(crate) fn opt_str(options: &Option<BTreeMap<String, VmValue>>, key: &str) -> Option<String> {
    match options.as_ref()?.get(key)? {
        VmValue::Nil => None,
        value => Some(value.display()),
    }
}

pub(crate) fn opt_int(options: &Option<BTreeMap<String, VmValue>>, key: &str) -> Option<i64> {
    options.as_ref()?.get(key)?.as_int()
}

pub(crate) fn opt_float(options: &Option<BTreeMap<String, VmValue>>, key: &str) -> Option<f64> {
    options.as_ref()?.get(key).and_then(|v| match v {
        VmValue::Float(f) => Some(*f),
        VmValue::Int(i) => Some(*i as f64),
        _ => None,
    })
}

pub(crate) fn opt_bool(options: &Option<BTreeMap<String, VmValue>>, key: &str) -> bool {
    options
        .as_ref()
        .and_then(|o| o.get(key))
        .map(|v| v.is_truthy())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, rc::Rc};

    use crate::value::VmValue;

    use super::opt_str;

    #[test]
    fn opt_str_treats_nil_as_unset() {
        let mut options = BTreeMap::new();
        options.insert("path".to_string(), VmValue::Nil);

        assert_eq!(opt_str(&Some(options), "path"), None);
    }

    #[test]
    fn opt_str_preserves_string_values() {
        let mut options = BTreeMap::new();
        options.insert("path".to_string(), VmValue::String(Rc::from("transcripts")));

        assert_eq!(
            opt_str(&Some(options), "path"),
            Some("transcripts".to_string())
        );
    }
}
