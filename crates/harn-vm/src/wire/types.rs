use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Stable semantic identity, independent of a backend's allocated symbol.
#[derive(
    Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Hash, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(transparent)]
pub struct WireTypeId(pub String);

/// A declaration location, never a rejected invocation value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireSourceRef {
    TextSpan {
        document: String,
        start: u32,
        end: u32,
    },
    DocumentPointer {
        document: String,
        pointer: String,
    },
    Synthetic {
        owner: String,
    },
}

/// Exact literal spelling keeps full-width integers out of a host JSON parser.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireLiteral {
    Null,
    Boolean { value: bool },
    String { value: String },
    Integer { value: String },
    Float { value: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum WireIntegerEncoding {
    JsonNumber,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum WireBytesEncoding {
    Base64Envelope,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum WireDecimalEncoding {
    CanonicalString,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct WireField {
    pub id: String,
    pub name: String,
    pub type_id: WireTypeId,
    pub optional: bool,
    pub source: WireSourceRef,
    pub documentation: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct WireDeclarationIdentity {
    pub declaration_id: String,
    pub name: String,
    pub arguments: Vec<WireTypeId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct WireEnumVariant {
    pub id: String,
    pub name: String,
    pub fields: Vec<WireField>,
    pub source: WireSourceRef,
    pub documentation: Option<String>,
}

/// Closed, normalized JSON wire algebra. References are graph edges.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireShape {
    Null,
    Boolean,
    Integer {
        encoding: WireIntegerEncoding,
        minimum: String,
        maximum: String,
    },
    Float,
    String,
    Bytes {
        encoding: WireBytesEncoding,
    },
    Decimal {
        encoding: WireDecimalEncoding,
    },
    Literal {
        value: WireLiteral,
    },
    List {
        item: WireTypeId,
    },
    Set {
        item: WireTypeId,
    },
    Tuple {
        items: Vec<WireTypeId>,
    },
    Map {
        value: WireTypeId,
    },
    Record {
        fields: Vec<WireField>,
        additional: Option<WireTypeId>,
        nominal: Option<WireDeclarationIdentity>,
    },
    Enum {
        nominal: WireDeclarationIdentity,
        variants: Vec<WireEnumVariant>,
    },
    Union {
        members: Vec<WireTypeId>,
    },
    Alias {
        target: WireTypeId,
        declaration: WireDeclarationIdentity,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct WireType {
    pub id: WireTypeId,
    pub name: Option<String>,
    pub source: WireSourceRef,
    pub documentation: Option<String>,
    pub shape: WireShape,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum WireFailureCode {
    UnsupportedType,
    UnresolvedReference,
    RecursiveExpansion,
    AmbiguousUnion,
    InvalidContract,
    InvalidValue,
    WrongRuntimeType,
    MaximumDepth,
    DuplicateSetMember,
    InvalidBase64,
    InvalidDecimal,
    NonFiniteFloat,
    UnsupportedMapKey,
    InvalidNominalDiscriminator,
    UnknownField,
    MissingField,
    InvalidRange,
}

/// Value-free failure shared by preparation, decoding and encoding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct WireFailure {
    pub code: WireFailureCode,
    pub type_id: Option<WireTypeId>,
    pub source: WireSourceRef,
    pub structural_path: String,
}

impl std::fmt::Display for WireFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "wire {:?}", self.code)?;
        if !self.structural_path.is_empty() {
            write!(formatter, " at {}", self.structural_path)?;
        }
        Ok(())
    }
}

impl std::error::Error for WireFailure {}
