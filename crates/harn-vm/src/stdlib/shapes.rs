use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_shape_builtins(vm: &mut Vm) {
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

        let fields: Option<&BTreeMap<String, VmValue>> = match &val {
            VmValue::Dict(map) => Some(map.as_ref()),
            VmValue::StructInstance { fields, .. } => Some(fields),
            _ => None,
        };
        let fields = match fields {
            Some(f) => f,
            None => {
                return Err(VmError::TypeError(format!(
                    "parameter '{}': expected dict or struct, got {}",
                    param_name,
                    val.type_name()
                )));
            }
        };

        assert_shape_fields(fields, &param_name, &spec)
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
        match fields_dict {
            VmValue::Dict(d) => Ok(VmValue::StructInstance {
                struct_name,
                fields: (*d).clone(),
            }),
            _ => Ok(VmValue::StructInstance {
                struct_name,
                fields: BTreeMap::new(),
            }),
        }
    });
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
                    return Err(VmError::TypeError(format!(
                        "parameter '{}': missing field '{}' ({})",
                        param_name, field_name, type_spec
                    )));
                }
            }
            Some(val) => {
                if type_spec.starts_with('{') && type_spec.ends_with('}') {
                    let inner_spec = &type_spec[1..type_spec.len() - 1];
                    let nested_fields: Option<&BTreeMap<String, VmValue>> = match val {
                        VmValue::Dict(map) => Some(map.as_ref()),
                        VmValue::StructInstance { fields, .. } => Some(fields),
                        _ => None,
                    };
                    match nested_fields {
                        Some(nf) => {
                            let nested_param = format!("{}.{}", param_name, field_name);
                            assert_shape_fields(nf, &nested_param, inner_spec)?;
                        }
                        None => {
                            return Err(VmError::TypeError(format!(
                                "parameter '{}': field '{}' expected dict or struct, got {}",
                                param_name,
                                field_name,
                                val.type_name()
                            )));
                        }
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
