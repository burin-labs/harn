//! Shared field projections for protocol records and session-update payloads.

use std::borrow::Cow;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum FieldKind {
    NonEmptyString,
    String,
    Integer(Integer),
    Number,
    Bool,
    LiteralBool(bool),
    Json,
    Named(String),
    List(Box<Self>),
    Map(Box<Self>),
    /// An optional list whose Rust projection defaults to an empty vector.
    DefaultList(Box<Self>),
    Nullable(Box<Self>),
    JsonObject,
    Literal {
        value: String,
        wire_type: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Integer {
    /// Existing plan ranges use Rust usize and Go's machine-sized int.
    HostIndex,
    /// Harn's signed integer projects to Swift Int64.
    Harn,
    /// Existing permission timestamps retain Rust u64 and Swift Int64.
    UnsignedHarn,
    U64,
    U32,
    Usize,
    I64,
}

#[derive(Clone, Copy)]
pub(super) enum Target {
    Rust,
    Swift,
    Typescript,
    Python,
    Go,
}

impl FieldKind {
    pub(super) fn type_name(&self, target: Target) -> String {
        use Target::*;
        match self {
            Self::Integer(integer) => match target {
                Rust => match integer {
                    Integer::U64 | Integer::UnsignedHarn => "u64",
                    Integer::U32 => "u32",
                    Integer::Usize | Integer::HostIndex => "usize",
                    Integer::I64 | Integer::Harn => "i64",
                },
                Go => match integer {
                    Integer::U32 => "uint32",
                    Integer::I64 | Integer::Harn => "int64",
                    Integer::HostIndex => "int",
                    _ => "uint64",
                },
                Swift if matches!(integer, Integer::Harn | Integer::UnsignedHarn) => "Int64",
                Swift => "Int",
                Typescript => "number",
                Python => "int",
            }
            .into(),
            Self::Nullable(inner) => match target {
                Typescript => format!("{} | null", inner.type_name(target)),
                Go if matches!(inner.as_ref(), Self::Json) => "JSONValue".into(),
                _ => inner.optional_type(target, false),
            },
            Self::JsonObject => match target {
                Rust => "std::collections::BTreeMap<String, Value>",
                Swift => "[String: HarnACPValue]",
                Typescript => "Record<string, ACPValue>",
                Python => "Dict[str, JsonValue]",
                Go => "JSONObject",
            }
            .into(),
            Self::Named(name) if name == "HarnACPToolKind" && matches!(target, Typescript) => {
                "ACPToolKind".into()
            }
            Self::Named(name) => name.clone(),
            Self::LiteralBool(value) => match target {
                Typescript => value.to_string(),
                _ => Self::Bool.type_name(target),
            },
            Self::Literal { value, wire_type } => match target {
                Typescript => super::support::json_string_literal(value),
                _ => wire_type
                    .clone()
                    .unwrap_or_else(|| Self::String.type_name(target)),
            },
            Self::List(inner) | Self::DefaultList(inner) => {
                let item = inner.type_name(target);
                match target {
                    Rust => format!("Vec<{item}>"),
                    Swift => format!("[{item}]"),
                    Typescript if matches!(inner.as_ref(), Self::Nullable(_)) => {
                        format!("({item})[]")
                    }
                    Typescript => format!("{item}[]"),
                    Python => format!("List[{item}]"),
                    Go => format!("[]{item}"),
                }
            }
            Self::Map(inner) => {
                let value = inner.type_name(target);
                match target {
                    Rust => format!("std::collections::BTreeMap<String, {value}>"),
                    Swift => format!("[String: {value}]"),
                    Typescript => format!("Record<string, {value}>"),
                    Python => format!("Dict[str, {value}]"),
                    Go => format!("map[string]{value}"),
                }
            }
            kind => match (kind, target) {
                (Self::NonEmptyString | Self::String, Rust | Swift) => "String",
                (Self::NonEmptyString | Self::String, Python) => "str",
                (Self::NonEmptyString | Self::String, Typescript | Go) => "string",
                (Self::Bool, Rust | Go) => "bool",
                (Self::Bool, Swift) => "Bool",
                (Self::Bool, Typescript) => "boolean",
                (Self::Bool, Python) => "bool",
                (Self::Number, Rust) => "serde_json::Number",
                (Self::Number, Swift) => "Double",
                (Self::Number, Typescript) => "number",
                (Self::Number, Python) => "float",
                (Self::Number, Go) => "float64",
                (Self::Json, Rust) => "Value",
                (Self::Json, Swift) => "HarnACPValue",
                (Self::Json, Typescript) => "ACPValue",
                (Self::Json, Python) => "JsonValue",
                (Self::Json, Go) => "json.RawMessage",
                _ => unreachable!("composite field handled above"),
            }
            .into(),
        }
    }

    pub(super) fn optional_type(&self, target: Target, required: bool) -> String {
        let inner = self.type_name(target);
        // These bindings represent an absent or null field with the same empty
        // value. Nullable already supplies that wrapper; adding another cannot
        // preserve presence with their generated decoders.
        if required
            || matches!(self, Self::Nullable(_))
            || (matches!(target, Target::Rust) && matches!(self, Self::DefaultList(_)))
        {
            return inner;
        }
        match target {
            Target::Rust => format!("Option<{inner}>"),
            Target::Swift => format!("{inner}?"),
            Target::Python => format!("Optional[{inner}]"),
            Target::Go if !matches!(self, Self::List(_) | Self::DefaultList(_) | Self::Json) => {
                format!("*{inner}")
            }
            _ => inner,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Field {
    pub wire_name: Cow<'static, str>,
    pub rust_name: Cow<'static, str>,
    pub kind: FieldKind,
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Record {
    pub name: String,
    pub fields: Vec<Field>,
}

pub(super) fn append_string_enum(out: &mut String, target: Target, name: &str, values: &[String]) {
    match target {
        Target::Typescript => out.push_str(&format!("export type {name} = {}\n", values.iter().map(|value| format!("{value:?}")).collect::<Vec<_>>().join(" | "))),
        Target::Rust => out.push_str(&format!("#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]\n#[serde(rename_all = \"snake_case\")]\npub enum {name} {{ {} }}\n", values.iter().map(|value| super::rust::rust_type_name(value)).collect::<Vec<_>>().join(", "))),
        Target::Swift => out.push_str(&format!("public enum {name}: String, Codable, Sendable, Equatable {{ case {} }}\n", values.iter().map(|value| {
            let case = super::swift::swift_case_name(value);
            if &case == value { case } else { format!("{case} = {value:?}") }
        }).collect::<Vec<_>>().join(", "))),
        Target::Python => out.push_str(&super::python::py_str_enum_owned(name, values)),
        Target::Go => out.push_str(&format!("type {name} string\n")),
    }
}

impl Record {
    pub(super) fn append_with_initializer(&self, out: &mut String, target: Target) {
        if !matches!(target, Target::Swift) {
            self.append_mutable(out, target, true);
            if matches!(target, Target::Python) {
                let required = self
                    .fields
                    .iter()
                    .filter(|field| field.required)
                    .map(|field| format!("{:?}", field.wire_name))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "{}.__harn_required_fields__ = frozenset([{required}])\n\n",
                    self.name
                ));
            }
            return;
        }
        let parameters = self
            .fields
            .iter()
            .map(|field| {
                let name = camel_ident(&field.wire_name);
                let kind = field.kind.optional_type(target, field.required);
                let default = if !field.required || matches!(field.kind, FieldKind::Nullable(_)) {
                    " = nil"
                } else {
                    ""
                };
                format!("{name}: {kind}{default}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        let mut body = format!("\n    public init({parameters}) {{\n");
        for field in &self.fields {
            let name = camel_ident(&field.wire_name);
            body.push_str(&format!("        self.{name} = {name}\n"));
        }
        body.push_str("    }\n");
        self.append_record(
            out,
            target,
            false,
            true,
            true,
            Some((&body, &BTreeMap::new())),
        );
    }

    pub(super) fn append(&self, out: &mut String, target: Target) {
        self.append_record(out, target, false, false, false, None);
    }

    pub(super) fn append_closed(&self, out: &mut String, target: Target) {
        self.append_record(out, target, true, true, false, None);
    }

    pub(super) fn append_mutable(
        &self,
        out: &mut String,
        target: Target,
        preserve_wire_presence: bool,
    ) {
        self.append_record(out, target, false, true, preserve_wire_presence, None);
    }

    pub(super) fn append_swift_with_body(
        &self,
        out: &mut String,
        body: &str,
        defaults: &BTreeMap<String, String>,
    ) {
        self.append_record(
            out,
            Target::Swift,
            false,
            true,
            false,
            Some((body, defaults)),
        );
    }

    fn append_record(
        &self,
        out: &mut String,
        target: Target,
        closed: bool,
        mutable: bool,
        preserve_wire_presence: bool,
        swift_body: Option<(&str, &BTreeMap<String, String>)>,
    ) {
        let name = &self.name;
        match target {
            Target::Rust if closed => out.push_str(&format!("#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]\n#[serde(rename_all = \"camelCase\", deny_unknown_fields)]\npub struct {name} {{\n")),
            Target::Rust => out.push_str(&format!("#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]\npub struct {name} {{\n")),
            Target::Swift => out.push_str(&format!("public struct {name}: Codable, Sendable, Equatable {{\n")),
            Target::Typescript => out.push_str(&format!("export interface {name} {{\n")),
            Target::Python => out.push_str(&format!("@dataclass\nclass {name}({}):\n", if closed { "_HarnStrictRecapDataclass" } else { "_HarnDataclass" })),
            Target::Go => out.push_str(&format!("type {name} struct {{\n")),
        }
        for field in &self.fields {
            let optional = if field.required { "" } else { "?" };
            let kind = field.kind.optional_type(target, field.required);
            match target {
                Target::Rust => {
                    if closed && matches!(field.kind, FieldKind::JsonObject) {
                        out.push_str("    #[serde(default)]\n");
                    }
                    if !field.required {
                        let predicate = if matches!(field.kind, FieldKind::DefaultList(_)) {
                            "Vec::is_empty"
                        } else {
                            "Option::is_none"
                        };
                        out.push_str(&format!(
                            "    #[serde(default, skip_serializing_if = {predicate:?})]\n"
                        ));
                        if preserve_wire_presence && matches!(field.kind, FieldKind::Json) {
                            out.push_str("    #[serde(deserialize_with = \"deserialize_present_session_update_value\")]\n");
                        }
                    }
                    if !closed && field.rust_name != field.wire_name {
                        out.push_str(&format!("    #[serde(rename = {:?})]\n", field.wire_name));
                    }
                    out.push_str(&format!("    pub {}: {kind},\n", field.rust_name));
                }
                Target::Swift => {
                    let default = swift_body
                        .and_then(|(_, defaults)| defaults.get(field.wire_name.as_ref()))
                        .map(|value| format!(" = {value}"))
                        .unwrap_or_default();
                    out.push_str(&format!(
                        "    public {} {}: {kind}{default}\n",
                        if mutable { "var" } else { "let" },
                        camel_ident(&field.wire_name)
                    ));
                }
                Target::Typescript => {
                    out.push_str(&format!("  {}{optional}: {kind}\n", field.wire_name));
                }
                Target::Python => {
                    let ident = super::python::field_ident(&field.wire_name);
                    let default = if ident != field.wire_name {
                        let default = if !closed && !field.required {
                            "default=None, "
                        } else {
                            ""
                        };
                        format!(
                            " = field({default}metadata={{\"wire_name\": {:?}}})",
                            field.wire_name
                        )
                    } else if !closed && !field.required {
                        " = None".into()
                    } else {
                        String::new()
                    };
                    out.push_str(&format!("    {ident}: {kind}{default}\n"));
                }
                Target::Go => out.push_str(&format!(
                    "    {} {kind} `json:\"{}{}\"`\n",
                    go_ident(&field.wire_name),
                    field.wire_name,
                    if field.required { "" } else { ",omitempty" },
                )),
            }
        }
        let explicit_swift_nulls = matches!(target, Target::Swift)
            && preserve_wire_presence
            && self
                .fields
                .iter()
                .any(|field| field.required && matches!(field.kind, FieldKind::Nullable(_)));
        let explicit_swift_presence = matches!(target, Target::Swift)
            && preserve_wire_presence
            && self
                .fields
                .iter()
                .any(|field| !field.required && matches!(field.kind, FieldKind::Json));
        if matches!(target, Target::Swift)
            && (explicit_swift_nulls
                || explicit_swift_presence
                || swift_body.is_some()
                || self
                    .fields
                    .iter()
                    .any(|field| camel_ident(&field.wire_name) != field.wire_name))
        {
            out.push_str("\n    enum CodingKeys: String, CodingKey {\n");
            for field in &self.fields {
                let name = camel_ident(&field.wire_name);
                out.push_str(&format!("        case {name}"));
                if name != field.wire_name {
                    out.push_str(&format!(" = {:?}", field.wire_name));
                }
                out.push('\n');
            }
            out.push_str("    }\n");
        }
        // A required-nullable field needs the custom decoder just as much as a
        // present-versus-absent one does, and for a reason the synthesized
        // decoder cannot express: the Swift property is `T?` either way, so
        // Codable synthesis reaches for `decodeIfPresent` and an omitted key
        // reads as `nil` rather than as the contract violation it is.
        //
        // Gating this on `explicit_swift_presence` alone was silently
        // conditional on an unrelated fact. A struct keeps its enforcement
        // only while some OTHER field happens to be a non-required JSON value,
        // and loses it the moment those fields become required — which is what
        // happened to the transcript-compacted meta struct, one of eleven,
        // when its optional JSON fields were tightened. The encoder below
        // stayed, so the artifact still wrote every key while no longer
        // refusing a frame that omitted one.
        if explicit_swift_nulls || explicit_swift_presence {
            out.push_str("\n    public init(from decoder: Decoder) throws {\n        let values = try decoder.container(keyedBy: CodingKeys.self)\n");
            for field in &self.fields {
                let name = camel_ident(&field.wire_name);
                let kind = match (&field.kind, field.required) {
                    (FieldKind::Nullable(inner), false) => inner.type_name(target),
                    _ => field.kind.type_name(target),
                };
                if !field.required && matches!(field.kind, FieldKind::Json) {
                    out.push_str(&format!("        {name} = values.contains(.{name}) ? try values.decode({kind}.self, forKey: .{name}) : nil\n"));
                } else {
                    let decode = if field.required {
                        "decode"
                    } else {
                        "decodeIfPresent"
                    };
                    out.push_str(&format!(
                        "        {name} = try values.{decode}({kind}.self, forKey: .{name})\n"
                    ));
                }
            }
            out.push_str("    }\n");
        }
        if explicit_swift_nulls {
            out.push_str("\n    public func encode(to encoder: Encoder) throws {\n        var values = encoder.container(keyedBy: CodingKeys.self)\n");
            for field in &self.fields {
                let name = camel_ident(&field.wire_name);
                let encode = if field.required {
                    "encode"
                } else {
                    "encodeIfPresent"
                };
                out.push_str(&format!(
                    "        try values.{encode}({name}, forKey: .{name})\n"
                ));
            }
            out.push_str("    }\n");
        }
        if let Some((body, _)) = swift_body {
            out.push_str(body);
        }
        out.push_str(if matches!(target, Target::Python) {
            "\n"
        } else {
            "}\n\n"
        });
    }
}

pub(super) fn camel_ident(value: &str) -> String {
    let mut parts = value.trim_start_matches('_').split('_');
    let mut out = parts.next().unwrap_or_default().to_owned();
    for part in parts {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
        }
        out.extend(chars);
    }
    out
}

pub(super) fn snake_ident(value: &str) -> String {
    let mut out = String::new();
    for (index, ch) in value.chars().enumerate() {
        if ch.is_uppercase() && index > 0 {
            out.push('_');
        }
        out.extend(ch.to_lowercase());
    }
    out
}

fn go_ident(value: &str) -> String {
    snake_ident(value)
        .split('_')
        .map(|word| {
            if word == "id" {
                return "ID".into();
            }
            if word == "ids" {
                return "IDs".into();
            }
            let mut chars = word.chars();
            chars
                .next()
                .map(|ch| ch.to_uppercase().to_string())
                .unwrap_or_default()
                + chars.as_str()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_nullable_fields_share_one_empty_value_wrapper() {
        let kind = FieldKind::Nullable(Box::new(FieldKind::String));
        for (target, expected) in [
            (Target::Rust, "Option<String>"),
            (Target::Swift, "String?"),
            (Target::Python, "Optional[str]"),
            (Target::Go, "*string"),
            (Target::Typescript, "string | null"),
        ] {
            assert_eq!(kind.optional_type(target, false), expected);
            assert_eq!(kind.optional_type(target, true), expected);
        }
    }

    #[test]
    fn nullable_list_elements_keep_typescript_union_precedence() {
        let kind = FieldKind::List(Box::new(FieldKind::Nullable(Box::new(FieldKind::String))));
        assert_eq!(kind.type_name(Target::Typescript), "(string | null)[]");
    }
}
