use super::super::constants::DeprecatedWireValue;
use super::super::support::json_string_literal;

pub(crate) fn swift_enum(name: &str, values: &[String]) -> String {
    swift_enum_with_deprecations(name, values, &[])
}

pub(crate) fn swift_enum_with_deprecations(
    name: &str,
    values: &[String],
    deprecated_values: &[DeprecatedWireValue],
) -> String {
    let mut out = format!("public enum {name}: String, Codable, Sendable, CaseIterable {{\n");
    for value in values {
        if let Some(deprecated) = deprecated_wire_value(deprecated_values, value) {
            out.push_str("    @available(*, deprecated, message: ");
            out.push_str(&json_string_literal(&deprecation_message(deprecated)));
            out.push_str(")\n");
        }
        out.push_str("    case ");
        out.push_str(&swift_case_name(value));
        out.push_str(" = ");
        out.push_str(&json_string_literal(value));
        out.push('\n');
    }
    out.push_str("\n    public static let allCases: [Self] = [\n");
    for value in values {
        out.push_str("        ");
        out.push_str(&json_string_literal(value));
        out.push_str(",\n");
    }
    out.push_str("    ].map { Self(rawValue: $0)! }\n");
    out.push_str("}\n\n");
    out
}

pub(crate) fn swift_case_name(value: &str) -> String {
    let mut out = String::new();
    for (index, part) in value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .enumerate()
    {
        if index == 0 {
            out.push_str(part);
        } else {
            let mut chars = part.chars();
            if let Some(first) = chars.next() {
                out.extend(first.to_uppercase());
                out.push_str(chars.as_str());
            }
        }
    }
    if out.is_empty() {
        "unknown".to_string()
    } else if out.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        format!("_{out}")
    } else if SWIFT_RESERVED_KEYWORDS.contains(&out.as_str()) {
        format!("`{out}`")
    } else {
        out
    }
}

/// Swift reserved keywords that cannot appear as bare identifiers in a `case`
/// declaration. Wire values like `private` / `public` (e.g. on
/// `HarnMCPCacheScope`) camelCase down to themselves, so without escaping they
/// land in the generated Swift as `case private = "private"`, which fails to
/// compile.
const SWIFT_RESERVED_KEYWORDS: &[&str] = &[
    "associatedtype",
    "break",
    "case",
    "catch",
    "class",
    "continue",
    "default",
    "defer",
    "deinit",
    "do",
    "else",
    "enum",
    "extension",
    "fallthrough",
    "false",
    "fileprivate",
    "for",
    "func",
    "guard",
    "if",
    "import",
    "in",
    "init",
    "inout",
    "internal",
    "is",
    "let",
    "nil",
    "open",
    "operator",
    "private",
    "protocol",
    "public",
    "repeat",
    "return",
    "rethrows",
    "self",
    "Self",
    "static",
    "struct",
    "subscript",
    "super",
    "switch",
    "throw",
    "throws",
    "true",
    "try",
    "typealias",
    "var",
    "where",
    "while",
];

pub(crate) fn deprecated_wire_value<'a>(
    deprecated_values: &'a [DeprecatedWireValue],
    value: &str,
) -> Option<&'a DeprecatedWireValue> {
    deprecated_values
        .iter()
        .find(|deprecated| deprecated.value == value)
}

pub(crate) fn deprecation_message(value: &DeprecatedWireValue) -> String {
    format!(
        "Use {}; {} will be removed after one release.",
        value.replacement, value.value
    )
}

pub(crate) fn wire_value_property_name(value: &str) -> String {
    swift_case_name(value)
}
