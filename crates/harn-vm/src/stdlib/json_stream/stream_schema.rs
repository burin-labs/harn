use super::*;

/// Send-safe Rust API for the incremental JSON validator. Mirrors the
/// `std/json/stream` script builtin but stores the schema as JSON so the LLM
/// transport can hold it across off-thread awaits without VM heap identity.
pub(crate) struct StreamSchemaValidator {
    schema_json: serde_json::Value,
    buffer: String,
    // Keep scanner state off the stack frames of async transport callers.
    scan: Box<JsonStreamScan>,
    status: JsonStreamStatus,
}

impl StreamSchemaValidator {
    /// Build a validator from a JSON Schema-shaped value.
    pub(crate) fn from_json_schema(schema: &serde_json::Value) -> Result<Self, String> {
        let schema_vm = schema::json_to_vm_value(schema);
        schema::schema_from_json_schema_value(&schema_vm).map_err(|err| err.to_string())?;
        Ok(Self {
            schema_json: schema.clone(),
            buffer: String::new(),
            scan: Box::new(JsonStreamScan::default()),
            status: JsonStreamStatus::Pending,
        })
    }

    /// Feed one text chunk and return the resulting validation status.
    pub(crate) fn feed(&mut self, chunk: &str) -> &JsonStreamStatus {
        if matches!(self.status, JsonStreamStatus::Invalid { .. }) {
            return &self.status;
        }
        if let Err(err) = self.scan.feed(chunk) {
            self.buffer.push_str(chunk);
            self.status = JsonStreamStatus::Invalid {
                reason_kind: SchemaValidationReasonKind::InvalidJson,
                reason: err,
                path: "$".to_string(),
            };
            return &self.status;
        }
        self.buffer.push_str(chunk);

        let json = self.scan.json_slice(&self.buffer);
        if json.trim().is_empty() {
            self.status = JsonStreamStatus::Pending;
            return &self.status;
        }

        let schema_vm = schema::json_to_vm_value(&self.schema_json);
        let canonical = match schema::schema_from_json_schema_value(&schema_vm) {
            Ok(schema) => schema,
            Err(_) => schema_vm,
        };
        if let Some(invalid) = early_invalid(json, &canonical) {
            self.status = JsonStreamStatus::Invalid {
                reason_kind: invalid.reason_kind,
                reason: invalid.reason,
                path: invalid.path,
            };
            return &self.status;
        }

        if self.scan.complete || self.scan.root_scalar {
            self.status = match parse_complete_buffer(json, &canonical) {
                ParseOutcome::Valid => {
                    self.scan.complete = true;
                    JsonStreamStatus::Valid
                }
                ParseOutcome::Pending => JsonStreamStatus::Pending,
                ParseOutcome::Invalid {
                    reason_kind,
                    reason,
                    path,
                } => JsonStreamStatus::Invalid {
                    reason_kind,
                    reason,
                    path,
                },
            };
        } else {
            self.status = JsonStreamStatus::Pending;
        }
        &self.status
    }
}
