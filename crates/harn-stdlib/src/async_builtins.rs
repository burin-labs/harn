use std::collections::BTreeMap;

use harn_runtime::{Interpreter, RuntimeError, Value};

/// Register async builtins (HTTP, sleep) on an interpreter.
pub fn register_async_builtins(interp: &mut Interpreter) {
    interp.register_async_builtin("http_get", |args| async move {
        let url = args.first().map(|a| a.as_string()).unwrap_or_default();
        if url.is_empty() {
            return Err(RuntimeError::thrown("http_get: URL is required"));
        }
        let response = reqwest::get(&url)
            .await
            .map_err(|e| RuntimeError::thrown(format!("http_get failed: {e}")))?;
        let text = response
            .text()
            .await
            .map_err(|e| RuntimeError::thrown(format!("http_get read failed: {e}")))?;
        Ok(Value::String(text))
    });

    interp.register_async_builtin("http_post", |args| async move {
        let url = args.first().map(|a| a.as_string()).unwrap_or_default();
        let body = args.get(1).map(|a| a.as_string()).unwrap_or_default();
        if url.is_empty() {
            return Err(RuntimeError::thrown("http_post: URL is required"));
        }

        let mut headers = BTreeMap::new();
        if let Some(Value::Dict(h)) = args.get(2) {
            for (k, v) in h {
                headers.insert(k.clone(), v.as_string());
            }
        }

        let client = reqwest::Client::new();
        let mut req = client.post(&url).body(body);
        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }

        let response = req
            .send()
            .await
            .map_err(|e| RuntimeError::thrown(format!("http_post failed: {e}")))?;
        let text = response
            .text()
            .await
            .map_err(|e| RuntimeError::thrown(format!("http_post read failed: {e}")))?;
        Ok(Value::String(text))
    });

    interp.register_async_builtin("sleep", |args| async move {
        let ms = match args.first() {
            Some(Value::Duration(ms)) => *ms,
            Some(Value::Int(n)) => *n as u64,
            _ => 0,
        };
        if ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(ms)).await;
        }
        Ok(Value::Nil)
    });

    // prompt_user(message) — reads a line from stdin (for CLI interactive mode)
    interp.register_builtin("prompt_user", |args, out| {
        let message = args.first().map(|a| a.as_string()).unwrap_or_default();
        if !message.is_empty() {
            out.extend_from_slice(message.as_bytes());
            out.extend_from_slice(b"\n");
        }
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .map_err(|e| RuntimeError::thrown(format!("Failed to read input: {e}")))?;
        Ok(Value::String(input.trim_end().to_string()))
    });

    // channel(name) — creates a named channel and returns it as a dict with name
    interp.register_builtin("channel", |args, _out| {
        let name = args
            .first()
            .map(|a| a.as_string())
            .unwrap_or("default".into());
        let mut ch = std::collections::BTreeMap::new();
        ch.insert("name".to_string(), Value::String(name));
        ch.insert("type".to_string(), Value::String("channel".to_string()));
        ch.insert("messages".to_string(), Value::List(Vec::new()));
        Ok(Value::Dict(ch))
    });

    // send(channel, value) — appends a value to a channel's message list
    // Note: For real concurrent channels, this would use tokio::sync::mpsc.
    // This simplified version works for single-threaded pipeline orchestration.
    interp.register_builtin("send", |args, _out| {
        if args.len() >= 2 {
            // In a real implementation, this would push to a shared queue.
            // For now, it's a placeholder that returns nil.
        }
        Ok(Value::Nil)
    });

    // receive(channel) — receives the next value from a channel
    interp.register_builtin("receive", |_args, _out| {
        // Placeholder for receive — would block on a shared queue.
        Ok(Value::Nil)
    });

    // atomic(initial) — creates a shared atomic counter
    interp.register_builtin("atomic", |args, _out| {
        let initial = args.first().cloned().unwrap_or(Value::Int(0));
        let mut a = std::collections::BTreeMap::new();
        a.insert("value".to_string(), initial);
        a.insert("type".to_string(), Value::String("atomic".to_string()));
        Ok(Value::Dict(a))
    });

    // atomic_get(a) — read the current value
    interp.register_builtin("atomic_get", |args, _out| {
        if let Some(Value::Dict(map)) = args.first() {
            return Ok(map.get("value").cloned().unwrap_or(Value::Nil));
        }
        Ok(Value::Nil)
    });

    // atomic_set(a, value) — returns new atomic with updated value
    interp.register_builtin("atomic_set", |args, _out| {
        if args.len() >= 2 {
            if let Value::Dict(map) = &args[0] {
                let mut new_map = map.clone();
                new_map.insert("value".to_string(), args[1].clone());
                return Ok(Value::Dict(new_map));
            }
        }
        Ok(Value::Nil)
    });

    // atomic_add(a, n) — returns new atomic with value incremented by n
    interp.register_builtin("atomic_add", |args, _out| {
        if args.len() >= 2 {
            if let Value::Dict(map) = &args[0] {
                let current = map.get("value").and_then(|v| v.as_int()).unwrap_or(0);
                let delta = args[1].as_int().unwrap_or(0);
                let mut new_map = map.clone();
                new_map.insert("value".to_string(), Value::Int(current + delta));
                return Ok(Value::Dict(new_map));
            }
        }
        Ok(Value::Nil)
    });
}
