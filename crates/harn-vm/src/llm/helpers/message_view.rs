//! Read-only field access shared by JSON and VM chat messages.
//!
//! Directive placement and lineage decisions read a handful of message fields.
//! The provider request path holds messages as JSON while the agent session
//! owns them as VM values. Reading both through this one view keeps a single
//! implementation of each decision and lets session builtins inspect the
//! durable transcript in place instead of converting it to JSON every turn.
//!
//! Each accessor matches only the exact JSON shape it names: a VM string is a
//! JSON string, a VM list is a JSON array, and a VM bool is a JSON bool.

use crate::value::VmValue;

pub(crate) trait MessageView {
    /// The field's value when present, including an explicit `null`/`nil`.
    fn has_field(&self, key: &str) -> bool;
    /// The field when it is a string.
    fn str_field(&self, key: &str) -> Option<&str>;
    /// The field when it is a bool.
    fn bool_field(&self, key: &str) -> Option<bool>;
    /// The string items of the field when it is a list; non-string items are skipped.
    fn str_list_field(&self, key: &str) -> Option<Vec<&str>>;
    /// The number of items in the field when it is a list.
    fn list_len_field(&self, key: &str) -> Option<usize>;
    /// Every text fragment `content` carries, whether it is a bare string or a
    /// list of typed blocks, joined by newlines.
    fn content_text(&self) -> String;
}

impl MessageView for serde_json::Value {
    fn has_field(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    fn str_field(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(serde_json::Value::as_str)
    }

    fn bool_field(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(serde_json::Value::as_bool)
    }

    fn str_list_field(&self, key: &str) -> Option<Vec<&str>> {
        self.get(key)
            .and_then(serde_json::Value::as_array)
            .map(|items| items.iter().filter_map(serde_json::Value::as_str).collect())
    }

    fn list_len_field(&self, key: &str) -> Option<usize> {
        self.get(key)
            .and_then(serde_json::Value::as_array)
            .map(Vec::len)
    }

    fn content_text(&self) -> String {
        match self.get("content") {
            Some(serde_json::Value::String(text)) => text.clone(),
            Some(serde_json::Value::Array(blocks)) => blocks
                .iter()
                .filter_map(|block| block.str_field("text"))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }
    }
}

impl MessageView for VmValue {
    fn has_field(&self, key: &str) -> bool {
        self.as_dict().is_some_and(|dict| dict.contains_key(key))
    }

    fn str_field(&self, key: &str) -> Option<&str> {
        match self.as_dict()?.get(key)? {
            VmValue::String(text) => Some(text.as_str()),
            _ => None,
        }
    }

    fn bool_field(&self, key: &str) -> Option<bool> {
        match self.as_dict()?.get(key)? {
            VmValue::Bool(value) => Some(*value),
            _ => None,
        }
    }

    fn str_list_field(&self, key: &str) -> Option<Vec<&str>> {
        match self.as_dict()?.get(key)? {
            VmValue::List(items) => Some(
                items
                    .iter()
                    .filter_map(|item| match item {
                        VmValue::String(text) => Some(text.as_str()),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        }
    }

    fn list_len_field(&self, key: &str) -> Option<usize> {
        match self.as_dict()?.get(key)? {
            VmValue::List(items) => Some(items.len()),
            _ => None,
        }
    }

    fn content_text(&self) -> String {
        match self.as_dict().and_then(|dict| dict.get("content")) {
            Some(VmValue::String(text)) => text.to_string(),
            Some(VmValue::List(blocks)) => blocks
                .iter()
                .filter_map(|block| block.str_field("text"))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }
    }
}

/// Whether `value` displays as `expected`, without allocating for the common
/// string case. Equivalent to `value.display() == expected`.
pub(crate) fn displays_as(value: &VmValue, expected: &str) -> bool {
    match value {
        VmValue::String(text) => text.as_str() == expected,
        other => other.display() == expected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The VM view must answer every accessor exactly as the JSON view does
    /// for the same message, since both feed one placement decision.
    #[test]
    fn vm_and_json_views_agree() {
        let samples = [
            serde_json::json!({"role": "user", "content": "plain"}),
            serde_json::json!({
                "role": "assistant",
                "content": [{"type": "text", "text": "a"}, {"type": "image"}, {"text": "b"}],
                "tool_calls": [{"id": "1"}],
                "_harn_directive_ids": ["r1", 7, "r2"],
                "_bookkeeping": true,
                "nil_field": null,
            }),
            serde_json::json!({"role": 3, "content": {"text": "not a list"}, "tool_calls": "x"}),
            serde_json::json!("not an object"),
        ];
        let keys = [
            "role",
            "content",
            "tool_calls",
            "_harn_directive_ids",
            "_bookkeeping",
            "nil_field",
            "missing",
        ];
        for json in samples {
            let vm = crate::stdlib::json_to_vm_value(&json);
            assert_eq!(json.content_text(), vm.content_text(), "{json}");
            for key in keys {
                assert_eq!(json.has_field(key), vm.has_field(key), "{json} {key}");
                assert_eq!(json.str_field(key), vm.str_field(key), "{json} {key}");
                assert_eq!(json.bool_field(key), vm.bool_field(key), "{json} {key}");
                assert_eq!(
                    json.str_list_field(key),
                    vm.str_list_field(key),
                    "{json} {key}"
                );
                assert_eq!(
                    json.list_len_field(key),
                    vm.list_len_field(key),
                    "{json} {key}"
                );
            }
        }
    }
}
