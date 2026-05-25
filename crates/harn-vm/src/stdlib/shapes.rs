use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use crate::runtime_limits::RuntimeLimits;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const SHAPE_SPEC_CACHE_LIMIT: usize = RuntimeLimits::DEFAULT.max_shape_spec_cache_entries;
const SHAPE_VALIDATION_MAX_DEPTH: usize = RuntimeLimits::DEFAULT.max_shape_validation_depth;

thread_local! {
    static SHAPE_SPEC_CACHE: RefCell<HashMap<String, Rc<ParsedShapeSpec>>> =
        RefCell::new(HashMap::new());
}

pub(crate) fn register_shape_builtins(vm: &mut Vm) {
    vm.register_builtin("keys", |args, _out| {
        match args.first().cloned().unwrap_or(VmValue::Nil) {
            VmValue::Dict(map) => Ok(VmValue::List(Rc::new(
                map.keys()
                    .map(|k| VmValue::String(Rc::from(k.as_str())))
                    .collect(),
            ))),
            _ => Ok(VmValue::List(Rc::new(Vec::new()))),
        }
    });

    vm.register_builtin("values", |args, _out| {
        match args.first().cloned().unwrap_or(VmValue::Nil) {
            VmValue::Dict(map) => Ok(VmValue::List(Rc::new(map.values().cloned().collect()))),
            _ => Ok(VmValue::List(Rc::new(Vec::new()))),
        }
    });

    vm.register_builtin("entries", |args, _out| {
        match args.first().cloned().unwrap_or(VmValue::Nil) {
            VmValue::Dict(map) => Ok(VmValue::List(Rc::new(
                map.iter()
                    .map(|(k, v)| {
                        VmValue::Dict(Rc::new(BTreeMap::from([
                            ("key".to_string(), VmValue::String(Rc::from(k.as_str()))),
                            ("value".to_string(), v.clone()),
                        ])))
                    })
                    .collect(),
            ))),
            _ => Ok(VmValue::List(Rc::new(Vec::new()))),
        }
    });

    // Runtime interface enforcement. Args: value, param_name, interface_name,
    // method_names_csv.
    vm.register_builtin("__assert_interface", |args, _out| {
        let val = args.first().cloned().unwrap_or(VmValue::Nil);
        let param_name = args.get(1).map(|a| a.display()).unwrap_or_default();
        let iface_name = args.get(2).map(|a| a.display()).unwrap_or_default();
        let methods_csv = args.get(3).map(|a| a.display()).unwrap_or_default();

        let struct_name = match &val {
            VmValue::StructInstance { layout, .. } => layout.struct_name().to_string(),
            _ => {
                return Err(VmError::TypeError(format!(
                    "parameter '{}': expected value satisfying interface '{}', got {}",
                    param_name,
                    iface_name,
                    val.type_name()
                )));
            }
        };

        // Compiler already checks method satisfaction; this runtime guard only
        // ensures the value is a struct instance so the VM dispatcher has
        // something to look up (interfaces only apply to structs with impl blocks).
        if methods_csv.is_empty() {
            return Ok(VmValue::Nil);
        }

        let _ = struct_name;
        Ok(VmValue::Nil)
    });

    vm.register_builtin("__assert_dict", |args, _out| {
        let val = args.first().cloned().unwrap_or(VmValue::Nil);
        if matches!(val, VmValue::Dict(_)) {
            Ok(VmValue::Nil)
        } else {
            Err(VmError::TypeError(format!(
                "cannot destructure {} with {{...}} pattern — expected dict",
                val.type_name()
            )))
        }
    });

    vm.register_builtin("__assert_list", |args, _out| {
        let val = args.first().cloned().unwrap_or(VmValue::Nil);
        if matches!(val, VmValue::List(_)) {
            Ok(VmValue::Nil)
        } else {
            Err(VmError::TypeError(format!(
                "cannot destructure {} with [...] pattern — expected list",
                val.type_name()
            )))
        }
    });

    vm.register_builtin("__assert_shape", |args, _out| {
        let val = args.first().cloned().unwrap_or(VmValue::Nil);
        let param_name = match args.get(1) {
            Some(VmValue::String(s)) => s.to_string(),
            _ => "value".to_string(),
        };
        let spec = match args.get(2) {
            Some(VmValue::String(s)) => s.to_string(),
            _ => return Ok(VmValue::Nil),
        };

        let struct_fields;
        let fields = match &val {
            VmValue::Dict(map) => map.as_ref(),
            VmValue::StructInstance { .. } => {
                struct_fields = val.struct_fields_map().unwrap_or_default();
                &struct_fields
            }
            _ => {
                return Err(VmError::TypeError(format!(
                    "parameter '{}': expected dict or struct, got {}",
                    param_name,
                    val.type_name()
                )));
            }
        };

        assert_shape_fields(fields, &param_name, &spec)
    });

    vm.register_builtin("__assert_schema", |args, _out| {
        let val = args.first().cloned().unwrap_or(VmValue::Nil);
        let param_name = match args.get(1) {
            Some(VmValue::String(s)) => s.to_string(),
            _ => "value".to_string(),
        };
        let schema = args.get(2).cloned().unwrap_or(VmValue::Nil);
        crate::schema::schema_assert_param(&val, &param_name, &schema)?;
        Ok(VmValue::Nil)
    });

    vm.register_builtin("__dict_rest", |args, _out| {
        let dict = args.first().cloned().unwrap_or(VmValue::Nil);
        let keys_list = args.get(1).cloned().unwrap_or(VmValue::Nil);
        if let VmValue::Dict(map) = dict {
            let exclude: std::collections::HashSet<String> = match keys_list {
                VmValue::List(items) => items
                    .iter()
                    .filter_map(|v| {
                        if let VmValue::String(s) = v {
                            Some(s.to_string())
                        } else {
                            None
                        }
                    })
                    .collect(),
                _ => std::collections::HashSet::new(),
            };
            let rest: BTreeMap<String, VmValue> = map
                .iter()
                .filter(|(k, _)| !exclude.contains(k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            Ok(VmValue::Dict(Rc::new(rest)))
        } else {
            Ok(VmValue::Nil)
        }
    });

    vm.register_builtin("__make_struct", |args, _out| {
        let struct_name = args.first().map(|a| a.display()).unwrap_or_default();
        let fields_dict = args.get(1).cloned().unwrap_or(VmValue::Nil);
        let layout_fields = args.get(2).and_then(field_names_from_value);
        match fields_dict {
            VmValue::Dict(d) => match layout_fields {
                Some(field_names) => Ok(VmValue::struct_instance_with_layout(
                    struct_name,
                    field_names,
                    (*d).clone(),
                )),
                None => Ok(VmValue::struct_instance_from_map(struct_name, (*d).clone())),
            },
            _ => match layout_fields {
                Some(field_names) => Ok(VmValue::struct_instance_with_layout(
                    struct_name,
                    field_names,
                    BTreeMap::new(),
                )),
                None => Ok(VmValue::struct_instance_from_map(
                    struct_name,
                    BTreeMap::new(),
                )),
            },
        }
    });
}

fn field_names_from_value(value: &VmValue) -> Option<Vec<String>> {
    let VmValue::List(items) = value else {
        return None;
    };

    Some(
        items
            .iter()
            .filter_map(|item| match item {
                VmValue::String(name) => Some(name.to_string()),
                _ => None,
            })
            .collect(),
    )
}

/// Parse a shape spec string and validate fields against it.
fn assert_shape_fields(
    fields: &BTreeMap<String, VmValue>,
    param_name: &str,
    spec: &str,
) -> Result<VmValue, VmError> {
    let parsed = cached_shape_spec(spec);
    assert_parsed_shape_fields(fields, param_name, &parsed)
}

fn assert_parsed_shape_fields(
    fields: &BTreeMap<String, VmValue>,
    param_name: &str,
    spec: &ParsedShapeSpec,
) -> Result<VmValue, VmError> {
    assert_parsed_shape_fields_at_depth(fields, param_name, spec, 0)
}

fn assert_parsed_shape_fields_at_depth(
    fields: &BTreeMap<String, VmValue>,
    param_name: &str,
    spec: &ParsedShapeSpec,
    depth: usize,
) -> Result<VmValue, VmError> {
    for field in &spec.fields {
        match fields.get(field.name.as_str()) {
            None => {
                if !field.optional {
                    let actual_keys: Vec<&str> = fields.keys().map(|k| k.as_str()).collect();
                    let max_dist = if field.name.len() <= 4 { 1 } else { 2 };
                    let suggestion = harn_parser::diagnostic::find_closest_match(
                        &field.name,
                        actual_keys.iter().copied(),
                        max_dist,
                    );
                    let actual_summary = format_available_fields(&actual_keys);
                    let msg = if let Some(closest) = suggestion {
                        format!(
                            "parameter '{}': missing field '{}' ({}), did you mean '{}'? — {}",
                            param_name, field.name, field.type_label, closest, actual_summary
                        )
                    } else {
                        format!(
                            "parameter '{}': missing field '{}' ({}) — {}",
                            param_name, field.name, field.type_label, actual_summary
                        )
                    };
                    return Err(VmError::TypeError(msg));
                }
            }
            Some(val) => {
                assert_shape_field_value(val, param_name, field, depth)?;
            }
        }
    }
    Ok(VmValue::Nil)
}

fn assert_shape_field_value(
    val: &VmValue,
    param_name: &str,
    field: &ParsedShapeField,
    depth: usize,
) -> Result<(), VmError> {
    match &field.kind {
        ShapeFieldType::Nested(nested) => {
            let nested_struct_fields;
            let nested_fields = match val {
                VmValue::Dict(map) => map.as_ref(),
                VmValue::StructInstance { .. } => {
                    nested_struct_fields = val.struct_fields_map().unwrap_or_default();
                    &nested_struct_fields
                }
                _ => {
                    return Err(VmError::TypeError(format!(
                        "parameter '{}': field '{}' expected dict or struct, got {}",
                        param_name,
                        field.name,
                        val.type_name()
                    )));
                }
            };
            let nested_param = format!("{}.{}", param_name, field.name);
            if depth >= SHAPE_VALIDATION_MAX_DEPTH {
                return Err(shape_depth_error(&nested_param));
            }
            assert_parsed_shape_fields_at_depth(nested_fields, &nested_param, nested, depth + 1)?;
            Ok(())
        }
        ShapeFieldType::DepthExceeded => {
            let nested_param = format!("{}.{}", param_name, field.name);
            Err(shape_depth_error(&nested_param))
        }
        ShapeFieldType::Union(types) => {
            let actual_type = val.type_name();
            let is_nil = matches!(val, VmValue::Nil);
            let matches = types
                .iter()
                .any(|ty| ty == actual_type || (ty == "nil" && is_nil));
            if !matches {
                return Err(VmError::TypeError(format!(
                    "parameter '{}': field '{}' expected {}, got {}",
                    param_name, field.name, field.type_label, actual_type
                )));
            }
            Ok(())
        }
        ShapeFieldType::Scalar(expected) => {
            let actual_type = val.type_name();
            if actual_type != expected.as_str() {
                return Err(VmError::TypeError(format!(
                    "parameter '{}': field '{}' expected {}, got {}",
                    param_name, field.name, field.type_label, actual_type
                )));
            }
            Ok(())
        }
    }
}

fn shape_depth_error(param_name: &str) -> VmError {
    VmError::TypeError(format!(
        "parameter '{param_name}': shape validation depth exceeded ({SHAPE_VALIDATION_MAX_DEPTH} levels)"
    ))
}

/// Render the "available fields: …" tail used in missing-field errors.
/// Matches the wording the typechecker emits at `harn check` time so
/// authors see the same phrasing whether the failure surfaces at static
/// analysis or runtime shape validation.
pub(crate) fn format_available_fields(keys: &[&str]) -> String {
    if keys.is_empty() {
        "no fields present".to_string()
    } else {
        format!("available fields: {}", keys.join(", "))
    }
}

#[derive(Debug)]
struct ParsedShapeSpec {
    fields: Vec<ParsedShapeField>,
}

#[derive(Debug)]
struct ParsedShapeField {
    name: String,
    type_label: String,
    optional: bool,
    kind: ShapeFieldType,
}

#[derive(Debug)]
enum ShapeFieldType {
    Scalar(String),
    Union(Vec<String>),
    Nested(Rc<ParsedShapeSpec>),
    DepthExceeded,
}

fn cached_shape_spec(spec: &str) -> Rc<ParsedShapeSpec> {
    SHAPE_SPEC_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(parsed) = cache.get(spec) {
            return Rc::clone(parsed);
        }

        let parsed = Rc::new(parse_shape_spec(spec));
        if cache.len() >= SHAPE_SPEC_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(spec.to_string(), Rc::clone(&parsed));
        parsed
    })
}

/// Parse a shape spec string into reusable runtime validation metadata.
fn parse_shape_spec(spec: &str) -> ParsedShapeSpec {
    parse_shape_spec_at_depth(spec, 0)
}

fn parse_shape_spec_at_depth(spec: &str, depth: usize) -> ParsedShapeSpec {
    let mut fields = Vec::new();
    let chars: Vec<char> = spec.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        while i < len && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= len {
            break;
        }

        let name_start = i;
        while i < len && chars[i] != ':' {
            i += 1;
        }
        if i >= len {
            break;
        }
        let field_name = chars[name_start..i]
            .iter()
            .collect::<String>()
            .trim()
            .to_string();
        i += 1;

        while i < len && chars[i].is_whitespace() {
            i += 1;
        }

        let optional = if i < len && chars[i] == '?' {
            i += 1;
            true
        } else {
            false
        };

        let type_start = i;
        let mut brace_depth = 0;
        while i < len {
            match chars[i] {
                '{' => {
                    brace_depth += 1;
                    i += 1;
                }
                '}' if brace_depth > 0 => {
                    brace_depth -= 1;
                    i += 1;
                }
                '}' => break,
                ',' if brace_depth == 0 => break,
                _ => {
                    i += 1;
                }
            }
        }
        let type_label = chars[type_start..i]
            .iter()
            .collect::<String>()
            .trim()
            .to_string();

        if !field_name.is_empty() && !type_label.is_empty() {
            fields.push(ParsedShapeField {
                name: field_name,
                kind: parse_shape_field_type(&type_label, depth),
                type_label,
                optional,
            });
        }

        if i < len && chars[i] == ',' {
            i += 1;
        }
    }

    ParsedShapeSpec { fields }
}

fn parse_shape_field_type(type_label: &str, depth: usize) -> ShapeFieldType {
    if type_label.starts_with('{') && type_label.ends_with('}') {
        if depth >= SHAPE_VALIDATION_MAX_DEPTH {
            return ShapeFieldType::DepthExceeded;
        }
        let inner_spec = &type_label[1..type_label.len() - 1];
        return ShapeFieldType::Nested(Rc::new(parse_shape_spec_at_depth(inner_spec, depth + 1)));
    }

    if type_label.contains('|') {
        return ShapeFieldType::Union(
            type_label
                .split('|')
                .map(str::trim)
                .filter(|ty| !ty.is_empty())
                .map(str::to_string)
                .collect(),
        );
    }

    ShapeFieldType::Scalar(type_label.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_spec_cache_reuses_parsed_specs() {
        let first = cached_shape_spec("cache_unique_x: int, cache_unique_y?: string");
        let second = cached_shape_spec("cache_unique_x: int, cache_unique_y?: string");

        assert!(Rc::ptr_eq(&first, &second));
    }

    #[test]
    fn parsed_shape_spec_validates_nested_and_union_fields() {
        let spec = cached_shape_spec("user: {name: string}, mode: string|nil");
        let user = VmValue::Dict(Rc::new(BTreeMap::from([(
            "name".to_string(),
            VmValue::String(Rc::from("Ada")),
        )])));
        let fields = BTreeMap::from([
            ("user".to_string(), user),
            ("mode".to_string(), VmValue::Nil),
        ]);

        assert_parsed_shape_fields(&fields, "payload", &spec).unwrap();
    }

    fn nested_shape_spec(depth: usize) -> String {
        let mut spec = "leaf: int".to_string();
        for _ in 0..depth {
            spec = format!("child: {{{spec}}}");
        }
        spec
    }

    fn nested_fields(depth: usize) -> BTreeMap<String, VmValue> {
        let mut fields = BTreeMap::from([("leaf".to_string(), VmValue::Int(1))]);
        for _ in 0..depth {
            fields = BTreeMap::from([("child".to_string(), VmValue::Dict(Rc::new(fields)))]);
        }
        fields
    }

    #[test]
    fn shape_validation_allows_normal_nested_specs() {
        let fields = nested_fields(3);
        let spec = nested_shape_spec(3);

        assert!(assert_shape_fields(&fields, "payload", &spec).is_ok());
    }

    #[test]
    fn shape_validation_reports_depth_limit_with_field_path() {
        let depth = SHAPE_VALIDATION_MAX_DEPTH + 1;
        let fields = nested_fields(depth);
        let spec = nested_shape_spec(depth);

        let err = assert_shape_fields(&fields, "payload", &spec).expect_err("depth limit");
        let message = err.to_string();
        assert!(message.contains("shape validation depth exceeded"));
        assert!(message.contains(&format!("({SHAPE_VALIDATION_MAX_DEPTH} levels)")));
        assert!(message.contains("payload.child"));
    }
}
