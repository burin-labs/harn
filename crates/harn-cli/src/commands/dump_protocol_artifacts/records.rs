//! Shared field projections for protocol records and session-update payloads.

use std::borrow::Cow;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum FieldKind {
    NonEmptyString,
    String,
    Integer(Integer),
    Bool,
    LiteralBool(bool),
    StringList,
    Json,
    Named(String),
    List(Box<Self>),
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
    /// Existing session-update counters use Go's machine-sized int.
    HostCount,
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
                    Integer::U64 | Integer::HostCount | Integer::UnsignedHarn => "u64",
                    Integer::U32 => "u32",
                    Integer::Usize | Integer::HostIndex => "usize",
                    Integer::I64 | Integer::Harn => "i64",
                },
                Go => match integer {
                    Integer::U32 => "uint32",
                    Integer::I64 | Integer::Harn => "int64",
                    Integer::HostCount | Integer::HostIndex => "int",
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
            Self::StringList => Self::List(Box::new(Self::String)).type_name(target),
            kind => match (kind, target) {
                (Self::NonEmptyString | Self::String, Rust | Swift) => "String",
                (Self::NonEmptyString | Self::String, Python) => "str",
                (Self::NonEmptyString | Self::String, Typescript | Go) => "string",
                (Self::Bool, Rust | Go) => "bool",
                (Self::Bool, Swift) => "Bool",
                (Self::Bool, Typescript) => "boolean",
                (Self::Bool, Python) => "bool",
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
        if required || (matches!(target, Target::Rust) && matches!(self, Self::DefaultList(_))) {
            return inner;
        }
        match target {
            Target::Rust => format!("Option<{inner}>"),
            Target::Swift => format!("{inner}?"),
            Target::Python => format!("Optional[{inner}]"),
            Target::Go
                if !matches!(
                    self,
                    Self::StringList | Self::List(_) | Self::DefaultList(_) | Self::Json
                ) =>
            {
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
    pub identity: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Record {
    pub name: String,
    pub fields: Vec<Field>,
}

impl Record {
    pub(super) fn append(&self, out: &mut String, target: Target) {
        self.append_record(out, target, false, false);
    }

    pub(super) fn append_closed(&self, out: &mut String, target: Target) {
        self.append_record(out, target, true, true);
    }

    pub(super) fn append_mutable(&self, out: &mut String, target: Target) {
        self.append_record(out, target, false, true);
    }

    fn append_record(&self, out: &mut String, target: Target, closed: bool, mutable: bool) {
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
                    }
                    if !closed && field.rust_name != field.wire_name {
                        out.push_str(&format!("    #[serde(rename = {:?})]\n", field.wire_name));
                    }
                    out.push_str(&format!("    pub {}: {kind},\n", field.rust_name));
                }
                Target::Swift => out.push_str(&format!(
                    "    public {} {}: {kind}\n",
                    if mutable { "var" } else { "let" },
                    camel_ident(&field.wire_name)
                )),
                Target::Typescript => {
                    out.push_str(&format!("  {}{optional}: {kind}\n", field.wire_name))
                }
                Target::Python => out.push_str(&format!(
                    "    {}: {kind}{}\n",
                    field.wire_name,
                    if !closed && !field.required {
                        " = None"
                    } else {
                        ""
                    }
                )),
                Target::Go => out.push_str(&format!(
                    "    {} {kind} `json:\"{}{}\"`\n",
                    go_ident(&field.wire_name),
                    field.wire_name,
                    if field.required { "" } else { ",omitempty" },
                )),
            }
        }
        if matches!(target, Target::Swift)
            && self
                .fields
                .iter()
                .any(|field| camel_ident(&field.wire_name) != field.wire_name)
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
        out.push_str(if matches!(target, Target::Python) {
            "\n"
        } else {
            "}\n\n"
        });
    }
}

fn camel_ident(value: &str) -> String {
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
    fn nullable_list_elements_keep_typescript_union_precedence() {
        let kind = FieldKind::List(Box::new(FieldKind::Nullable(Box::new(FieldKind::String))));
        assert_eq!(kind.type_name(Target::Typescript), "(string | null)[]");
    }
}
