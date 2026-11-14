//! Closure-preserving inverse of the canonical catalog projection.

use std::collections::{BTreeMap, BTreeSet};

use super::{tool_registry_catalog, ToolCatalog};
use crate::value::{VmClosure, VmError, VmValue};

/// Attach exactly one local closure to each independently prepared catalog
/// entry. Metadata is projected mechanically and checked by the same parser
/// that every discovery and invocation adapter consumes.
pub fn bind_catalog_handlers(
    catalog: ToolCatalog,
    handlers: BTreeMap<String, VmClosure>,
) -> Result<VmValue, VmError> {
    let refused = || VmError::Runtime("catalog handler binding is not exact".into());
    catalog.validate().map_err(|_| refused())?;
    let names = catalog
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<BTreeSet<_>>();
    if names != handlers.keys().map(String::as_str).collect() {
        return Err(refused());
    }
    let mut value = serde_json::to_value(&catalog).map_err(|_| refused())?;
    let registry = value.as_object_mut().ok_or_else(refused)?;
    registry.remove("schema_version");
    registry.insert("_type".into(), "tool_registry".into());
    for tool in registry
        .get_mut("tools")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(refused)?
    {
        let tool = tool.as_object_mut().ok_or_else(refused)?;
        for (source, target) in [
            ("deferLoading", "defer_loading"),
            ("_meta", "meta"),
            ("policy", "execution_policy"),
        ] {
            if let Some(value) = tool.remove(source) {
                tool.insert(target.into(), value);
            }
        }
    }
    let mut registry = crate::schema::json_to_vm_value(&value);
    let VmValue::Dict(fields) = &mut registry else {
        return Err(refused());
    };
    let Some(VmValue::List(entries)) = std::sync::Arc::make_mut(fields).get_mut("tools") else {
        return Err(refused());
    };
    for entry in std::sync::Arc::make_mut(entries) {
        let VmValue::Dict(fields) = entry else {
            return Err(refused());
        };
        let fields = std::sync::Arc::make_mut(fields);
        let Some(VmValue::String(name)) = fields.get("name") else {
            return Err(refused());
        };
        let handler = handlers.get(name.as_str()).ok_or_else(refused)?.clone();
        fields.insert(
            "handler".into(),
            VmValue::Closure(std::sync::Arc::new(handler)),
        );
    }
    // Includes schemas and opaque metadata. In particular, an unsupported JSON
    // number must refuse rather than silently round during VM conversion.
    if tool_registry_catalog(&registry)? != catalog {
        return Err(refused());
    }
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn full_catalog_binding_preserves_metadata_and_reaches_only_the_admitted_handler() {
        let catalog: ToolCatalog = serde_json::from_value(json!({
            "schema_version":"harn-tools/2.0", "info":{"name":"inverse","version":"1"},
            "components":{"schemas":{"Count":{"type":"integer"}}},
            "cli":{"commands":[{"command":["native"],"title":"Native commands"}]},
            "tools":[{
                "name":"native.echo", "title":"Echo", "description":"Return one count.",
                "inputSchema":{"type":"object","properties":{"count":{"$ref":"#/components/schemas/Count"}},"required":["count"],"additionalProperties":false},
                "outputSchema":{"type":"object","properties":{"count":{"type":"integer"}},"required":["count"],"additionalProperties":false},
                "errorSchema":{"type":"string"},
                "annotations":{"readOnlyHint":true,"idempotentHint":true},
                "execution":{"taskSupport":"optional"},
                "governance":{"audiences":["cli","mcp","catalog"]},
                "cli":{"command":["native","echo"],"aliases":[],"hidden":false,"arguments":{}},
                "namespace":"native","deferLoading":true,
                "source":{"kind":"mcp","id":"worker"},
                "policy":{"kind":"read","side_effect_level":"read_only"},
                "_meta":{"retained":{"value":9223372036854775807i64}}
            }]
        })).unwrap();
        let chunk =
            crate::compile_source("fn identity(input: dict) { return input }\nreturn identity")
                .unwrap();
        let mut vm = crate::new_vm();
        let VmValue::Closure(handler) = vm.execute(&chunk).await.unwrap() else {
            panic!("closure fixture not reached")
        };
        let handlers = BTreeMap::from([("native.echo".into(), handler.as_ref().clone())]);
        let registry = bind_catalog_handlers(catalog.clone(), handlers.clone()).unwrap();
        assert_eq!(tool_registry_catalog(&registry).unwrap(), catalog);
        let mut tools =
            super::super::executable_tools_for_audience(&registry, super::super::ToolAudience::Mcp)
                .unwrap();
        assert_eq!(tools.len(), 1);
        let tool = tools.remove(0);
        let prepared = super::super::PreparedToolCatalog::prepare(catalog.clone()).unwrap();
        let input = prepared
            .decode_input("native.echo", &json!({"count":9223372036854775807i64}))
            .unwrap();
        let value = super::super::call_tool_handler(
            &mut vm,
            &tool,
            super::super::ToolAudience::Mcp,
            input.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            super::super::result_to_json(&value).unwrap(),
            json!({"count":9223372036854775807i64})
        );
        assert!(super::super::call_tool_handler(
            &mut vm,
            &tool,
            super::super::ToolAudience::Agent,
            input
        )
        .await
        .is_err());
        assert!(bind_catalog_handlers(catalog.clone(), BTreeMap::new()).is_err());
        let mut extra = handlers.clone();
        extra.insert("unregistered".into(), handler.as_ref().clone());
        assert!(bind_catalog_handlers(catalog.clone(), extra).is_err());
        let mut unrepresentable = catalog;
        unrepresentable.tools[0]
            .meta
            .as_mut()
            .unwrap()
            .insert("outside_vm_integer".into(), json!(u64::MAX));
        assert!(bind_catalog_handlers(unrepresentable, handlers).is_err());
    }
}
