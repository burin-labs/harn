use std::collections::BTreeMap;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_shape_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(sig = "keys(dict: dict) -> list", category = "shapes")]
fn keys_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first().cloned().unwrap_or(VmValue::Nil) {
        VmValue::Dict(map) => Ok(VmValue::List(std::sync::Arc::new(
            map.keys()
                .map(|k| VmValue::String(std::sync::Arc::from(k.as_str())))
                .collect(),
        ))),
        _ => Ok(VmValue::List(std::sync::Arc::new(Vec::new()))),
    }
}

#[harn_builtin(sig = "values(...args: any) -> list", category = "shapes")]
fn values_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first().cloned().unwrap_or(VmValue::Nil) {
        VmValue::Dict(map) => Ok(VmValue::List(std::sync::Arc::new(
            map.values().cloned().collect(),
        ))),
        _ => Ok(VmValue::List(std::sync::Arc::new(Vec::new()))),
    }
}

#[harn_builtin(sig = "entries(dict: dict) -> list", category = "shapes")]
fn entries_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first().cloned().unwrap_or(VmValue::Nil) {
        VmValue::Dict(map) => Ok(VmValue::List(std::sync::Arc::new(
            map.iter()
                .map(|(k, v)| {
                    VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
                        (
                            "key".to_string(),
                            VmValue::String(std::sync::Arc::from(k.as_str())),
                        ),
                        ("value".to_string(), v.clone()),
                    ])))
                })
                .collect(),
        ))),
        _ => Ok(VmValue::List(std::sync::Arc::new(Vec::new()))),
    }
}

// Runtime interface enforcement. Args: value, param_name, interface_name,
// method_names_csv.
#[harn_builtin(runtime_only = true, category = "shapes")]
fn __assert_interface(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
}

#[harn_builtin(runtime_only = true, category = "shapes")]
fn __assert_dict(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().cloned().unwrap_or(VmValue::Nil);
    if matches!(val, VmValue::Dict(_)) {
        Ok(VmValue::Nil)
    } else {
        Err(VmError::TypeError(format!(
            "cannot destructure {} with {{...}} pattern — expected dict",
            val.type_name()
        )))
    }
}

#[harn_builtin(runtime_only = true, category = "shapes")]
fn __assert_list(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().cloned().unwrap_or(VmValue::Nil);
    if matches!(val, VmValue::List(_)) {
        Ok(VmValue::Nil)
    } else {
        Err(VmError::TypeError(format!(
            "cannot destructure {} with [...] pattern — expected list",
            val.type_name()
        )))
    }
}

#[harn_builtin(runtime_only = true, category = "shapes")]
fn __assert_schema(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().cloned().unwrap_or(VmValue::Nil);
    let param_name = match args.get(1) {
        Some(VmValue::String(s)) => s.to_string(),
        _ => "value".to_string(),
    };
    let schema = args.get(2).cloned().unwrap_or(VmValue::Nil);
    crate::schema::schema_assert_param(&val, &param_name, &schema)?;
    Ok(VmValue::Nil)
}

#[harn_builtin(runtime_only = true, category = "shapes")]
fn __dict_rest(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
        Ok(VmValue::Dict(std::sync::Arc::new(rest)))
    } else {
        Ok(VmValue::Nil)
    }
}

#[harn_builtin(runtime_only = true, category = "shapes")]
fn __make_struct(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &KEYS_IMPL_DEF,
    &VALUES_IMPL_DEF,
    &ENTRIES_IMPL_DEF,
    &__ASSERT_INTERFACE_DEF,
    &__ASSERT_DICT_DEF,
    &__ASSERT_LIST_DEF,
    &__ASSERT_SCHEMA_DEF,
    &__DICT_REST_DEF,
    &__MAKE_STRUCT_DEF,
];

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
