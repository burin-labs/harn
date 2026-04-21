use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

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
    let parsed = parse_shape_spec(spec);
    for (field_name, type_spec, optional) in &parsed {
        match fields.get(field_name.as_str()) {
            None => {
                if !optional {
                    let actual_keys: Vec<&str> = fields.keys().map(|k| k.as_str()).collect();
                    let max_dist = if field_name.len() <= 4 { 1 } else { 2 };
                    let suggestion = find_closest_field(field_name, &actual_keys, max_dist);
                    let msg = if let Some(closest) = suggestion {
                        format!(
                            "parameter '{}': missing field '{}' ({}), did you mean '{}'?",
                            param_name, field_name, type_spec, closest
                        )
                    } else {
                        format!(
                            "parameter '{}': missing field '{}' ({})",
                            param_name, field_name, type_spec
                        )
                    };
                    return Err(VmError::TypeError(msg));
                }
            }
            Some(val) => {
                if type_spec.starts_with('{') && type_spec.ends_with('}') {
                    let inner_spec = &type_spec[1..type_spec.len() - 1];
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
                                field_name,
                                val.type_name()
                            )));
                        }
                    };
                    {
                        let nested_param = format!("{}.{}", param_name, field_name);
                        assert_shape_fields(nested_fields, &nested_param, inner_spec)?;
                    }
                } else if type_spec.contains('|') {
                    let actual_type = val.type_name();
                    let is_nil = matches!(val, VmValue::Nil);
                    let matches = type_spec
                        .split('|')
                        .any(|t| t.trim() == actual_type || (t.trim() == "nil" && is_nil));
                    if !matches {
                        return Err(VmError::TypeError(format!(
                            "parameter '{}': field '{}' expected {}, got {}",
                            param_name, field_name, type_spec, actual_type
                        )));
                    }
                } else {
                    let actual_type = val.type_name();
                    if actual_type != type_spec.as_str() {
                        return Err(VmError::TypeError(format!(
                            "parameter '{}': field '{}' expected {}, got {}",
                            param_name, field_name, type_spec, actual_type
                        )));
                    }
                }
            }
        }
    }
    Ok(VmValue::Nil)
}

/// Compute the Levenshtein edit distance between two strings.
fn edit_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let n = b_chars.len();
    let mut prev = (0..=n).collect::<Vec<_>>();
    let mut curr = vec![0; n + 1];
    for (i, ac) in a_chars.iter().enumerate() {
        curr[0] = i + 1;
        for (j, bc) in b_chars.iter().enumerate() {
            let cost = if ac == bc { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// Find the closest match to `name` among `candidates`, within `max_dist` edits.
fn find_closest_field<'a>(name: &str, candidates: &[&'a str], max_dist: usize) -> Option<&'a str> {
    candidates
        .iter()
        .copied()
        .filter(|c| c.len().abs_diff(name.len()) <= max_dist)
        .min_by_key(|c| edit_distance(name, c))
        .filter(|c| edit_distance(name, c) <= max_dist && *c != name)
}

/// Parse a shape spec string into a list of (field_name, type_spec, optional).
fn parse_shape_spec(spec: &str) -> Vec<(String, String, bool)> {
    let mut result = Vec::new();
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
                '}' => {
                    brace_depth -= 1;
                    i += 1;
                }
                ',' if brace_depth == 0 => break,
                _ => {
                    i += 1;
                }
            }
        }
        let type_spec = chars[type_start..i]
            .iter()
            .collect::<String>()
            .trim()
            .to_string();

        if !field_name.is_empty() && !type_spec.is_empty() {
            result.push((field_name, type_spec, optional));
        }

        if i < len && chars[i] == ',' {
            i += 1;
        }
    }

    result
}
