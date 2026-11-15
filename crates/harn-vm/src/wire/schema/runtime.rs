//! Observe typed annotations through the existing schema walk and resolver.

use std::collections::BTreeSet;
use std::sync::Arc;

use referencing::{Draft, Resolver, ResourceRef};
use serde_json::Value;

#[derive(Clone)]
pub(crate) struct RuntimeJsonCodec(Arc<jsonschema::Validator>);

impl std::fmt::Debug for RuntimeJsonCodec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeJsonCodec")
            .finish_non_exhaustive()
    }
}

impl RuntimeJsonCodec {
    pub(super) fn compile(
        schema: &Value,
        registry: &jsonschema::Registry,
        resolver: &Resolver<'_>,
        location: &str,
    ) -> Result<Self, ()> {
        let observed = resolver.lookup(location).map_err(|_| ())?;
        if observed.contents() != schema || observed.resolver().base_uri() != resolver.base_uri() {
            return Err(());
        }
        let base = uuid::Uuid::now_v7().urn().to_string();
        if registry.contains_resource(&base) {
            return Err(());
        }
        // A reference keeps local pointers anchored to the original document.
        let reference = serde_json::json!({"$ref":location});
        crate::tool_registry::compile_runtime_schema(&reference, registry, &base)
            .map(|validator| Self(Arc::new(validator)))
            .map_err(|_| ())
    }

    pub(crate) fn accepts(&self, value: &Value) -> bool {
        self.0.is_valid(value)
    }
}

pub(crate) struct WireAnnotations {
    pub(crate) declared: bool,
    complete: bool,
}

impl WireAnnotations {
    pub(super) fn allows_opaque_json(&self) -> bool {
        self.complete && !self.declared
    }
}

pub(crate) fn wire_annotations(schema: &Value, resolver: Resolver<'_>) -> WireAnnotations {
    let mut result = WireAnnotations {
        declared: false,
        complete: true,
    };
    let mut pending = vec![(schema.clone(), resolver)];
    let mut references = BTreeSet::new();
    while let Some((schema, resolver)) = pending.pop() {
        let walked = crate::tool_registry::visit_schema_with_context(
            &schema,
            (resolver, true),
            &mut |schema, (resolver, already_scoped)| {
                let resolver = if already_scoped {
                    resolver
                } else {
                    resolver.in_subresource(ResourceRef::new(schema, Draft::Draft202012))?
                };
                let Some(object) = schema.as_object() else {
                    return Ok((resolver, false));
                };
                result.declared |= [
                    "x-harn-wire-id",
                    "x-harn-type",
                    "x-harn-nominal",
                    "x-harn-alias",
                ]
                .iter()
                .any(|key| object.contains_key(*key));
                // Unsupported dynamic reference scopes cannot establish an opaque island.
                result.complete &= ![
                    "$dynamicRef",
                    "$dynamicAnchor",
                    "$recursiveRef",
                    "$recursiveAnchor",
                ]
                .iter()
                .any(|key| object.contains_key(*key));
                result.complete &= object.get("$ref").is_none_or(Value::is_string);
                if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                    let absolute =
                        resolver.resolve_uri(&resolver.base_uri().as_ref().borrow(), reference)?;
                    if references.insert(absolute.as_str().to_owned()) {
                        let resolved = resolver.lookup(reference)?;
                        let (schema, scope, _) = resolved.into_inner();
                        pending.push((schema.clone(), scope));
                    }
                }
                Ok::<_, referencing::Error>((resolver, false))
            },
        );
        result.complete &= walked.is_ok();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn annotation_census_resolves_relative_refs_in_their_subresource_scope() {
        let root = json!({"properties":{"metadata":{
            "$id":"nested/schema.json", "allOf":[{"$ref":"types.json"}]
        }}});
        for (target, declared) in [
            (
                Value::Object(crate::value::json::bytes_json_schema(false)),
                true,
            ),
            (json!({"type":"object","additionalProperties":true}), false),
        ] {
            let registry = jsonschema::Registry::new()
                .draft(jsonschema::Draft::Draft202012)
                .extend([
                    ("https://schema.invalid/root.json", root.clone()),
                    ("https://schema.invalid/nested/types.json", target),
                ])
                .unwrap()
                .prepare()
                .unwrap();
            let resolver = registry
                .resolver(jsonschema::uri::from_str("https://schema.invalid/root.json").unwrap());
            let observed = wire_annotations(&root, resolver);
            assert!(
                observed.complete,
                "the registered relative reference was actually observed"
            );
            assert_eq!(observed.declared, declared);
            assert_eq!(observed.allows_opaque_json(), !declared);
        }
    }

    #[test]
    fn unmeasured_reference_scope_cannot_authorize_an_opaque_island() {
        for schema in [
            json!({"allOf":[{"$ref":"missing.json"}]}),
            json!({"$dynamicRef":"#node"}),
            json!({"$ref":7}),
        ] {
            let registry = jsonschema::Registry::new().prepare().unwrap();
            let resolver = registry
                .resolver(jsonschema::uri::from_str("https://schema.invalid/root.json").unwrap());
            let observed = wire_annotations(&schema, resolver);
            assert!(!observed.complete);
            assert!(!observed.allows_opaque_json());
        }
    }
}
