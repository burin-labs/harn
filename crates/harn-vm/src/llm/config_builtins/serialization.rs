use serde::Serialize;

use crate::schema::json_to_vm_value;
use crate::value::VmValue;

pub(super) fn value<T: Serialize>(value: &T) -> VmValue {
    json_to_vm_value(&serde_json::to_value(value).expect("catalog data is serializable"))
}

/// Select a public surface with checked field identifiers. Field serialization
/// retains nil/empty values independently of catalog-level omission attributes.
macro_rules! fields {
    ($value:expr; $($field:ident),* $(,)?) => {{
        let source = $value;
        let mut fields = $crate::value::DictMap::new();
        $(fields.insert(
            $crate::value::intern_key(stringify!($field)),
            $crate::llm::config_builtins::serialization::value(&source.$field),
        );)*
        fields
    }};
}

pub(super) use fields;
