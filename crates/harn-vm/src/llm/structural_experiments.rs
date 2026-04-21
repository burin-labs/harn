use std::collections::BTreeMap;

use crate::value::{VmError, VmValue};

#[derive(Clone, Debug)]
pub(crate) enum StructuralExperimentHandler {
    BuiltIn(BuiltInStructuralExperiment),
    Closure(VmValue),
}

#[derive(Clone, Debug)]
pub(crate) enum BuiltInStructuralExperiment {
    PromptOrderPermutation { seed: i64 },
    DoubledPrompt,
    ChainOfDraft,
    InvertedSystem,
}

#[derive(Clone, Debug)]
pub(crate) struct StructuralExperimentConfig {
    pub label: String,
    pub name: String,
    pub args: serde_json::Value,
    pub handler: StructuralExperimentHandler,
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct AppliedStructuralExperiment {
    pub label: String,
    pub name: String,
    pub args: serde_json::Value,
    pub metadata: serde_json::Value,
}

const STRUCTURAL_EXPERIMENT_ENV: &str = "HARN_STRUCTURAL_EXPERIMENT";

pub(crate) fn parse_structural_experiment_option(
    options: Option<&BTreeMap<String, VmValue>>,
) -> Result<Option<StructuralExperimentConfig>, VmError> {
    let explicit = options.and_then(|dict| dict.get("structural_experiment"));
    match explicit {
        Some(VmValue::Nil) | Some(VmValue::Bool(false)) => Ok(None),
        Some(VmValue::String(spec)) => parse_structural_experiment_spec(spec.as_ref()),
        Some(VmValue::Dict(dict)) => parse_structural_experiment_dict(dict),
        Some(VmValue::Closure(closure)) => Ok(Some(StructuralExperimentConfig {
            label: "custom".to_string(),
            name: "custom".to_string(),
            args: serde_json::json!({}),
            handler: StructuralExperimentHandler::Closure(VmValue::Closure(closure.clone())),
        })),
        Some(other) => Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
            format!(
                "structural_experiment: expected string, dict, closure, nil, or false; got {}",
                other.type_name()
            ),
        )))),
        None => match std::env::var(STRUCTURAL_EXPERIMENT_ENV) {
            Ok(spec) if !spec.trim().is_empty() => parse_structural_experiment_spec(spec.trim()),
            _ => Ok(None),
        },
    }
}

fn parse_structural_experiment_dict(
    dict: &BTreeMap<String, VmValue>,
) -> Result<Option<StructuralExperimentConfig>, VmError> {
    let label = dict
        .get("label")
        .map(|value| value.display())
        .filter(|value| !value.trim().is_empty());
    let name = dict
        .get("name")
        .or_else(|| dict.get("experiment"))
        .map(|value| value.display())
        .filter(|value| !value.trim().is_empty());
    let args_value = dict
        .get("args")
        .map(crate::llm::helpers::vm_value_to_json)
        .unwrap_or_else(|| {
            let mut args = serde_json::Map::new();
            for (key, value) in dict {
                if matches!(
                    key.as_str(),
                    "label" | "name" | "experiment" | "transform" | "handler"
                ) {
                    continue;
                }
                args.insert(key.clone(), crate::llm::helpers::vm_value_to_json(value));
            }
            serde_json::Value::Object(args)
        });

    if let Some(transform) = dict
        .get("transform")
        .or_else(|| dict.get("handler"))
        .filter(|value| matches!(value, VmValue::Closure(_)))
    {
        let resolved_name = name.unwrap_or_else(|| "custom".to_string());
        let resolved_label = label.unwrap_or_else(|| resolved_name.clone());
        return Ok(Some(StructuralExperimentConfig {
            label: resolved_label,
            name: resolved_name,
            args: args_value,
            handler: StructuralExperimentHandler::Closure(transform.clone()),
        }));
    }

    let Some(name) = name else {
        return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
            "structural_experiment: dict form requires `name` or `transform`",
        ))));
    };
    build_builtin_experiment(&name, label, args_value)
}

fn parse_structural_experiment_spec(
    spec: &str,
) -> Result<Option<StructuralExperimentConfig>, VmError> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Ok(None);
    }
    let (name, args) = if let Some(open_idx) = spec.find('(') {
        let close_idx = spec.rfind(')').ok_or_else(|| {
            VmError::Thrown(VmValue::String(std::rc::Rc::from(
                "structural_experiment: missing closing `)`",
            )))
        })?;
        if close_idx < open_idx {
            return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                "structural_experiment: invalid argument list",
            ))));
        }
        let name = spec[..open_idx].trim();
        let args = parse_named_args(&spec[open_idx + 1..close_idx])?;
        (name, serde_json::Value::Object(args))
    } else {
        (spec, serde_json::json!({}))
    };
    build_builtin_experiment(name, Some(spec.to_string()), args)
}

fn build_builtin_experiment(
    raw_name: &str,
    explicit_label: Option<String>,
    args: serde_json::Value,
) -> Result<Option<StructuralExperimentConfig>, VmError> {
    let name = raw_name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    let handler = match name {
        "prompt_order_permutation" => {
            let seed = args
                .get("seed")
                .and_then(|value| value.as_i64())
                .unwrap_or(0);
            StructuralExperimentHandler::BuiltIn(
                BuiltInStructuralExperiment::PromptOrderPermutation { seed },
            )
        }
        "doubled_prompt" => {
            StructuralExperimentHandler::BuiltIn(BuiltInStructuralExperiment::DoubledPrompt)
        }
        "chain_of_draft" => {
            StructuralExperimentHandler::BuiltIn(BuiltInStructuralExperiment::ChainOfDraft)
        }
        "inverted_system" => {
            StructuralExperimentHandler::BuiltIn(BuiltInStructuralExperiment::InvertedSystem)
        }
        other => {
            return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                format!("unknown structural experiment `{other}`"),
            ))))
        }
    };
    Ok(Some(StructuralExperimentConfig {
        label: explicit_label.unwrap_or_else(|| name.to_string()),
        name: name.to_string(),
        args,
        handler,
    }))
}

fn parse_named_args(input: &str) -> Result<serde_json::Map<String, serde_json::Value>, VmError> {
    let mut out = serde_json::Map::new();
    for part in split_top_level(input, ',') {
        let item = part.trim();
        if item.is_empty() {
            continue;
        }
        let Some(colon_idx) = item.find(':') else {
            return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                format!("structural_experiment: expected `name: value`, got `{item}`"),
            ))));
        };
        let key = item[..colon_idx].trim();
        if key.is_empty() {
            return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                "structural_experiment: empty argument name",
            ))));
        }
        let value = parse_scalar_value(item[colon_idx + 1..].trim())?;
        out.insert(key.to_string(), value);
    }
    Ok(out)
}

fn split_top_level(input: &str, delimiter: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut escape = false;
    for ch in input.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }
        match ch {
            '\\' if in_string => {
                current.push(ch);
                escape = true;
            }
            '"' => {
                in_string = !in_string;
                current.push(ch);
            }
            _ if ch == delimiter && !in_string => {
                out.push(current);
                current = String::new();
            }
            _ => current.push(ch),
        }
    }
    out.push(current);
    out
}

fn parse_scalar_value(input: &str) -> Result<serde_json::Value, VmError> {
    if input.eq_ignore_ascii_case("true") {
        return Ok(serde_json::json!(true));
    }
    if input.eq_ignore_ascii_case("false") {
        return Ok(serde_json::json!(false));
    }
    if input.eq_ignore_ascii_case("nil") || input.eq_ignore_ascii_case("null") {
        return Ok(serde_json::Value::Null);
    }
    if let Ok(value) = input.parse::<i64>() {
        return Ok(serde_json::json!(value));
    }
    if let Ok(value) = input.parse::<f64>() {
        return Ok(serde_json::json!(value));
    }
    if input.starts_with('"') && input.ends_with('"') && input.len() >= 2 {
        return serde_json::from_str(input).map_err(|error| {
            VmError::Thrown(VmValue::String(std::rc::Rc::from(format!(
                "structural_experiment: invalid string literal `{input}`: {error}"
            ))))
        });
    }
    Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
        format!("structural_experiment: unsupported argument value `{input}`",),
    ))))
}

pub(crate) async fn apply_structural_experiment(
    opts: &mut crate::llm::api::LlmCallOptions,
    iteration: Option<usize>,
) -> Result<Option<AppliedStructuralExperiment>, VmError> {
    let Some(config) = opts.structural_experiment.clone() else {
        opts.applied_structural_experiment = None;
        return Ok(None);
    };
    let current_messages = opts.messages.clone();
    let current_system = opts.system.clone();
    let (messages, system, metadata) = match config.handler {
        StructuralExperimentHandler::BuiltIn(kind) => {
            let (messages, system) =
                apply_builtin_experiment(&kind, current_messages, current_system.clone());
            (messages, system, config.args.clone())
        }
        StructuralExperimentHandler::Closure(ref closure) => {
            let VmValue::Closure(closure) = closure else {
                return Err(VmError::Runtime(
                    "structural_experiment transform must be a closure".to_string(),
                ));
            };
            let mut vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
                VmError::Runtime(
                    "structural_experiment requires an async builtin VM context".to_string(),
                )
            })?;
            let mut ctx = BTreeMap::new();
            ctx.insert(
                "messages".to_string(),
                VmValue::List(std::rc::Rc::new(crate::llm::helpers::json_messages_to_vm(
                    &current_messages,
                ))),
            );
            ctx.insert(
                "system".to_string(),
                current_system
                    .as_ref()
                    .map(|value| VmValue::String(std::rc::Rc::from(value.as_str())))
                    .unwrap_or(VmValue::Nil),
            );
            ctx.insert(
                "iteration".to_string(),
                iteration
                    .map(|value| VmValue::Int(value as i64))
                    .unwrap_or(VmValue::Nil),
            );
            ctx.insert(
                "label".to_string(),
                VmValue::String(std::rc::Rc::from(config.label.as_str())),
            );
            ctx.insert(
                "name".to_string(),
                VmValue::String(std::rc::Rc::from(config.name.as_str())),
            );
            ctx.insert(
                "args".to_string(),
                crate::stdlib::json_to_vm_value(&config.args),
            );
            interpret_closure_result(
                &current_messages,
                current_system.as_deref(),
                &vm.call_closure_pub(closure, &[VmValue::Dict(std::rc::Rc::new(ctx))], &[])
                    .await?,
                &config,
            )?
        }
    };
    opts.messages = messages;
    opts.system = system;
    let applied = AppliedStructuralExperiment {
        label: config.label.clone(),
        name: config.name.clone(),
        args: config.args,
        metadata,
    };
    opts.applied_structural_experiment = Some(applied.clone());
    Ok(Some(applied))
}

fn interpret_closure_result(
    current_messages: &[serde_json::Value],
    current_system: Option<&str>,
    result: &VmValue,
    config: &StructuralExperimentConfig,
) -> Result<(Vec<serde_json::Value>, Option<String>, serde_json::Value), VmError> {
    match result {
        VmValue::Nil => Ok((
            current_messages.to_vec(),
            current_system.map(str::to_string),
            serde_json::json!({}),
        )),
        VmValue::List(list) => Ok((
            crate::llm::helpers::vm_messages_to_json(list)?,
            current_system.map(str::to_string),
            serde_json::json!({}),
        )),
        VmValue::Dict(dict) => {
            let messages = match dict.get("messages") {
                Some(VmValue::List(list)) => crate::llm::helpers::vm_messages_to_json(list)?,
                Some(VmValue::Nil) | None => current_messages.to_vec(),
                Some(_) => {
                    return Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
                        "structural_experiment transform: `messages` must be a list",
                    ))))
                }
            };
            let system = match dict.get("system") {
                Some(VmValue::Nil) => None,
                Some(value) => Some(value.display()),
                None => current_system.map(str::to_string),
            };
            let label = dict
                .get("label")
                .map(|value| value.display())
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| config.label.clone());
            let name = dict
                .get("name")
                .map(|value| value.display())
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| config.name.clone());
            let mut metadata = serde_json::Map::new();
            metadata.insert("label".to_string(), serde_json::json!(label));
            metadata.insert("name".to_string(), serde_json::json!(name));
            if let Some(value) = dict.get("metadata") {
                metadata.insert(
                    "details".to_string(),
                    crate::llm::helpers::vm_value_to_json(value),
                );
            }
            Ok((messages, system, serde_json::Value::Object(metadata)))
        }
        _ => Err(VmError::Thrown(VmValue::String(std::rc::Rc::from(
            "structural_experiment transform must return nil, a message list, or a dict",
        )))),
    }
}

fn apply_builtin_experiment(
    kind: &BuiltInStructuralExperiment,
    mut messages: Vec<serde_json::Value>,
    mut system: Option<String>,
) -> (Vec<serde_json::Value>, Option<String>) {
    match kind {
        BuiltInStructuralExperiment::PromptOrderPermutation { seed } => {
            if let Some(index) = latest_string_user_message_index(&messages) {
                if let Some(content) = message_text(&messages[index]) {
                    let sections = split_prompt_sections(content);
                    if sections.len() > 1 {
                        let mut ranked: Vec<(u64, usize, String)> = sections
                            .into_iter()
                            .enumerate()
                            .map(|(idx, section)| (permute_score(*seed, idx), idx, section))
                            .collect();
                        ranked.sort_by_key(|entry| (entry.0, entry.1));
                        let mut ordered: Vec<String> =
                            ranked.into_iter().map(|entry| entry.2).collect();
                        if ordered.join("\n\n") == content && ordered.len() > 1 {
                            ordered.rotate_left(1);
                        }
                        set_message_text(&mut messages[index], ordered.join("\n\n"));
                    }
                }
            }
        }
        BuiltInStructuralExperiment::DoubledPrompt => {
            if let Some(index) = latest_string_user_message_index(&messages) {
                if let Some(original) = messages.get(index).cloned() {
                    messages.insert(0, original.clone());
                    messages.push(original);
                }
            }
        }
        BuiltInStructuralExperiment::ChainOfDraft => {
            if let Some(index) = latest_string_user_message_index(&messages) {
                if let Some(content) = message_text(&messages[index]).map(str::to_string) {
                    set_message_text(
                        &mut messages[index],
                        format!(
                            "{content}\n\nBefore the final answer, emit a terse scratch draft inside <draft>...</draft>. After the draft block, provide the final answer outside those tags."
                        ),
                    );
                }
            }
        }
        BuiltInStructuralExperiment::InvertedSystem => {
            if let Some(index) = latest_string_user_message_index(&messages) {
                if let (Some(content), Some(current_system)) = (
                    message_text(&messages[index]).map(str::to_string),
                    system.clone(),
                ) {
                    set_message_text(&mut messages[index], current_system);
                    system = Some(content);
                }
            }
        }
    }
    (messages, system)
}

fn latest_string_user_message_index(messages: &[serde_json::Value]) -> Option<usize> {
    messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(idx, message)| {
            let is_user = message.get("role").and_then(|value| value.as_str()) == Some("user");
            if is_user
                && message
                    .get("content")
                    .and_then(|value| value.as_str())
                    .is_some()
            {
                Some(idx)
            } else {
                None
            }
        })
}

fn message_text(message: &serde_json::Value) -> Option<&str> {
    message.get("content").and_then(|value| value.as_str())
}

fn set_message_text(message: &mut serde_json::Value, content: String) {
    if let Some(object) = message.as_object_mut() {
        object.insert("content".to_string(), serde_json::json!(content));
    }
}

fn split_prompt_sections(content: &str) -> Vec<String> {
    content
        .split("\n\n")
        .map(str::trim)
        .filter(|section| !section.is_empty())
        .map(str::to_string)
        .collect()
}

fn permute_score(seed: i64, idx: usize) -> u64 {
    let mut value = seed as u64 ^ (idx as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    value = value.wrapping_mul(6364136223846793005).wrapping_add(1);
    value ^ (value >> 33)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_string_spec_with_seed() {
        let parsed = parse_structural_experiment_spec("prompt_order_permutation(seed: 42)")
            .expect("parse")
            .expect("config");
        assert_eq!(parsed.name, "prompt_order_permutation");
        assert_eq!(parsed.args["seed"], serde_json::json!(42));
    }

    #[test]
    fn prompt_order_permutation_reorders_sections() {
        let (messages, _system) = apply_builtin_experiment(
            &BuiltInStructuralExperiment::PromptOrderPermutation { seed: 42 },
            vec![serde_json::json!({
                "role": "user",
                "content": "alpha\n\nbeta\n\ngamma"
            })],
            None,
        );
        let content = messages[0]["content"].as_str().expect("content");
        assert_ne!(content, "alpha\n\nbeta\n\ngamma");
        assert!(content.contains("alpha"));
        assert!(content.contains("beta"));
        assert!(content.contains("gamma"));
    }

    #[test]
    fn inverted_system_swaps_latest_user_and_system() {
        let (messages, system) = apply_builtin_experiment(
            &BuiltInStructuralExperiment::InvertedSystem,
            vec![serde_json::json!({
                "role": "user",
                "content": "user prompt"
            })],
            Some("system prompt".to_string()),
        );
        assert_eq!(messages[0]["content"], serde_json::json!("system prompt"));
        assert_eq!(system.as_deref(), Some("user prompt"));
    }
}
