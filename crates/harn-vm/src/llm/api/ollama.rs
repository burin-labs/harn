//! Ollama-specific env-driven overrides consumed by both the dedicated
//! Ollama provider and any Ollama-flavored completion path.

pub(crate) fn ollama_num_ctx_override() -> Option<u64> {
    for key in [
        "HARN_OLLAMA_NUM_CTX",
        "OLLAMA_CONTEXT_LENGTH",
        "OLLAMA_NUM_CTX",
    ] {
        if let Ok(raw) = std::env::var(key) {
            if let Ok(parsed) = raw.trim().parse::<u64>() {
                if parsed > 0 {
                    return Some(parsed);
                }
            }
        }
    }
    None
}

pub(crate) fn ollama_keep_alive_override() -> Option<serde_json::Value> {
    for key in ["HARN_OLLAMA_KEEP_ALIVE", "OLLAMA_KEEP_ALIVE"] {
        if let Ok(raw) = std::env::var(key) {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                return Some(match trimmed.to_ascii_lowercase().as_str() {
                    "default" => serde_json::json!("30m"),
                    "forever" | "infinite" | "-1" => serde_json::json!(-1),
                    _ => {
                        if let Ok(n) = trimmed.parse::<i64>() {
                            serde_json::json!(n)
                        } else {
                            serde_json::json!(trimmed)
                        }
                    }
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{ollama_keep_alive_override, ollama_num_ctx_override};
    use crate::llm::env_lock;

    #[test]
    fn ollama_num_ctx_override_prefers_harn_env() {
        let _guard = env_lock().lock().expect("env lock");
        unsafe {
            std::env::set_var("HARN_OLLAMA_NUM_CTX", "131072");
            std::env::remove_var("OLLAMA_CONTEXT_LENGTH");
            std::env::remove_var("OLLAMA_NUM_CTX");
        }
        assert_eq!(ollama_num_ctx_override(), Some(131072));
        unsafe {
            std::env::remove_var("HARN_OLLAMA_NUM_CTX");
        }
    }

    #[test]
    fn ollama_keep_alive_override_normalizes_forever() {
        let _guard = env_lock().lock().expect("env lock");
        unsafe {
            std::env::set_var("HARN_OLLAMA_KEEP_ALIVE", "forever");
            std::env::remove_var("OLLAMA_KEEP_ALIVE");
        }
        assert_eq!(ollama_keep_alive_override(), Some(serde_json::json!(-1)));
        unsafe {
            std::env::remove_var("HARN_OLLAMA_KEEP_ALIVE");
        }
    }
}
