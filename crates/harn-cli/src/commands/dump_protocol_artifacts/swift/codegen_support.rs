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

/// Members the open-enum struct declares itself. A wire value that camelCases
/// onto one of these would shadow the struct's own surface, so the generator
/// refuses rather than emitting Swift that does not compile.
const SWIFT_OPEN_ENUM_RESERVED_MEMBERS: &[&str] =
    &["rawValue", "allCases", "isKnown", "description", "init"];

/// A Swift open string enum: a `RawRepresentable` struct whose static members
/// are the generated vocabulary and whose initializer never fails, so a wire
/// value this binding does not name round-trips verbatim instead of failing to
/// decode.
///
/// The closed `enum` form emitted by [`swift_enum`] is the wrong shape for a
/// producer-owned vocabulary. Decoding a `String`-raw-value enum throws
/// `DataCorrupted` for a case it does not name, so one kind a newer Harn adds
/// does not degrade a field — it fails the whole containing payload, and a host
/// pinned to an older binding loses the entire terminal outcome. This mirrors
/// the Rust binding's `Unrecognized(String)` escape.
pub(crate) fn swift_open_enum(name: &str, doc: &str, values: &[String]) -> String {
    for value in values {
        let member = swift_case_name(value);
        assert!(
            !SWIFT_OPEN_ENUM_RESERVED_MEMBERS.contains(&member.as_str()),
            "wire value `{value}` in `{name}` collides with the open-enum member `{member}`"
        );
    }

    let mut out = String::new();
    for line in doc.lines() {
        out.push_str("/// ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(
        "/// Open vocabulary: the static members are the values this binding was generated\n\
         /// from, and any other wire string is preserved verbatim so a newer Harn never\n\
         /// breaks an older consumer.\n",
    );
    out.push_str(&format!(
        "public struct {name}: RawRepresentable, Codable, Sendable, Hashable, CaseIterable, \
         CustomStringConvertible {{\n"
    ));
    out.push_str("    public let rawValue: String\n\n");
    out.push_str(
        "    public init(rawValue: String) {\n        self.rawValue = rawValue\n    }\n\n",
    );
    out.push_str(
        "    public init(from decoder: any Decoder) throws {\n\
         \x20       self.rawValue = try decoder.singleValueContainer().decode(String.self)\n\
         \x20   }\n\n",
    );
    out.push_str(
        "    public func encode(to encoder: any Encoder) throws {\n\
         \x20       var container = encoder.singleValueContainer()\n\
         \x20       try container.encode(rawValue)\n\
         \x20   }\n\n",
    );
    for value in values {
        out.push_str("    public static let ");
        out.push_str(&swift_case_name(value));
        out.push_str(" = Self(rawValue: ");
        out.push_str(&json_string_literal(value));
        out.push_str(")\n");
    }
    out.push_str(
        "\n    /// Every value this binding was generated from, in wire order. A value\n\
         \x20   /// outside it is valid and preserved; it is simply not listed here.\n",
    );
    out.push_str("    public static let allCases: [Self] = [\n");
    for value in values {
        out.push_str("        ");
        out.push_str(&json_string_literal(value));
        out.push_str(",\n");
    }
    out.push_str("    ].map { Self(rawValue: $0) }\n\n");
    out.push_str(
        "    /// Whether this value is part of the vocabulary this binding was generated from.\n\
         \x20   public var isKnown: Bool { Self.allCases.contains(self) }\n\n",
    );
    out.push_str("    public var description: String { rawValue }\n");
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
