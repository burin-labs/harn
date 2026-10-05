//! Header lookups shared by the connector modules.

use std::collections::BTreeMap;

/// The value of header `name`, matched case-insensitively.
pub(crate) fn header_value<'a>(
    headers: &'a BTreeMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}
