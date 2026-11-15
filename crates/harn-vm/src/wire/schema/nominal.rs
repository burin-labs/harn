use super::*;
use crate::wire::metadata::{FieldOrigin, VariantOrigin};

impl SchemaWireBuilder<'_> {
    pub(super) fn field_origin(
        &self,
        schema: &Value,
        parent: &WireTypeId,
        name: &str,
        location: &str,
    ) -> Result<FieldOrigin, WireFailure> {
        let origin: FieldOrigin = match schema.get("x-harn-field") {
            Some(value) => serde_json::from_value(value.clone())
                .map_err(|_| error(parent, location, WireFailureCode::InvalidContract))?,
            None => FieldOrigin {
                id: format!("{}:field:{name}", parent.0),
                name: name.into(),
                source: source(location),
                documentation: schema
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            },
        };
        if origin.name != name {
            return Err(error(parent, location, WireFailureCode::InvalidContract));
        }
        Ok(origin)
    }

    pub(super) fn nominal_identity(
        &mut self,
        value: &Value,
        resolver: Resolver<'_>,
        id: &WireTypeId,
        location: &str,
    ) -> Result<WireDeclarationIdentity, WireFailure> {
        let mut declaration: WireDeclarationIdentity = serde_json::from_value(value.clone())
            .map_err(|_| error(id, location, WireFailureCode::InvalidContract))?;
        let mut arguments = Vec::new();
        for argument in &declaration.arguments {
            let reference = format!("#/$defs/{}", escape(&argument.0));
            let target = resolver
                .lookup(&reference)
                .map_err(|_| error(id, location, WireFailureCode::UnresolvedReference))?;
            let absolute = resolver
                .resolve_uri(&resolver.base_uri().as_ref().borrow(), &reference)
                .map_err(|_| error(id, location, WireFailureCode::UnresolvedReference))?;
            let (schema, scope, _) = target.into_inner();
            arguments.push(self.lower(schema, scope, absolute.as_str())?);
        }
        declaration.arguments = arguments;
        Ok(declaration)
    }

    pub(super) fn nominal_enum(
        &mut self,
        schema: &Value,
        nominal: &Value,
        resolver: Resolver<'_>,
        id: &WireTypeId,
        location: &str,
    ) -> Result<WireShape, WireFailure> {
        let nominal = self.nominal_identity(nominal, resolver.clone(), id, location)?;
        let branches = schema
            .get("oneOf")
            .and_then(Value::as_array)
            .ok_or_else(|| error(id, location, WireFailureCode::InvalidContract))?;
        let mut variants = Vec::new();
        for (index, branch) in branches.iter().enumerate() {
            let here = child(location, &format!("oneOf/{index}"));
            let origin: VariantOrigin = branch
                .get("x-harn-variant")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| error(id, &here, WireFailureCode::InvalidContract))?
                .ok_or_else(|| error(id, &here, WireFailureCode::InvalidContract))?;
            let properties = branch
                .get("properties")
                .and_then(Value::as_object)
                .ok_or_else(|| error(id, &here, WireFailureCode::InvalidContract))?;
            let required = branch
                .get("required")
                .and_then(Value::as_array)
                .ok_or_else(|| error(id, &here, WireFailureCode::InvalidContract))?;
            if branch.get("type").and_then(Value::as_str) != Some("object")
                || branch.get("additionalProperties") != Some(&Value::Bool(false))
                || properties.len() != 3
                || required.len() != 3
                || ["enum", "variant", "fields"]
                    .iter()
                    .any(|name| !required.contains(&Value::String((*name).into())))
                || properties
                    .get("enum")
                    .and_then(|value| value.get("const"))
                    .and_then(Value::as_str)
                    != Some(nominal.name.as_str())
                || properties
                    .get("variant")
                    .and_then(|value| value.get("const"))
                    .and_then(Value::as_str)
                    != Some(origin.name.as_str())
            {
                return Err(error(id, &here, WireFailureCode::InvalidContract));
            }
            let sequence = &properties["fields"];
            let items = fixed_tuple_items(sequence)
                .ok_or_else(|| error(id, &here, WireFailureCode::InvalidContract))?;
            let mut fields = Vec::new();
            for (position, item) in items.iter().enumerate() {
                let field_location =
                    child(&here, &format!("properties/fields/prefixItems/{position}"));
                let origin: FieldOrigin = item
                    .get("x-harn-field")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|_| error(id, &field_location, WireFailureCode::InvalidContract))?
                    .ok_or_else(|| error(id, &field_location, WireFailureCode::InvalidContract))?;
                fields.push(WireField {
                    id: origin.id,
                    name: origin.name,
                    type_id: self.lower_child(item, resolver.clone(), &field_location)?,
                    optional: false,
                    source: origin.source,
                    documentation: origin.documentation,
                });
            }
            variants.push(WireEnumVariant {
                id: origin.id,
                name: origin.name,
                fields,
                source: origin.source,
                documentation: origin.documentation,
            });
        }
        Ok(WireShape::Enum { nominal, variants })
    }
}
