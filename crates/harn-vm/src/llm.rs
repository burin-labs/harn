use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

/// Register LLM builtins on a VM.
pub fn register_llm_builtins(vm: &mut Vm) {
    vm.register_async_builtin("llm_call", |args| async move {
        let prompt = args.first().map(|a| a.display()).unwrap_or_default();
        let system = args.get(1).map(|a| a.display());
        let options = args.get(2).and_then(|a| a.as_dict()).cloned();

        let provider = vm_resolve_provider(&options);
        let model = vm_resolve_model(&options, &provider);
        let api_key = vm_resolve_api_key(&provider)?;
        let max_tokens = options
            .as_ref()
            .and_then(|o| o.get("max_tokens"))
            .and_then(|v| v.as_int())
            .unwrap_or(4096);

        vm_call_llm(
            &provider,
            &model,
            &api_key,
            &prompt,
            system.as_deref(),
            max_tokens,
        )
        .await
    });

    vm.register_async_builtin("agent_loop", |args| async move {
        let prompt = args.first().map(|a| a.display()).unwrap_or_default();
        let system = args.get(1).map(|a| a.display());
        let options = args.get(2).and_then(|a| a.as_dict()).cloned();

        let provider = vm_resolve_provider(&options);
        let model = vm_resolve_model(&options, &provider);
        let api_key = vm_resolve_api_key(&provider)?;
        let max_iterations = options
            .as_ref()
            .and_then(|o| o.get("max_iterations"))
            .and_then(|v| v.as_int())
            .unwrap_or(50) as usize;
        let persistent = options
            .as_ref()
            .and_then(|o| o.get("persistent"))
            .map(|v| v.is_truthy())
            .unwrap_or(false);
        let max_nudges = options
            .as_ref()
            .and_then(|o| o.get("max_nudges"))
            .and_then(|v| v.as_int())
            .unwrap_or(3) as usize;
        let custom_nudge = options
            .as_ref()
            .and_then(|o| o.get("nudge"))
            .map(|v| v.display());
        let max_tokens = options
            .as_ref()
            .and_then(|o| o.get("max_tokens"))
            .and_then(|v| v.as_int())
            .unwrap_or(4096);

        let mut system_prompt = system.unwrap_or_default();
        if persistent {
            system_prompt.push_str(
                "\n\nIMPORTANT: You MUST keep working until the task is complete. \
                 Do NOT stop to explain or summarize — take action. \
                 Output ##DONE## only when the task is fully complete and verified.",
            );
        }

        let mut messages: Vec<serde_json::Value> = vec![serde_json::json!({
            "role": "user",
            "content": prompt,
        })];

        let mut total_text = String::new();
        let mut consecutive_text_only = 0usize;

        for _iteration in 0..max_iterations {
            let response = vm_call_llm_messages(
                &provider,
                &model,
                &api_key,
                &messages,
                if system_prompt.is_empty() {
                    None
                } else {
                    Some(&system_prompt)
                },
                max_tokens,
            )
            .await?;

            let text = response.display();
            total_text.push_str(&text);

            messages.push(serde_json::json!({
                "role": "assistant",
                "content": text,
            }));

            if persistent && text.contains("##DONE##") {
                break;
            }

            if !persistent {
                break;
            }

            consecutive_text_only += 1;
            if consecutive_text_only > max_nudges {
                break;
            }

            let nudge = if let Some(ref custom) = custom_nudge {
                custom.clone()
            } else {
                "You have not output ##DONE## yet — the task is not complete. \
                 Use your tools to continue working. \
                 Only output ##DONE## when the task is fully complete and verified."
                    .to_string()
            };

            messages.push(serde_json::json!({
                "role": "user",
                "content": nudge,
            }));
        }

        Ok(VmValue::String(Rc::from(total_text.as_str())))
    });
}

// =============================================================================
// LLM helpers
// =============================================================================

fn vm_resolve_provider(options: &Option<BTreeMap<String, VmValue>>) -> String {
    options
        .as_ref()
        .and_then(|o| o.get("provider"))
        .map(|v| v.display())
        .unwrap_or_else(|| "anthropic".to_string())
}

fn vm_resolve_model(options: &Option<BTreeMap<String, VmValue>>, provider: &str) -> String {
    options
        .as_ref()
        .and_then(|o| o.get("model"))
        .map(|v| v.display())
        .unwrap_or_else(|| match provider {
            "openai" => "gpt-4o".to_string(),
            "ollama" => "llama3.2".to_string(),
            "openrouter" => "anthropic/claude-sonnet-4-20250514".to_string(),
            _ => "claude-sonnet-4-20250514".to_string(),
        })
}

fn vm_resolve_api_key(provider: &str) -> Result<String, VmError> {
    match provider {
        "ollama" => Ok(String::new()),
        "openai" => std::env::var("OPENAI_API_KEY").map_err(|_| {
            VmError::Thrown(VmValue::String(Rc::from(
                "Missing API key: set OPENAI_API_KEY environment variable",
            )))
        }),
        "openrouter" => std::env::var("OPENROUTER_API_KEY").map_err(|_| {
            VmError::Thrown(VmValue::String(Rc::from(
                "Missing API key: set OPENROUTER_API_KEY environment variable",
            )))
        }),
        _ => std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
            VmError::Thrown(VmValue::String(Rc::from(
                "Missing API key: set ANTHROPIC_API_KEY environment variable",
            )))
        }),
    }
}

async fn vm_call_llm(
    provider: &str,
    model: &str,
    api_key: &str,
    prompt: &str,
    system: Option<&str>,
    max_tokens: i64,
) -> Result<VmValue, VmError> {
    let messages = vec![serde_json::json!({
        "role": "user",
        "content": prompt,
    })];
    vm_call_llm_messages(provider, model, api_key, &messages, system, max_tokens).await
}

async fn vm_call_llm_messages(
    provider: &str,
    model: &str,
    api_key: &str,
    messages: &[serde_json::Value],
    system: Option<&str>,
    max_tokens: i64,
) -> Result<VmValue, VmError> {
    let client = reqwest::Client::new();

    match provider {
        "openai" | "ollama" | "openrouter" => {
            let base_url = match provider {
                "ollama" => std::env::var("OLLAMA_HOST")
                    .unwrap_or_else(|_| "http://localhost:11434".to_string()),
                "openrouter" => "https://openrouter.ai/api".to_string(),
                _ => "https://api.openai.com".to_string(),
            };

            let mut msgs = Vec::new();
            if let Some(sys) = system {
                msgs.push(serde_json::json!({"role": "system", "content": sys}));
            }
            msgs.extend(messages.iter().cloned());

            let body = serde_json::json!({
                "model": model,
                "messages": msgs,
                "max_tokens": max_tokens,
            });

            let mut req = client
                .post(format!("{base_url}/v1/chat/completions"))
                .header("Content-Type", "application/json")
                .json(&body);

            if !api_key.is_empty() {
                req = req.header("Authorization", format!("Bearer {api_key}"));
            }

            let response = req.send().await.map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "{provider} API error: {e}"
                ))))
            })?;

            let json: serde_json::Value = response.json().await.map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "{provider} response parse error: {e}"
                ))))
            })?;

            if let Some(err) = json["error"]["message"].as_str() {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "{provider} API error: {err}"
                )))));
            }

            let text = json["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or("")
                .to_string();
            Ok(VmValue::String(Rc::from(text.as_str())))
        }
        _ => {
            let mut body = serde_json::json!({
                "model": model,
                "messages": messages,
                "max_tokens": max_tokens,
            });
            if let Some(sys) = system {
                body["system"] = serde_json::json!(sys);
            }

            let response = client
                .post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| {
                    VmError::Thrown(VmValue::String(Rc::from(format!(
                        "Anthropic API error: {e}"
                    ))))
                })?;

            let json: serde_json::Value = response.json().await.map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Anthropic response parse error: {e}"
                ))))
            })?;

            let text = json["content"]
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();

            if text.is_empty() {
                if let Some(err) = json["error"]["message"].as_str() {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "Anthropic API error: {err}"
                    )))));
                }
            }

            Ok(VmValue::String(Rc::from(text.as_str())))
        }
    }
}
