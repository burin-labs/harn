use std::collections::BTreeMap;

use harn_runtime::{Interpreter, RuntimeError, Value};

/// Register LLM-related builtins (llm_call, agent_loop).
pub fn register_llm_builtins(interp: &mut Interpreter) {
    // llm_call(prompt, system?, options?) → string
    interp.register_async_builtin("llm_call", |args| async move {
        let prompt = args.first().map(|a| a.as_string()).unwrap_or_default();
        let system = args.get(1).map(|a| a.as_string());
        let options = args.get(2).and_then(|a| a.as_dict()).cloned();

        let provider = resolve_provider(&options);
        let model = resolve_model(&options, &provider);
        let api_key = resolve_api_key(&provider)?;
        let max_tokens = options
            .as_ref()
            .and_then(|o| o.get("max_tokens"))
            .and_then(|v| v.as_int())
            .unwrap_or(4096);

        call_llm(
            &provider,
            &model,
            &api_key,
            &prompt,
            system.as_deref(),
            max_tokens,
        )
        .await
    });

    // agent_loop(prompt, system?, options?) → string
    // Full agent loop with tool dispatch, ##DONE## sentinel, nudging
    interp.register_async_builtin("agent_loop", |args| async move {
        let prompt = args.first().map(|a| a.as_string()).unwrap_or_default();
        let system = args.get(1).map(|a| a.as_string());
        let options = args.get(2).and_then(|a| a.as_dict()).cloned();

        let provider = resolve_provider(&options);
        let model = resolve_model(&options, &provider);
        let api_key = resolve_api_key(&provider)?;
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
            .map(|v| v.as_string());

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
            let response = call_llm_messages(
                &provider,
                &model,
                &api_key,
                &messages,
                if system_prompt.is_empty() {
                    None
                } else {
                    Some(&system_prompt)
                },
                4096,
            )
            .await?;

            let text = response.as_string();
            total_text.push_str(&text);

            // Append assistant response
            messages.push(serde_json::json!({
                "role": "assistant",
                "content": text,
            }));

            // Check for ##DONE## sentinel in persistent mode
            if persistent && text.contains("##DONE##") {
                break;
            }

            // In non-persistent mode, break on first response
            if !persistent {
                break;
            }

            // Persistent mode: nudge if text-only response
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

        Ok(Value::String(total_text))
    });
}

fn resolve_provider(options: &Option<BTreeMap<String, Value>>) -> String {
    options
        .as_ref()
        .and_then(|o| o.get("provider"))
        .map(|v| v.as_string())
        .unwrap_or_else(|| "anthropic".to_string())
}

fn resolve_model(options: &Option<BTreeMap<String, Value>>, provider: &str) -> String {
    options
        .as_ref()
        .and_then(|o| o.get("model"))
        .map(|v| v.as_string())
        .unwrap_or_else(|| match provider {
            "openai" => "gpt-4o".to_string(),
            "ollama" => "llama3.2".to_string(),
            "openrouter" => "anthropic/claude-sonnet-4-20250514".to_string(),
            _ => "claude-sonnet-4-20250514".to_string(),
        })
}

fn resolve_api_key(provider: &str) -> Result<String, RuntimeError> {
    match provider {
        "ollama" => Ok(String::new()), // Ollama doesn't need an API key
        "openai" => std::env::var("OPENAI_API_KEY").map_err(|_| {
            RuntimeError::thrown("Missing API key: set OPENAI_API_KEY environment variable")
        }),
        "openrouter" => std::env::var("OPENROUTER_API_KEY").map_err(|_| {
            RuntimeError::thrown("Missing API key: set OPENROUTER_API_KEY environment variable")
        }),
        _ => std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
            RuntimeError::thrown("Missing API key: set ANTHROPIC_API_KEY environment variable")
        }),
    }
}

#[allow(dead_code)]
fn resolve_base_url(provider: &str, options: &Option<BTreeMap<String, Value>>) -> String {
    // Allow custom base_url override
    if let Some(url) = options
        .as_ref()
        .and_then(|o| o.get("base_url"))
        .map(|v| v.as_string())
    {
        return url;
    }
    match provider {
        "ollama" => {
            std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string())
        }
        "openrouter" => "https://openrouter.ai/api".to_string(),
        "openai" => "https://api.openai.com".to_string(),
        _ => "https://api.anthropic.com".to_string(),
    }
}

/// Single-shot LLM call with a prompt string.
async fn call_llm(
    provider: &str,
    model: &str,
    api_key: &str,
    prompt: &str,
    system: Option<&str>,
    max_tokens: i64,
) -> Result<Value, RuntimeError> {
    let messages = vec![serde_json::json!({
        "role": "user",
        "content": prompt,
    })];
    call_llm_messages(provider, model, api_key, &messages, system, max_tokens).await
}

/// Multi-turn LLM call with a messages array.
async fn call_llm_messages(
    provider: &str,
    model: &str,
    api_key: &str,
    messages: &[serde_json::Value],
    system: Option<&str>,
    max_tokens: i64,
) -> Result<Value, RuntimeError> {
    let client = reqwest::Client::new();

    match provider {
        // OpenAI-compatible providers: openai, ollama, openrouter
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

            let response = req
                .send()
                .await
                .map_err(|e| RuntimeError::thrown(format!("{provider} API error: {e}")))?;

            let json: serde_json::Value = response.json().await.map_err(|e| {
                RuntimeError::thrown(format!("{provider} response parse error: {e}"))
            })?;

            if let Some(err) = json["error"]["message"].as_str() {
                return Err(RuntimeError::thrown(format!("{provider} API error: {err}")));
            }

            let text = json["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or("")
                .to_string();
            Ok(Value::String(text))
        }
        // Anthropic (default)
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
                .map_err(|e| RuntimeError::thrown(format!("Anthropic API error: {e}")))?;

            let json: serde_json::Value = response.json().await.map_err(|e| {
                RuntimeError::thrown(format!("Anthropic response parse error: {e}"))
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
                    return Err(RuntimeError::thrown(format!("Anthropic API error: {err}")));
                }
            }

            Ok(Value::String(text))
        }
    }
}
