use crate::value::{VmError, VmValue};

use super::api::LlmCallOptions;
pub(crate) async fn vm_stream_llm(
    opts: &LlmCallOptions,
    tx: &tokio::sync::mpsc::Sender<VmValue>,
) -> Result<(), VmError> {
    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let call = super::api::vm_call_llm_full_streaming(opts, delta_tx);
    tokio::pin!(call);
    loop {
        tokio::select! {
            result = &mut call => {
                while let Ok(delta) = delta_rx.try_recv() {
                    if tx
                        .send(VmValue::String(arcstr::ArcStr::from(delta)))
                        .await
                        .is_err()
                    {
                        return Ok(());
                    }
                }
                return result.map(|_| ());
            }
            Some(delta) = delta_rx.recv() => {
                if tx
                    .send(VmValue::String(arcstr::ArcStr::from(delta)))
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn partial_sse_transport_failure_is_not_success() {
        let _guard = crate::llm::env_guard();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind SSE stub");
        let addr = listener.local_addr().expect("SSE stub address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept SSE request");
            let mut request = [0_u8; 8192];
            let _ = stream.read(&mut request).expect("read SSE request");
            let event = "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{event}",
                event.len() + 100
            )
            .expect("write truncated SSE response");
        });

        let mut providers = crate::llm_config::ProvidersConfig::default();
        providers.providers.insert(
            "fixture".to_string(),
            crate::llm_config::ProviderDef {
                base_url: format!("http://{addr}"),
                auth_style: "none".to_string(),
                auth_env: crate::llm_config::AuthEnv::None,
                chat_endpoint: "/chat/completions".to_string(),
                ..Default::default()
            },
        );
        crate::llm_config::set_user_overrides(Some(providers));
        let previous_disabled = std::env::var_os(crate::llm::LLM_CALLS_DISABLED_ENV);
        unsafe {
            std::env::remove_var(crate::llm::LLM_CALLS_DISABLED_ENV);
        }

        let opts = LlmCallOptions {
            provider: "fixture".to_string(),
            model: "fixture-model".to_string(),
            messages: vec![serde_json::json!({"role": "user", "content": "test"})],
            ..Default::default()
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let result = vm_stream_llm(&opts, &tx).await;

        crate::llm_config::clear_user_overrides();
        match previous_disabled {
            Some(value) => unsafe {
                std::env::set_var(crate::llm::LLM_CALLS_DISABLED_ENV, value);
            },
            None => unsafe {
                std::env::remove_var(crate::llm::LLM_CALLS_DISABLED_ENV);
            },
        }
        server.join().expect("SSE stub thread");

        let mut partial = String::new();
        while let Ok(item) = rx.try_recv() {
            let VmValue::String(item) = item else {
                panic!("expected a string stream item");
            };
            partial.push_str(&item);
        }
        assert_eq!(partial, "p");
        assert!(
            result.is_err(),
            "a truncated SSE body after partial output must be a failure"
        );
        let failure = result
            .as_ref()
            .expect_err("truncated stream")
            .provider_stream_failure()
            .expect("typed provider stream failure");
        assert_eq!(failure.phase, crate::value::ProviderStreamPhase::Streaming);
        assert!(failure.partial);
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn zero_chunk_sse_transport_failure_is_typed_not_success() {
        let _guard = crate::llm::env_guard();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind SSE stub");
        let addr = listener.local_addr().expect("SSE stub address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept SSE request");
            let mut request = [0_u8; 8192];
            let _ = stream.read(&mut request).expect("read SSE request");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 100\r\nconnection: close\r\n\r\n"
            )
            .expect("write empty truncated SSE response");
        });

        let mut providers = crate::llm_config::ProvidersConfig::default();
        providers.providers.insert(
            "fixture".to_string(),
            crate::llm_config::ProviderDef {
                base_url: format!("http://{addr}"),
                auth_style: "none".to_string(),
                auth_env: crate::llm_config::AuthEnv::None,
                chat_endpoint: "/chat/completions".to_string(),
                ..Default::default()
            },
        );
        crate::llm_config::set_user_overrides(Some(providers));
        let previous_disabled = std::env::var_os(crate::llm::LLM_CALLS_DISABLED_ENV);
        unsafe {
            std::env::remove_var(crate::llm::LLM_CALLS_DISABLED_ENV);
        }
        let opts = LlmCallOptions {
            provider: "fixture".to_string(),
            model: "fixture-model".to_string(),
            messages: vec![serde_json::json!({"role": "user", "content": "test"})],
            ..Default::default()
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let result = vm_stream_llm(&opts, &tx).await;

        crate::llm_config::clear_user_overrides();
        match previous_disabled {
            Some(value) => unsafe {
                std::env::set_var(crate::llm::LLM_CALLS_DISABLED_ENV, value);
            },
            None => unsafe {
                std::env::remove_var(crate::llm::LLM_CALLS_DISABLED_ENV);
            },
        }
        server.join().expect("SSE stub thread");

        assert!(rx.try_recv().is_err(), "zero-chunk failure emitted a delta");
        let failure = result
            .expect_err("zero-chunk truncation must fail")
            .provider_stream_failure()
            .expect("typed provider stream failure")
            .clone();
        assert_eq!(
            failure.phase,
            crate::value::ProviderStreamPhase::AwaitingFirstChunk
        );
        assert!(!failure.partial);
    }
}
