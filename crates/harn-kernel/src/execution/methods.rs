use super::*;

impl Machine<'_> {
    pub(super) fn call_method(
        &mut self,
        receiver: RuntimeValue,
        method: &str,
        args: Vec<RuntimeValue>,
    ) -> OpStep {
        if let Err(diagnostic) = self.charge_call_validation(&args) {
            return OpStep::Error(diagnostic);
        }
        if let RuntimeValue::Harness(capability) = receiver {
            let capability = if capability == "root" {
                "root".to_string()
            } else {
                capability
            };
            let Some(contract) =
                harn_capability_contracts::capability_method_entry(&capability, method)
            else {
                return OpStep::Error(diagnostic(
                    "unsupported_capability",
                    format!("capability `{capability}.{method}` is not in the canonical registry"),
                ));
            };
            if !manifest_signature_is_portable(contract.signature) {
                return OpStep::Error(diagnostic(
                    "unsupported_portable_capability_type",
                    format!(
                        "capability `{capability}.{method}` uses a type outside the portable value contract"
                    ),
                ));
            }
            let required = contract
                .signature
                .params
                .iter()
                .filter(|parameter| !parameter.optional)
                .count();
            let maximum = (!contract.signature.has_rest).then_some(contract.signature.params.len());
            if args.len() < required || maximum.is_some_and(|maximum| args.len() > maximum) {
                return OpStep::Error(diagnostic(
                    "capability_arguments",
                    format!(
                        "capability `{capability}.{method}` expected {}..{} arguments, got {}",
                        required,
                        maximum.map_or_else(|| "unbounded".to_string(), |value| value.to_string()),
                        args.len()
                    ),
                ));
            }
            if !self.grants.allows(&capability, method) {
                return OpStep::Error(diagnostic(
                    "capability_denied",
                    format!("capability `{capability}.{method}` was not granted"),
                ));
            }
            let argument_values = match args
                .into_iter()
                .map(DataValue::try_from)
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(arguments) => DataValue::List(arguments),
                Err(diagnostic) => return OpStep::Error(diagnostic),
            };
            let DataValue::List(argument_items) = &argument_values else {
                unreachable!("capability arguments are constructed as a list")
            };
            for (index, value) in argument_items.iter().enumerate() {
                let parameter = contract
                    .signature
                    .params
                    .get(index)
                    .or_else(|| {
                        contract
                            .signature
                            .has_rest
                            .then(|| contract.signature.params.last())
                            .flatten()
                    })
                    .expect("arity validation guarantees a parameter contract");
                let omitted_sentinel = parameter.optional && matches!(value, DataValue::Nil);
                if !omitted_sentinel && !matches_manifest_type(value, &parameter.ty) {
                    return OpStep::Error(diagnostic(
                        "capability_argument_type",
                        format!(
                            "capability `{capability}.{method}` argument `{}` is {}, expected {}",
                            parameter.name,
                            value_kind(value),
                            parameter.ty
                        ),
                    ));
                }
            }
            let arguments = argument_values;
            if let Err(diagnostic) = arguments.validate() {
                return OpStep::Error(diagnostic);
            }
            let id = request_id(
                self.program.digest(),
                self.request_ordinal,
                &capability,
                method,
                &arguments,
            );
            self.request_ordinal += 1;
            let expected = ValueShape::from_type(contract.signature.returns);
            let request = CapabilityRequest {
                id,
                capability,
                operation: method.to_string(),
                arguments,
                expected: expected.clone(),
            };
            if let Some(response) = self.responses.get(self.response_cursor).cloned() {
                if response.request_id() != request.id {
                    return OpStep::Error(diagnostic(
                        "capability_replay_mismatch",
                        "recorded capability response does not match deterministic request",
                    ));
                }
                self.response_cursor += 1;
                return match response {
                    CapabilityResult::Ok { value, .. }
                        if matches_manifest_type(&value, &contract.signature.returns) =>
                    {
                        let value = RuntimeValue::from(value);
                        match self.charge_value_work(&value) {
                            Ok(()) => OpStep::Push(value),
                            Err(diagnostic) => OpStep::Error(diagnostic),
                        }
                    }
                    CapabilityResult::Ok { value, .. } => OpStep::Error(diagnostic(
                        "capability_result_type",
                        format!(
                            "capability `{}` returned {}, expected {expected:?}",
                            request.operation,
                            value_kind(&value)
                        ),
                    )),
                    CapabilityResult::Err { code, message, .. } => {
                        let value = RuntimeValue::Record(Rc::new(BTreeMap::from([
                            ("code".to_string(), RuntimeValue::String(Arc::from(code))),
                            (
                                "message".to_string(),
                                RuntimeValue::String(Arc::from(message)),
                            ),
                        ])));
                        match self.charge_value_work(&value) {
                            Ok(()) => OpStep::Throw(value),
                            Err(diagnostic) => OpStep::Error(diagnostic),
                        }
                    }
                };
            }
            return OpStep::Suspend(request);
        }
        if let RuntimeValue::Record(values) = &receiver {
            if let Some(callable) = values.get(method).cloned() {
                return match callable {
                    RuntimeValue::Closure(closure) => OpStep::Call(closure, args, false),
                    RuntimeValue::Builtin(name) => self.call_builtin(&name, args),
                    _ => OpStep::Error(diagnostic(
                        "not_callable",
                        format!("record member `{method}` is not callable"),
                    )),
                };
            }
        }
        match (receiver, method, args.as_slice()) {
            (RuntimeValue::List(values), "count" | "len", []) => {
                OpStep::Push(RuntimeValue::Int(values.len() as i64))
            }
            (RuntimeValue::List(values), "empty", []) => {
                OpStep::Push(RuntimeValue::Bool(values.is_empty()))
            }
            (RuntimeValue::List(values), "contains" | "includes", [value]) => {
                for item in values.iter() {
                    match self.values_equal(item, value) {
                        Ok(true) => return OpStep::Push(RuntimeValue::Bool(true)),
                        Ok(false) => {}
                        Err(diagnostic) => return OpStep::Error(diagnostic),
                    }
                }
                OpStep::Push(RuntimeValue::Bool(false))
            }
            (RuntimeValue::List(values), "appending" | "append", [value]) => {
                let mut appended = Rc::unwrap_or_clone(values);
                appended.push(value.clone());
                self.push_charged(RuntimeValue::List(Rc::new(appended)))
            }
            (RuntimeValue::String(value), "count" | "len", []) => {
                OpStep::Push(RuntimeValue::Int(value.chars().count() as i64))
            }
            (RuntimeValue::String(value), "empty", []) => {
                OpStep::Push(RuntimeValue::Bool(value.is_empty()))
            }
            (RuntimeValue::String(value), "contains", [RuntimeValue::String(needle)]) => {
                OpStep::Push(RuntimeValue::Bool(value.contains(needle.as_ref())))
            }
            (RuntimeValue::String(value), "trim", []) => self.push_charged(RuntimeValue::String(
                Arc::from(crate::pure::trim_text(&value)),
            )),
            (RuntimeValue::String(value), "starts_with", [prefix]) => {
                match self.render_value(prefix) {
                    Ok(prefix) => OpStep::Push(RuntimeValue::Bool(crate::pure::starts_with_text(
                        &value, &prefix,
                    ))),
                    Err(diagnostic) => OpStep::Error(diagnostic),
                }
            }
            (RuntimeValue::String(value), "ends_with", [suffix]) => match self.render_value(suffix)
            {
                Ok(suffix) => OpStep::Push(RuntimeValue::Bool(crate::pure::ends_with_text(
                    &value, &suffix,
                ))),
                Err(diagnostic) => OpStep::Error(diagnostic),
            },
            (RuntimeValue::String(value), "replace", [old, new]) => {
                match (self.render_value(old), self.render_value(new)) {
                    (Ok(old), Ok(new)) => self.push_charged(RuntimeValue::String(Arc::from(
                        crate::pure::replace_text(&value, &old, &new),
                    ))),
                    (Err(diagnostic), _) | (_, Err(diagnostic)) => OpStep::Error(diagnostic),
                }
            }
            (RuntimeValue::Record(values), "count", []) => {
                OpStep::Push(RuntimeValue::Int(values.len() as i64))
            }
            (RuntimeValue::Record(values), "has", [key]) => match self.render_value(key) {
                Ok(key) => OpStep::Push(RuntimeValue::Bool(values.contains_key(&key))),
                Err(diagnostic) => OpStep::Error(diagnostic),
            },
            (RuntimeValue::Record(values), "keys", []) => {
                self.push_charged(RuntimeValue::List(Rc::new(
                    values
                        .keys()
                        .map(|key| RuntimeValue::String(Arc::from(key.as_str())))
                        .collect(),
                )))
            }
            (RuntimeValue::Record(values), "values", []) => self.push_charged(RuntimeValue::List(
                Rc::new(values.values().cloned().collect()),
            )),
            (RuntimeValue::Record(values), "entries" | "to_list", []) => {
                self.push_charged(RuntimeValue::List(Rc::new(record_entries(&values))))
            }
            (RuntimeValue::Record(values), "merging" | "merge", [RuntimeValue::Record(other)]) => {
                let mut merged = Rc::unwrap_or_clone(values);
                merged.extend(
                    other
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone())),
                );
                self.push_charged(RuntimeValue::Record(Rc::new(merged)))
            }
            (RuntimeValue::Record(values), "removing" | "remove", [key]) => {
                match self.render_value(key) {
                    Ok(key) => {
                        let mut updated = Rc::unwrap_or_clone(values);
                        updated.remove(&key);
                        self.push_charged(RuntimeValue::Record(Rc::new(updated)))
                    }
                    Err(diagnostic) => OpStep::Error(diagnostic),
                }
            }
            (RuntimeValue::Record(values), "get", [key])
            | (RuntimeValue::Record(values), "get", [key, _]) => match self.render_value(key) {
                Ok(key) => OpStep::Push(
                    values
                        .get(&key)
                        .cloned()
                        .unwrap_or_else(|| args.get(1).cloned().unwrap_or(RuntimeValue::Nil)),
                ),
                Err(diagnostic) => OpStep::Error(diagnostic),
            },
            (RuntimeValue::Record(values), "to_dict", []) => {
                OpStep::Push(RuntimeValue::Record(values))
            }
            _ => OpStep::Error(diagnostic(
                "unsupported_method",
                format!("method `{method}` is not portable for this value"),
            )),
        }
    }

    pub(super) fn subscript(
        &mut self,
        value: &RuntimeValue,
        index: &RuntimeValue,
    ) -> Result<Option<RuntimeValue>, Diagnostic> {
        Ok(match (value, index) {
            (RuntimeValue::List(values), RuntimeValue::Int(index)) => {
                normalized_index(values.len(), *index).and_then(|index| values.get(index).cloned())
            }
            (RuntimeValue::Record(values), key) => values.get(&self.render_value(key)?).cloned(),
            (RuntimeValue::String(value), RuntimeValue::Int(index)) => {
                let length = value.chars().count();
                normalized_index(length, *index)
                    .and_then(|index| value.chars().nth(index))
                    .map(|value| RuntimeValue::String(Arc::from(value.to_string())))
            }
            _ => None,
        })
    }

    pub(super) fn contains(
        &mut self,
        container: &RuntimeValue,
        item: &RuntimeValue,
    ) -> Result<bool, Diagnostic> {
        match container {
            RuntimeValue::List(values) => {
                for value in values.iter() {
                    if self.values_equal(value, item)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            RuntimeValue::Record(values) => Ok(values.contains_key(&self.render_value(item)?)),
            RuntimeValue::String(value) => Ok(value.contains(&self.render_value(item)?)),
            _ => Ok(false),
        }
    }
}

fn record_entries(values: &BTreeMap<String, RuntimeValue>) -> Vec<RuntimeValue> {
    values
        .iter()
        .map(|(key, value)| {
            RuntimeValue::Record(Rc::new(BTreeMap::from([
                (
                    "key".to_string(),
                    RuntimeValue::String(Arc::from(key.as_str())),
                ),
                ("value".to_string(), value.clone()),
            ])))
        })
        .collect()
}
