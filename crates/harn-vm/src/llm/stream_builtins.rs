use crate::value::VmDictExt;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::value::{
    VmChannelCloseState, VmChannelHandle, VmError, VmStream, VmStreamCancel, VmValue,
};

use super::api;
use super::call::build_llm_error_dict;
use super::helpers::extract_llm_options;
use super::stream::vm_stream_llm;

pub(super) async fn llm_stream_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let opts = extract_llm_options(&args)?;
    let provider = opts.provider.clone();
    let prompt_text = opts
        .messages
        .last()
        .and_then(|m| m["content"].as_str())
        .unwrap_or("")
        .to_string();

    let (tx, rx) = tokio::sync::mpsc::channel::<VmValue>(64);
    let close = Arc::new(VmChannelCloseState::open());
    let close_for_task = close.clone();
    #[allow(clippy::arc_with_non_send_sync)]
    let tx_arc = Arc::new(tx);
    let tx_for_task = tx_arc.clone();

    tokio::task::spawn_local(async move {
        if provider == "mock" {
            let words: Vec<&str> = prompt_text.split_whitespace().collect();
            for word in &words {
                let _ = tx_for_task
                    .send(VmValue::String(std::sync::Arc::from(*word)))
                    .await;
            }
            close_for_task.close();
            return;
        }

        let result = vm_stream_llm(&opts, &tx_for_task).await;
        if let Err(e) = result {
            let _ = tx_for_task
                .send(VmValue::String(std::sync::Arc::from(format!("error: {e}"))))
                .await;
        }
        close_for_task.close();
    });

    #[allow(clippy::arc_with_non_send_sync)]
    let handle = VmChannelHandle {
        name: Arc::from("llm_stream"),
        sender: tx_arc,
        receiver: Arc::new(tokio::sync::Mutex::new(rx)),
        close,
    };
    Ok(VmValue::channel(handle))
}

fn llm_stream_chunk(
    delta: &str,
    visible_delta: &str,
    partial: &str,
    finish_reason: Option<&str>,
) -> VmValue {
    let mut dict = std::collections::BTreeMap::new();
    dict.put_str("delta", delta);
    dict.put_str("visible_delta", visible_delta);
    dict.put_str("partial", partial);
    dict.put_str("role", "assistant");
    dict.insert(
        "finish_reason".to_string(),
        finish_reason
            .map(|reason| VmValue::String(std::sync::Arc::from(reason.to_string())))
            .unwrap_or(VmValue::Nil),
    );
    VmValue::dict(dict)
}

async fn forward_llm_stream_delta(
    stream_tx: &tokio::sync::mpsc::Sender<Result<VmValue, VmError>>,
    visible: &mut crate::visible_text::VisibleTextState,
    delta: String,
) -> Result<String, ()> {
    let (partial, visible_delta) = visible.push(&delta, true);
    let chunk = llm_stream_chunk(&delta, &visible_delta, &partial, None);
    stream_tx.send(Ok(chunk)).await.map_err(|_| ())?;
    Ok(partial)
}

async fn send_llm_stream_error(
    stream_tx: &tokio::sync::mpsc::Sender<Result<VmValue, VmError>>,
    err: VmError,
    provider: &str,
    model: &str,
) {
    let wrapped = VmError::Thrown(build_llm_error_dict(&err, provider, model));
    let _ = stream_tx.send(Err(wrapped)).await;
}

/// Shared implementation of `llm_stream_call`: a first-class `Stream`
/// of structured chunks using `llm_call`'s provider error taxonomy.
pub(super) async fn llm_stream_call_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let opts = extract_llm_options(&args)?;
    let provider = opts.provider.clone();
    let model = opts.model.clone();

    let (stream_tx, stream_rx) = tokio::sync::mpsc::channel::<Result<VmValue, VmError>>(64);
    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let cancel = VmStreamCancel::new();
    let mut cancel_rx = cancel.subscribe();
    let mut first_token = super::first_token::FirstTokenTimer::for_current_span();

    tokio::task::spawn_local(async move {
        let mut visible = crate::visible_text::VisibleTextState::default();
        let mut partial = String::new();
        let mut deltas_open = true;
        let mut llm_task = tokio::task::spawn_local(async move {
            api::vm_call_llm_full_streaming(&opts, delta_tx).await
        });

        loop {
            tokio::select! {
                _ = cancel_rx.changed() => {
                    llm_task.abort();
                    break;
                }
                _ = stream_tx.closed() => {
                    llm_task.abort();
                    break;
                }
                maybe_delta = delta_rx.recv(), if deltas_open => {
                    match maybe_delta {
                        Some(delta) => {
                            first_token.observe_delta();
                            match forward_llm_stream_delta(&stream_tx, &mut visible, delta).await {
                                Ok(next_partial) => partial = next_partial,
                                Err(()) => {
                                    llm_task.abort();
                                    break;
                                }
                            }
                        }
                        None => deltas_open = false,
                    }
                }
                joined = &mut llm_task => {
                    while let Ok(delta) = delta_rx.try_recv() {
                        first_token.observe_delta();
                        match forward_llm_stream_delta(&stream_tx, &mut visible, delta).await {
                            Ok(next_partial) => partial = next_partial,
                            Err(()) => break,
                        }
                    }
                    match joined {
                        Ok(Ok(result)) => {
                            let final_chunk = llm_stream_chunk(
                                "",
                                "",
                                &partial,
                                result.stop_reason.as_deref(),
                            );
                            let _ = stream_tx.send(Ok(final_chunk)).await;
                        }
                        Ok(Err(err)) => {
                            send_llm_stream_error(&stream_tx, err, &provider, &model).await;
                        }
                        Err(join_err) if join_err.is_cancelled() => {}
                        Err(join_err) => {
                            let err = VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
                                "llm_stream_call background task failed: {join_err}"
                            ))));
                            send_llm_stream_error(&stream_tx, err, &provider, &model).await;
                        }
                    }
                    break;
                }
            }
        }
    });

    Ok(VmValue::stream(VmStream {
        done: Arc::new(AtomicBool::new(false)),
        receiver: Arc::new(tokio::sync::Mutex::new(stream_rx)),
        cancel: Some(cancel),
    }))
}

#[cfg(test)]
mod tests {
    use crate::value::VmDictExt;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::Duration;

    use crate::llm::fake::{install_fake_llm_script, FakeLlmEvent, FakeLlmScript, FakeStopReason};
    use crate::tracing::SpanKind;
    use crate::value::{VmError, VmValue};

    use super::llm_stream_call_impl;

    #[tokio::test(start_paused = true)]
    async fn first_token_budget_records_streaming_ttft_under_virtual_time() {
        crate::llm::reset_llm_state();
        crate::tracing::set_tracing_enabled(true);
        let local = tokio::task::LocalSet::new();
        let stall = Duration::from_millis(1_500);

        local
            .run_until(async move {
                let _guard = install_fake_llm_script(FakeLlmScript::streaming(vec![
                    FakeLlmEvent::Stall(stall),
                    FakeLlmEvent::Token("hello".into()),
                    FakeLlmEvent::Done(FakeStopReason::EndTurn),
                ]));

                let span_id =
                    crate::tracing::span_start(SpanKind::LlmCall, "llm_stream_call".into());
                let stream = match llm_stream_call_impl(fake_stream_args()).await? {
                    VmValue::Stream(stream) => stream,
                    other => {
                        return Err(VmError::Runtime(format!(
                            "expected stream, got {}",
                            other.type_name()
                        )));
                    }
                };
                crate::tracing::span_end(span_id);

                let mut receiver = stream.receiver.lock().await;
                let first_chunk =
                    tokio::time::timeout(stall + Duration::from_millis(250), receiver.recv())
                        .await
                        .expect("first stream chunk should arrive within the TTFT budget")
                        .expect("stream should produce first chunk")?;
                assert_eq!(dict_string(&first_chunk, "delta").as_deref(), Some("hello"));
                drop(first_chunk);

                let profile = crate::profile::build(&crate::tracing::peek_spans());
                let first_token_ms = profile
                    .first_token_ms
                    .expect("profile should include first_token_ms");
                assert!(
                    first_token_ms >= 1_500,
                    "first token should include fake provider stall, got {first_token_ms}ms"
                );
                assert!(
                    first_token_ms < 1_750,
                    "stream assembly overhead should stay under 250ms, got {first_token_ms}ms"
                );
                Ok::<(), VmError>(())
            })
            .await
            .expect("streaming first-token budget test should pass");
    }

    fn fake_stream_args() -> Vec<VmValue> {
        let mut options = BTreeMap::new();
        options.put_str("provider", "fake");
        options.put_str("model", "fake");
        vec![
            VmValue::String(Arc::from("hello".to_string())),
            VmValue::Nil,
            VmValue::dict(options),
        ]
    }

    fn dict_string(value: &VmValue, key: &str) -> Option<String> {
        let VmValue::Dict(dict) = value else {
            return None;
        };
        dict.get(key).map(VmValue::display)
    }
}
