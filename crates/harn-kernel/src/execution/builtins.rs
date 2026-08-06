use super::*;

fn runtime_text(value: &RuntimeValue) -> Option<String> {
    (!matches!(value, RuntimeValue::Nil)).then(|| value.display())
}

fn regex_capture_value(capture: crate::pure::RegexCapture) -> RuntimeValue {
    let mut values = BTreeMap::from([
        (
            "match".to_string(),
            RuntimeValue::String(Arc::from(capture.full_match)),
        ),
        (
            "groups".to_string(),
            RuntimeValue::List(Rc::new(
                capture
                    .groups
                    .into_iter()
                    .map(|value| {
                        value.map_or(RuntimeValue::Nil, |value| {
                            RuntimeValue::String(Arc::from(value))
                        })
                    })
                    .collect(),
            )),
        ),
        ("start".to_string(), RuntimeValue::Int(capture.start as i64)),
        ("end".to_string(), RuntimeValue::Int(capture.end as i64)),
        ("line".to_string(), RuntimeValue::Int(capture.line as i64)),
    ]);
    values.extend(
        capture
            .named
            .into_iter()
            .map(|(name, value)| (name, RuntimeValue::String(Arc::from(value)))),
    );
    RuntimeValue::Record(Rc::new(values))
}

fn is_filter_nil_value(value: &RuntimeValue) -> bool {
    matches!(value, RuntimeValue::Nil)
        || matches!(value, RuntimeValue::String(value) if value.is_empty() || value.as_ref() == "null")
}

impl Machine<'_> {
    pub(super) fn call_builtin(&mut self, name: &str, args: Vec<RuntimeValue>) -> OpStep {
        if let Err(diagnostic) = self.charge_call_validation(&args) {
            return OpStep::Error(diagnostic);
        }
        let Some(builtin) = PortableBuiltin::from_name(name) else {
            return OpStep::Error(diagnostic(
                "unsupported_builtin",
                format!("builtin `{name}` is not implemented by the portable kernel"),
            ));
        };
        match (builtin, args.as_slice()) {
            (PortableBuiltin::Len | PortableBuiltin::Count, [RuntimeValue::List(v)]) => {
                OpStep::Push(RuntimeValue::Int(v.len() as i64))
            }
            (PortableBuiltin::Len | PortableBuiltin::Count, [RuntimeValue::String(v)]) => {
                OpStep::Push(RuntimeValue::Int(v.chars().count() as i64))
            }
            (PortableBuiltin::String | PortableBuiltin::ToString, [value]) => {
                match self.render_value(value) {
                    Ok(value) => self.push_charged(RuntimeValue::String(Arc::from(value))),
                    Err(diagnostic) => OpStep::Error(diagnostic),
                }
            }
            (PortableBuiltin::HexEncode, [RuntimeValue::String(value)]) => self.push_charged(
                RuntimeValue::String(Arc::from(crate::pure::hex_encode(value.as_bytes()))),
            ),
            (PortableBuiltin::HexEncode, [RuntimeValue::Bytes(value)]) => self.push_charged(
                RuntimeValue::String(Arc::from(crate::pure::hex_encode(value))),
            ),
            (PortableBuiltin::HexDecode, [RuntimeValue::String(value)]) => {
                match crate::pure::hex_decode_text(value) {
                    Ok(value) => self.push_charged(RuntimeValue::String(Arc::from(value))),
                    Err(message) => OpStep::Error(diagnostic("builtin_error", message)),
                }
            }
            (PortableBuiltin::Trim, [value]) => match self.render_value(value) {
                Ok(value) => self.push_charged(RuntimeValue::String(Arc::from(
                    crate::pure::trim_text(&value),
                ))),
                Err(diagnostic) => OpStep::Error(diagnostic),
            },
            (PortableBuiltin::Replace, [input, old, new]) => {
                match (
                    self.render_value(input),
                    self.render_value(old),
                    self.render_value(new),
                ) {
                    (Ok(input), Ok(old), Ok(new)) => self.push_charged(RuntimeValue::String(
                        Arc::from(crate::pure::replace_text(&input, &old, &new)),
                    )),
                    (Err(diagnostic), _, _) | (_, Err(diagnostic), _) | (_, _, Err(diagnostic)) => {
                        OpStep::Error(diagnostic)
                    }
                }
            }
            (PortableBuiltin::StartsWith, [input, prefix]) => {
                match (self.render_value(input), self.render_value(prefix)) {
                    (Ok(input), Ok(prefix)) => OpStep::Push(RuntimeValue::Bool(
                        crate::pure::starts_with_text(&input, &prefix),
                    )),
                    (Err(diagnostic), _) | (_, Err(diagnostic)) => OpStep::Error(diagnostic),
                }
            }
            (PortableBuiltin::JsonStringify, [value]) => {
                match DataValue::try_from(value.clone()).and_then(|value| {
                    serde_json::to_string(&value.to_json()).map_err(|error| {
                        diagnostic("json_stringify", format!("json_stringify: {error}"))
                    })
                }) {
                    Ok(value) => self.push_charged(RuntimeValue::String(Arc::from(value))),
                    Err(diagnostic) => OpStep::Error(diagnostic),
                }
            }
            (PortableBuiltin::RegexMatch, [pattern, value])
            | (PortableBuiltin::RegexMatch, [pattern, value, _]) => {
                let (Some(pattern), Some(value)) = (runtime_text(pattern), runtime_text(value))
                else {
                    return OpStep::Push(RuntimeValue::Nil);
                };
                let flags = args.get(2).and_then(runtime_text).unwrap_or_default();
                match crate::pure::regex_matches(&pattern, &value, &flags) {
                    Ok(values) if values.is_empty() => OpStep::Push(RuntimeValue::Nil),
                    Ok(values) => self.push_charged(RuntimeValue::List(Rc::new(
                        values
                            .into_iter()
                            .map(|value| RuntimeValue::String(Arc::from(value)))
                            .collect(),
                    ))),
                    Err(message) => OpStep::Error(diagnostic(
                        "invalid_regex",
                        format!("Invalid regex: {message}"),
                    )),
                }
            }
            (PortableBuiltin::RegexReplace, [pattern, replacement, value])
            | (PortableBuiltin::RegexReplace, [pattern, replacement, value, _]) => {
                let (Some(pattern), Some(replacement), Some(value)) = (
                    runtime_text(pattern),
                    runtime_text(replacement),
                    runtime_text(value),
                ) else {
                    return OpStep::Push(RuntimeValue::Nil);
                };
                let flags = args.get(3).and_then(runtime_text).unwrap_or_default();
                match crate::pure::regex_replace(&pattern, &replacement, &value, &flags) {
                    Ok(value) => self.push_charged(RuntimeValue::String(Arc::from(value))),
                    Err(message) => OpStep::Error(diagnostic(
                        "invalid_regex",
                        format!("Invalid regex: {message}"),
                    )),
                }
            }
            (PortableBuiltin::RegexCaptures, [pattern, value])
            | (PortableBuiltin::RegexCaptures, [pattern, value, _]) => {
                let (Some(pattern), Some(value)) = (runtime_text(pattern), runtime_text(value))
                else {
                    return OpStep::Push(RuntimeValue::List(Rc::new(Vec::new())));
                };
                let flags = args.get(2).and_then(runtime_text).unwrap_or_default();
                match crate::pure::regex_captures(&pattern, &value, &flags) {
                    Ok(captures) => self.push_charged(RuntimeValue::List(Rc::new(
                        captures.into_iter().map(regex_capture_value).collect(),
                    ))),
                    Err(message) => OpStep::Error(diagnostic(
                        "invalid_regex",
                        format!("Invalid regex: {message}"),
                    )),
                }
            }
            (PortableBuiltin::RegexSplit, [value, pattern])
            | (PortableBuiltin::RegexSplit, [value, pattern, _]) => {
                let (Some(value), Some(pattern)) = (runtime_text(value), runtime_text(pattern))
                else {
                    return OpStep::Push(RuntimeValue::Nil);
                };
                let flags = args.get(2).and_then(runtime_text).unwrap_or_default();
                match crate::pure::regex_split(&pattern, &value, &flags) {
                    Ok(values) => self.push_charged(RuntimeValue::List(Rc::new(
                        values
                            .into_iter()
                            .map(|value| RuntimeValue::String(Arc::from(value)))
                            .collect(),
                    ))),
                    Err(message) => OpStep::Error(diagnostic(
                        "invalid_regex",
                        format!("Invalid regex: {message}"),
                    )),
                }
            }
            (PortableBuiltin::Sha256, [RuntimeValue::String(value)]) => self.push_charged(
                RuntimeValue::String(Arc::from(crate::pure::sha256_hex(value.as_bytes()))),
            ),
            (PortableBuiltin::Sha256, [RuntimeValue::Bytes(value)]) => self.push_charged(
                RuntimeValue::String(Arc::from(crate::pure::sha256_hex(value))),
            ),
            (PortableBuiltin::SecretScan, [value]) => {
                let Some(content) = runtime_text(value) else {
                    return OpStep::Error(diagnostic(
                        "builtin_error",
                        "secret_scan: content is required",
                    ));
                };
                match serde_json::to_value(crate::pure::scan_secrets(&content))
                    .map_err(|error| diagnostic("builtin_error", format!("secret_scan: {error}")))
                    .and_then(DataValue::from_json)
                {
                    Ok(value) => self.push_charged(RuntimeValue::from(value)),
                    Err(diagnostic) => OpStep::Error(diagnostic),
                }
            }
            (PortableBuiltin::PathJoin, values) => {
                let mut segments = Vec::with_capacity(values.len());
                for value in values {
                    match self.render_value(value) {
                        Ok(value) => segments.push(value),
                        Err(diagnostic) => return OpStep::Error(diagnostic),
                    }
                }
                self.push_charged(RuntimeValue::String(Arc::from(
                    crate::pure::join_path_segments(&segments),
                )))
            }
            (PortableBuiltin::DictFilterNil, [RuntimeValue::Record(values)]) => {
                let filtered = values
                    .iter()
                    .filter(|(_, value)| !is_filter_nil_value(value))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                self.push_charged(RuntimeValue::Record(Rc::new(filtered)))
            }
            (
                PortableBuiltin::MakeStruct,
                [RuntimeValue::String(_), RuntimeValue::Record(values), _],
            ) => OpStep::Push(RuntimeValue::Record(values.clone())),
            (PortableBuiltin::AssertList, [RuntimeValue::List(_)]) => {
                OpStep::Push(RuntimeValue::Nil)
            }
            (PortableBuiltin::AssertList, [value]) => OpStep::Error(diagnostic(
                "list_type",
                format!(
                    "cannot destructure {} with [...] pattern — expected list",
                    runtime_value_kind(value)
                ),
            )),
            (PortableBuiltin::AssertSchema, [value, RuntimeValue::String(name), schema]) => {
                match DataValue::try_from(schema.clone()) {
                    Ok(schema) if matches_compiler_schema(value, &schema) => {
                        OpStep::Push(RuntimeValue::Nil)
                    }
                    Ok(_) => OpStep::Error(diagnostic(
                        "argument_type",
                        format!("parameter `{name}` rejected {}", runtime_value_kind(value)),
                    )),
                    Err(diagnostic) => OpStep::Error(diagnostic),
                }
            }
            _ => OpStep::Error(diagnostic(
                "unsupported_builtin",
                format!("builtin `{name}` is not implemented by the portable kernel"),
            )),
        }
    }
}
