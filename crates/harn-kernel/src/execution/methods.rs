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
                            Ok(()) => OpStep::Throw(RuntimeException {
                                value,
                                declared: false,
                            }),
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
            (RuntimeValue::List(values), "len", []) => {
                OpStep::Push(RuntimeValue::Int(values.len() as i64))
            }
            (RuntimeValue::List(values), "empty", []) => {
                OpStep::Push(RuntimeValue::Bool(values.is_empty()))
            }
            (RuntimeValue::List(values), "contains", [value]) => {
                for item in values.iter() {
                    match self.values_equal(item, value) {
                        Ok(true) => return OpStep::Push(RuntimeValue::Bool(true)),
                        Ok(false) => {}
                        Err(diagnostic) => return OpStep::Error(diagnostic),
                    }
                }
                OpStep::Push(RuntimeValue::Bool(false))
            }
            (RuntimeValue::String(value), "len", []) => {
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
            (RuntimeValue::Record(values), "len", []) => {
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
            (RuntimeValue::Record(values), "merge", [RuntimeValue::Record(other)]) => {
                let mut merged = Rc::unwrap_or_clone(values);
                merged.extend(
                    other
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone())),
                );
                self.push_charged(RuntimeValue::Record(Rc::new(merged)))
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

    /// `__method_mutate(receiver, method, args...)`: the receiver's next
    /// value and the call's result as `[next, result]`, or nil when the
    /// receiver has no such mutating builtin method and the compiled call
    /// site falls back to an ordinary method call.
    pub(super) fn method_mutate(
        &mut self,
        receiver: &RuntimeValue,
        method: &str,
        args: &[RuntimeValue],
    ) -> OpStep {
        let outcome = |next: RuntimeValue, result: RuntimeValue| {
            RuntimeValue::List(Rc::new(vec![next, result]))
        };
        let pushed = match (receiver, method, args) {
            (RuntimeValue::List(values), "push", [item]) => {
                let mut next = values.to_vec();
                next.push(item.clone());
                outcome(RuntimeValue::List(Rc::new(next)), RuntimeValue::Nil)
            }
            (RuntimeValue::List(values), "pop", []) => {
                let mut next = values.to_vec();
                let last = next.pop().unwrap_or(RuntimeValue::Nil);
                outcome(RuntimeValue::List(Rc::new(next)), last)
            }
            (RuntimeValue::List(values), "insert", [RuntimeValue::Int(index), item]) => {
                let len = values.len() as i64;
                let at = if *index < 0 { len + index } else { *index }.clamp(0, len) as usize;
                let mut next = values.to_vec();
                next.insert(at, item.clone());
                outcome(RuntimeValue::List(Rc::new(next)), RuntimeValue::Nil)
            }
            (RuntimeValue::List(values), "extend", [RuntimeValue::List(more)]) => {
                let mut next = values.to_vec();
                next.extend(more.iter().cloned());
                outcome(RuntimeValue::List(Rc::new(next)), RuntimeValue::Nil)
            }
            (RuntimeValue::List(_), "clear", []) => {
                outcome(RuntimeValue::List(Rc::new(Vec::new())), RuntimeValue::Nil)
            }
            (RuntimeValue::List(values), "sort", []) => {
                let mut next = values.to_vec();
                next.sort_by(|a, b| ordering(a, b).unwrap_or(0).cmp(&0));
                outcome(RuntimeValue::List(Rc::new(next)), RuntimeValue::Nil)
            }
            (RuntimeValue::List(values), "reverse", []) => {
                let mut next = values.to_vec();
                next.reverse();
                outcome(RuntimeValue::List(Rc::new(next)), RuntimeValue::Nil)
            }
            (RuntimeValue::Record(values), "remove", [key]) => match self.render_value(key) {
                Ok(key) => {
                    let mut next = (**values).clone();
                    let removed = next.remove(&key).unwrap_or(RuntimeValue::Nil);
                    outcome(RuntimeValue::Record(Rc::new(next)), removed)
                }
                Err(diagnostic) => return OpStep::Error(diagnostic),
            },
            (RuntimeValue::Record(_), "clear", []) => outcome(
                RuntimeValue::Record(Rc::new(BTreeMap::new())),
                RuntimeValue::Nil,
            ),
            _ => RuntimeValue::Nil,
        };
        self.push_charged(pushed)
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

#[cfg(test)]
mod registry_drift_tests {
    //! The portable kernel implements a subset of the builtin methods. Every
    //! method it dispatches must be a row of the method registry
    //! (`harn_parser::builtin_methods`) with the same effect, so the kernel
    //! can never accept a spelling the typechecker rejects.

    use harn_parser::builtin_methods::{self as methods, MethodReceiver};

    /// `(receiver, method)` pairs from `(RuntimeValue::Kind(..), "method"`
    /// match arms.
    fn dispatched(source: &str) -> Vec<(MethodReceiver, &str)> {
        let mut found = Vec::new();
        for line in source.lines() {
            let arm = line.trim_start();
            let arm = arm.strip_prefix("| ").unwrap_or(arm);
            for (kind, receiver) in [
                ("(RuntimeValue::List(", MethodReceiver::List),
                ("(RuntimeValue::String(", MethodReceiver::String),
                ("(RuntimeValue::Record(", MethodReceiver::Dict),
            ] {
                let Some(rest) = arm.strip_prefix(kind) else {
                    continue;
                };
                let Some((_, after)) = rest.split_once("), \"") else {
                    continue;
                };
                let Some((name, _)) = after.split_once('"') else {
                    continue;
                };
                if name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                    found.push((receiver, name));
                }
            }
        }
        found
    }

    #[test]
    fn kernel_methods_are_registry_rows_with_the_same_effect() {
        let source = include_str!("methods.rs");
        let split = source
            .find("pub(super) fn method_mutate(")
            .expect("method_mutate is defined");
        let end = source
            .find("pub(super) fn subscript(")
            .expect("subscript follows method_mutate");
        let plain = source.get(..split).expect("plain section");
        let mutating = source.get(split..end).expect("mutating section");
        for (section, mutates) in [(plain, false), (mutating, true)] {
            let arms = dispatched(section);
            assert!(!arms.is_empty(), "scan found no kernel method arms");
            for (receiver, name) in arms {
                let row = methods::method(receiver, name).unwrap_or_else(|| {
                    panic!("the kernel dispatches {receiver:?}.{name}, which has no registry row")
                });
                assert_eq!(
                    row.is_mutating(),
                    mutates,
                    "{receiver:?}.{name}: the kernel and the registry disagree on mutation"
                );
            }
        }
    }
}
