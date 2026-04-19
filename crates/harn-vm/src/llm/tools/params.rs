use std::collections::BTreeMap;
use std::rc::Rc;

use super::components::ComponentRegistry;
use super::json_schema::json_schema_to_type_expr;
use super::type_expr::TypeExpr;
use crate::value::VmValue;

/// Extract parameter info from a Harn VmValue dict (tool_registry entry).
/// Harn tool definitions default to `required: true`; a param is optional only
/// when its dict explicitly contains `required: false`. The per-param dict
/// carries a JSON-Schema-ish subset (type / enum / const / items / properties
/// / oneOf / anyOf / allOf / default / examples / $ref) which we recursively
/// lift into TypeExpr. The `root_json` is the whole tool-registry converted
/// to JSON so `$ref` pointers can resolve against it.
pub(super) fn extract_params_from_vm_dict(
    td: &BTreeMap<String, VmValue>,
    root_json: &serde_json::Value,
    registry: &mut ComponentRegistry,
) -> Vec<ToolParamSchema> {
    let mut params = Vec::new();
    if let Some(VmValue::Dict(pd)) = td.get("parameters") {
        for (pname, pval) in pd.iter() {
            let (ty, desc, required, default, examples) = if let VmValue::Dict(pdef) = pval {
                let desc = pdef
                    .get("description")
                    .map(|value| value.display())
                    .unwrap_or_default();
                let required = match pdef.get("required") {
                    Some(VmValue::Bool(required)) => *required,
                    _ => true,
                };
                let json = vm_dict_to_json(pdef);
                let ty = json_schema_to_type_expr(&json, root_json, registry);
                let default = json.get("default").cloned();
                let examples = extract_examples_vm(pdef);
                (ty, desc, required, default, examples)
            } else {
                // Simple string description — treat as required string.
                (
                    TypeExpr::Primitive("string".to_string()),
                    pval.display(),
                    true,
                    None,
                    Vec::new(),
                )
            };
            params.push(ToolParamSchema {
                name: pname.clone(),
                ty,
                description: desc,
                required,
                default,
                examples,
            });
        }
    }
    // Required params first so the rendered TS signature and any
    // order-dependent consumer see critical fields first.
    params.sort_by_key(|param| !param.required);
    params
}

/// Convert a VmValue dict fragment into a serde_json::Value using the crate's
/// canonical VmValue → JSON conversion (re-exported via `super::vm_value_to_json`
/// at the top of this file). We wrap the dict contents in `VmValue::Dict` so
/// the single shared conversion path handles every field uniformly.
fn vm_dict_to_json(dict: &BTreeMap<String, VmValue>) -> serde_json::Value {
    super::super::vm_value_to_json(&VmValue::Dict(Rc::new(dict.clone())))
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct ToolParamSchema {
    pub(crate) name: String,
    pub(crate) ty: TypeExpr,
    pub(crate) description: String,
    pub(crate) required: bool,
    pub(crate) default: Option<serde_json::Value>,
    /// JSON Schema `examples` (plural) or `example` (singular, legacy). Shown
    /// inline after the description so models see concrete valid values
    /// alongside the type constraint.
    pub(crate) examples: Vec<serde_json::Value>,
}

/// Pull examples from a JSON-schema-ish fragment, accepting both plural
/// `examples: [...]` (OAS 3.1 preferred) and the legacy singular `example: v`.
pub(super) fn extract_examples(
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Vec<serde_json::Value> {
    if let Some(serde_json::Value::Array(arr)) = obj.get("examples") {
        return arr.clone();
    }
    if let Some(single) = obj.get("example") {
        return vec![single.clone()];
    }
    Vec::new()
}

/// Pull examples from a VmValue dict, same dual-key convention.
pub(super) fn extract_examples_vm(pdef: &BTreeMap<String, VmValue>) -> Vec<serde_json::Value> {
    if let Some(VmValue::List(items)) = pdef.get("examples") {
        return items.iter().map(super::super::vm_value_to_json).collect();
    }
    if let Some(single) = pdef.get("example") {
        return vec![super::super::vm_value_to_json(single)];
    }
    Vec::new()
}
