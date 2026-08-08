//! Runtime type-contract matching shared by native and portable execution.
//!
//! The parser owns `TypeExpr`; this module owns how those expressions match
//! runtime values. Runtimes project their value representation through the
//! small `TypeContractValue` interface instead of maintaining another type
//! dispatch table.

use harn_parser::builtin_signatures::{BuiltinSignature, Ty};
use harn_parser::TypeExpr;

use crate::DataValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeTypeKind {
    Int,
    Float,
    Decimal,
    String,
    Bytes,
    Bool,
    Nil,
    List,
    Dict,
    Closure,
    Duration,
    Enum,
    Struct,
    TaskHandle,
    Channel,
    Atomic,
    Rng,
    SyncPermit,
    Resource,
    ResourceGuard,
    McpClient,
    VerdictReceipt,
    Set,
    Generator,
    Stream,
    Range,
    Iter,
    Pair,
    Harness,
}

pub trait TypeContractValue: Sized {
    fn runtime_type_kind(&self) -> RuntimeTypeKind;
    fn list_items(&self) -> Option<&[Self]> {
        None
    }
    fn record_field(&self, _name: &str) -> Option<&Self> {
        None
    }
    /// Apply `predicate` to every value in a dictionary-like value.
    ///
    /// Returning `None` distinguishes a value that is not enumerable from an
    /// empty dictionary. This keeps homogeneous `dict<K, V>` checks exact
    /// without forcing either runtime to allocate a projection vector.
    fn record_values_match(&self, _predicate: &mut dyn FnMut(&Self) -> bool) -> Option<bool> {
        None
    }
    fn string_literal(&self) -> Option<&str> {
        None
    }
    fn int_literal(&self) -> Option<i64> {
        None
    }
    fn nominal_type_name(&self) -> Option<&str> {
        None
    }
}

fn is_nominal(name: &str, nominal_type_names: &[String]) -> bool {
    nominal_type_names.iter().any(|ty| ty == name)
}

pub fn matches_type<V: TypeContractValue>(
    value: &V,
    expected: &TypeExpr,
    type_params: &[String],
    nominal_type_names: &[String],
) -> bool {
    use RuntimeTypeKind as Kind;
    match expected {
        TypeExpr::Named(name) => match name.as_str() {
            _ if type_params.iter().any(|param| param == name) => true,
            "any" | "unknown" => true,
            "int" => value.runtime_type_kind() == Kind::Int,
            "float" | "number" => matches!(value.runtime_type_kind(), Kind::Float | Kind::Int),
            "decimal" => value.runtime_type_kind() == Kind::Decimal,
            "string" => value.runtime_type_kind() == Kind::String,
            "bool" => value.runtime_type_kind() == Kind::Bool,
            "nil" => value.runtime_type_kind() == Kind::Nil,
            "list" => value.runtime_type_kind() == Kind::List,
            "dict" | "record" => value.runtime_type_kind() == Kind::Dict,
            "bytes" => value.runtime_type_kind() == Kind::Bytes,
            "duration" => value.runtime_type_kind() == Kind::Duration,
            "set" => value.runtime_type_kind() == Kind::Set,
            "range" => value.runtime_type_kind() == Kind::Range,
            "iter" => value.runtime_type_kind() == Kind::Iter,
            "generator" | "Generator" => value.runtime_type_kind() == Kind::Generator,
            "stream" | "Stream" => value.runtime_type_kind() == Kind::Stream,
            "channel" => value.runtime_type_kind() == Kind::Channel,
            "task_handle" => value.runtime_type_kind() == Kind::TaskHandle,
            "atomic" => value.runtime_type_kind() == Kind::Atomic,
            "rng" => value.runtime_type_kind() == Kind::Rng,
            "sync_permit" => value.runtime_type_kind() == Kind::SyncPermit,
            "resource" => value.runtime_type_kind() == Kind::Resource,
            "resource_guard" => value.runtime_type_kind() == Kind::ResourceGuard,
            "mcp_client" => value.runtime_type_kind() == Kind::McpClient,
            "verdict_receipt" => value.runtime_type_kind() == Kind::VerdictReceipt,
            "pair" => value.runtime_type_kind() == Kind::Pair,
            "enum" => value.runtime_type_kind() == Kind::Enum,
            "struct" => value.runtime_type_kind() == Kind::Struct,
            "closure" => value.runtime_type_kind() == Kind::Closure,
            _ if !nominal_type_names.iter().any(|ty| ty == name) => true,
            _ => value
                .nominal_type_name()
                .is_some_and(|actual| actual == name),
        },
        TypeExpr::Union(members) => members
            .iter()
            .any(|member| matches_type(value, member, type_params, nominal_type_names)),
        TypeExpr::Intersection(members) => members
            .iter()
            .all(|member| matches_type(value, member, type_params, nominal_type_names)),
        TypeExpr::List(inner) => value.list_items().is_some_and(|items| {
            items
                .iter()
                .all(|item| matches_type(item, inner, type_params, nominal_type_names))
        }),
        TypeExpr::Tuple(elements) => value.list_items().is_some_and(|items| {
            items.len() == elements.len()
                && items.iter().zip(elements).all(|(item, element)| {
                    matches_type(item, element, type_params, nominal_type_names)
                })
        }),
        TypeExpr::DictType(_, value_type) => {
            value.runtime_type_kind() == Kind::Dict
                && record_values_match(value, value_type, type_params, nominal_type_names)
        }
        TypeExpr::Iter(_) | TypeExpr::Generator(_) | TypeExpr::Stream(_) => matches!(
            value.runtime_type_kind(),
            Kind::List | Kind::Generator | Kind::Stream
        ),
        TypeExpr::Shape(fields) | TypeExpr::OpenShape { fields, .. } => {
            matches!(value.runtime_type_kind(), Kind::Dict | Kind::Struct)
                && fields
                    .iter()
                    .all(|field| match value.record_field(&field.name) {
                        Some(field_value)
                            if field.optional && field_value.runtime_type_kind() == Kind::Nil =>
                        {
                            true
                        }
                        Some(field_value) => matches_type(
                            field_value,
                            &field.type_expr,
                            type_params,
                            nominal_type_names,
                        ),
                        None => field.optional,
                    })
        }
        TypeExpr::Applied { name, args } => match (name.as_str(), args.as_slice()) {
            ("list" | "List", [inner]) => value.list_items().is_some_and(|items| {
                items
                    .iter()
                    .all(|item| matches_type(item, inner, type_params, nominal_type_names))
            }),
            ("dict" | "Dict", [_, value_type]) => {
                value.runtime_type_kind() == Kind::Dict
                    && record_values_match(value, value_type, type_params, nominal_type_names)
            }
            // `Option<T>` is the nullable spelling *unless* the program declares
            // its own nominal `Option`. A user-defined `enum Option<T>` produces
            // an `EnumVariant`, which is neither nil nor a `T`, so the built-in
            // reading rejects every value the user could construct.
            ("Option", [inner]) if !is_nominal("Option", nominal_type_names) => {
                value.runtime_type_kind() == Kind::Nil
                    || matches_type(value, inner, type_params, nominal_type_names)
            }
            // An applied nominal type constrains identity but not its arguments:
            // the VM does not monomorphize, so `Box<int>` and `Box<string>` are
            // the same runtime shape. This mirrors the `Named` arm.
            (name, _) if is_nominal(name, nominal_type_names) => value
                .nominal_type_name()
                .is_some_and(|actual| actual == name),
            _ => true,
        },
        TypeExpr::FnType { .. } => value.runtime_type_kind() == Kind::Closure,
        TypeExpr::Never => false,
        TypeExpr::LitString(expected) => value.string_literal() == Some(expected),
        TypeExpr::LitInt(expected) => value.int_literal() == Some(*expected),
        TypeExpr::Owned(inner) => matches_type(value, inner, type_params, nominal_type_names),
    }
}

/// Match a value against the canonical const-friendly type used by builtin
/// and capability manifests.
///
/// Conversion is owned by `harn-parser`; runtimes therefore do not maintain a
/// second interpretation of `Ty` alongside the source-language `TypeExpr`.
pub fn matches_manifest_type<V: TypeContractValue>(value: &V, expected: &Ty) -> bool {
    let expected = harn_parser::builtin_signatures::ty_to_type_expr(expected);
    matches_type(value, &expected, &[], &[])
}

/// Match a runtime value against the closed schema subset emitted by the
/// canonical compiler for typed default parameters.
///
/// This is intentionally narrower than Harn's user-facing schema language:
/// it owns only the compiler-generated `type`, `properties`, `required`,
/// `items`, `additional_properties`, `union`, `all_of`, `enum`, and `const`
/// vocabulary. Both native and portable runtimes can project values through
/// [`TypeContractValue`] without growing a second source-type dispatcher.
pub fn matches_compiler_schema<V: TypeContractValue>(value: &V, schema: &DataValue) -> bool {
    matches_compiler_schema_inner(value, schema, 0)
}

fn matches_compiler_schema_inner<V: TypeContractValue>(
    value: &V,
    schema: &DataValue,
    depth: usize,
) -> bool {
    const MAX_SCHEMA_DEPTH: usize = 256;
    if depth >= MAX_SCHEMA_DEPTH {
        return false;
    }
    let DataValue::Record(fields) = schema else {
        return false;
    };

    if let Some(DataValue::List(branches)) = fields.get("union") {
        return branches
            .iter()
            .any(|branch| matches_compiler_schema_inner(value, branch, depth + 1));
    }
    if let Some(DataValue::List(branches)) = fields.get("all_of") {
        return branches
            .iter()
            .all(|branch| matches_compiler_schema_inner(value, branch, depth + 1));
    }
    if let Some(DataValue::String(expected)) = fields.get("type") {
        use RuntimeTypeKind as Kind;
        let matches = match expected.as_str() {
            "int" => value.runtime_type_kind() == Kind::Int,
            "float" => matches!(value.runtime_type_kind(), Kind::Int | Kind::Float),
            "string" => value.runtime_type_kind() == Kind::String,
            "bool" => value.runtime_type_kind() == Kind::Bool,
            "nil" => value.runtime_type_kind() == Kind::Nil,
            "list" => value.runtime_type_kind() == Kind::List,
            "dict" => value.runtime_type_kind() == Kind::Dict,
            "bytes" => value.runtime_type_kind() == Kind::Bytes,
            "closure" => value.runtime_type_kind() == Kind::Closure,
            // Sets cannot cross the portable data boundary.
            "set" => value.runtime_type_kind() == Kind::Set,
            _ => false,
        };
        if !matches {
            return false;
        }
    }
    if let Some(DataValue::List(allowed)) = fields.get("enum") {
        if !allowed
            .iter()
            .any(|candidate| literal_matches(value, candidate))
        {
            return false;
        }
    }
    if let Some(expected) = fields.get("const") {
        if !literal_matches(value, expected) {
            return false;
        }
    }
    if let Some(DataValue::List(required)) = fields.get("required") {
        if !required.iter().all(|name| match name {
            DataValue::String(name) => value.record_field(name).is_some(),
            _ => false,
        }) {
            return false;
        }
    }
    if let Some(DataValue::Record(properties)) = fields.get("properties") {
        for (name, child_schema) in properties {
            if let Some(child) = value.record_field(name) {
                if !matches_compiler_schema_inner(child, child_schema, depth + 1) {
                    return false;
                }
            }
        }
    }
    if let Some(additional) = fields.get("additional_properties") {
        if value.record_values_match(&mut |child| {
            matches_compiler_schema_inner(child, additional, depth + 1)
        }) != Some(true)
        {
            return false;
        }
    }
    true
}

fn literal_matches<V: TypeContractValue>(value: &V, expected: &DataValue) -> bool {
    match expected {
        DataValue::String(expected) => value.string_literal() == Some(expected),
        DataValue::Int(expected) => value.int_literal() == Some(*expected),
        DataValue::Nil => value.runtime_type_kind() == RuntimeTypeKind::Nil,
        _ => false,
    }
}

/// Return whether a canonical manifest type can cross the portable
/// [`DataValue`] boundary without losing information or type precision.
///
/// The capability registry describes both JSON-shaped host calls and native
/// runtime objects such as channels, streams, closures, schemas, and generic
/// results. Portable execution must reject the latter structurally instead of
/// collapsing them to `any` and pretending the contract is enforceable.
pub fn manifest_type_is_portable(expected: &Ty) -> bool {
    match expected {
        Ty::Any | Ty::LitInt(_) | Ty::LitString(_) => true,
        Ty::Named(name) => matches!(
            *name,
            "any"
                | "unknown"
                | "nil"
                | "bool"
                | "int"
                | "float"
                | "number"
                | "string"
                | "bytes"
                | "list"
                | "dict"
                | "record"
        ),
        Ty::Optional(inner) => manifest_type_is_portable(inner),
        Ty::Apply("list" | "List", [inner]) | Ty::Apply("Option", [inner]) => {
            manifest_type_is_portable(inner)
        }
        Ty::Apply("dict" | "Dict", [key, value]) => {
            manifest_dict_key_is_portable(key) && manifest_type_is_portable(value)
        }
        Ty::Union(members) => !members.is_empty() && members.iter().all(manifest_type_is_portable),
        Ty::Shape(fields) => fields
            .iter()
            .all(|field| manifest_type_is_portable(&field.ty)),
        // An open record is portable when its named fields are. The row tails
        // stand for keys the manifest does not describe, so they are only
        // portable if they are themselves portable container types.
        Ty::OpenShape(fields, rests) => {
            fields
                .iter()
                .all(|field| manifest_type_is_portable(&field.ty))
                && rests.iter().all(manifest_type_is_portable)
        }
        Ty::Generic(_) | Ty::Apply(_, _) | Ty::Fn(_, _) | Ty::SchemaOf(_) | Ty::Never => false,
    }
}

fn manifest_dict_key_is_portable(expected: &Ty) -> bool {
    match expected {
        Ty::Any | Ty::Named("any" | "unknown" | "string") | Ty::LitString(_) => true,
        Ty::Union(members) => {
            !members.is_empty() && members.iter().all(manifest_dict_key_is_portable)
        }
        _ => false,
    }
}

/// Return whether every parameter and successful return value in a canonical
/// capability signature is representable by the portable value contract.
pub fn manifest_signature_is_portable(signature: &BuiltinSignature) -> bool {
    signature
        .params
        .iter()
        .all(|parameter| manifest_type_is_portable(&parameter.ty))
        && manifest_type_is_portable(&signature.returns)
}

fn record_values_match<V: TypeContractValue>(
    value: &V,
    value_type: &TypeExpr,
    type_params: &[String],
    nominal_type_names: &[String],
) -> bool {
    value
        .record_values_match(&mut |field| {
            matches_type(field, value_type, type_params, nominal_type_names)
        })
        .unwrap_or(false)
}

impl TypeContractValue for DataValue {
    fn runtime_type_kind(&self) -> RuntimeTypeKind {
        match self {
            Self::Nil => RuntimeTypeKind::Nil,
            Self::Bool(_) => RuntimeTypeKind::Bool,
            Self::Int(_) => RuntimeTypeKind::Int,
            Self::Float(_) => RuntimeTypeKind::Float,
            Self::String(_) => RuntimeTypeKind::String,
            Self::Bytes(_) => RuntimeTypeKind::Bytes,
            Self::List(_) => RuntimeTypeKind::List,
            Self::Record(_) => RuntimeTypeKind::Dict,
        }
    }

    fn list_items(&self) -> Option<&[Self]> {
        match self {
            Self::List(items) => Some(items),
            _ => None,
        }
    }

    fn record_field(&self, name: &str) -> Option<&Self> {
        match self {
            Self::Record(fields) => fields.get(name),
            _ => None,
        }
    }

    fn record_values_match(&self, predicate: &mut dyn FnMut(&Self) -> bool) -> Option<bool> {
        match self {
            Self::Record(fields) => Some(fields.values().all(predicate)),
            _ => None,
        }
    }

    fn string_literal(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    fn int_literal(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use harn_parser::builtin_signatures::{ShapeFieldDescriptor, Ty};

    use super::*;

    const STRING: Ty = Ty::Named("string");
    const STRING_LIST_ARGS: &[Ty] = &[STRING];
    const RESULT_ARGS: &[Ty] = &[STRING, Ty::Named("dict")];
    const INT_KEYED_DICT_ARGS: &[Ty] = &[Ty::Named("int"), STRING];
    const RECORD_FIELDS: &[ShapeFieldDescriptor] = &[
        ShapeFieldDescriptor::new("name", STRING),
        ShapeFieldDescriptor::optional("note", STRING),
    ];

    #[test]
    fn manifest_types_use_the_source_type_contract() {
        let strings = DataValue::List(vec![DataValue::String("kernel".into())]);
        assert!(matches_manifest_type(
            &strings,
            &Ty::Apply("list", STRING_LIST_ARGS)
        ));
        assert!(!matches_manifest_type(
            &DataValue::List(vec![DataValue::Int(1)]),
            &Ty::Apply("list", STRING_LIST_ARGS)
        ));

        let record = DataValue::Record(std::collections::BTreeMap::from([(
            "name".to_string(),
            DataValue::String("portable".into()),
        )]));
        assert!(matches_manifest_type(&record, &Ty::Shape(RECORD_FIELDS)));
    }

    #[test]
    fn portable_manifest_types_are_exactly_data_value_types() {
        assert!(manifest_type_is_portable(&Ty::Apply(
            "list",
            STRING_LIST_ARGS
        )));
        assert!(manifest_type_is_portable(&Ty::Shape(RECORD_FIELDS)));
        assert!(!manifest_type_is_portable(&Ty::Apply(
            "Result",
            RESULT_ARGS
        )));
        assert!(!manifest_type_is_portable(&Ty::Named("channel")));
        assert!(!manifest_type_is_portable(&Ty::Fn(&[], &STRING)));
        assert!(!manifest_type_is_portable(&Ty::Generic("T")));
        assert!(!manifest_type_is_portable(&Ty::Apply(
            "dict",
            INT_KEYED_DICT_ARGS
        )));
    }

    /// A stand-in for a runtime that has nominal values (enum variants and
    /// struct instances). `DataValue` deliberately has no such variant, so the
    /// nominal arms of the matcher need a value type that reports one.
    struct Nominal(&'static str);

    impl TypeContractValue for Nominal {
        fn runtime_type_kind(&self) -> RuntimeTypeKind {
            RuntimeTypeKind::Enum
        }

        fn nominal_type_name(&self) -> Option<&str> {
            Some(self.0)
        }
    }

    /// A program that declares its own `enum Option<T>` means *that* type, not
    /// the built-in nullable spelling. Before this was distinguished, the
    /// built-in reading rejected every value the user could construct, because
    /// an `Option.Some(1)` is an enum variant rather than nil or an int.
    #[test]
    fn a_user_declared_option_is_matched_nominally() {
        let applied = TypeExpr::Applied {
            name: "Option".to_string(),
            args: vec![TypeExpr::Named("int".to_string())],
        };
        let user_declared = vec!["Option".to_string()];

        assert!(
            matches_type(&Nominal("Option"), &applied, &[], &user_declared),
            "a user-declared Option enum must satisfy its own type"
        );
        assert!(
            !matches_type(&Nominal("Result"), &applied, &[], &user_declared),
            "and must still reject a different nominal type"
        );

        // With no such declaration in scope, `Option<int>` keeps its built-in
        // nullable reading.
        assert!(matches_type(&DataValue::Nil, &applied, &[], &[]));
        assert!(matches_type(&DataValue::Int(1), &applied, &[], &[]));
        assert!(!matches_type(
            &DataValue::String("no".into()),
            &applied,
            &[],
            &[]
        ));
    }
}
