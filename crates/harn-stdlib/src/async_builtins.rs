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
        let ms = args.first().and_then(|a| a.as_int()).unwrap_or(0);
        if ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(ms as u64)).await;
        }
        Ok(Value::Nil)
    });
}
