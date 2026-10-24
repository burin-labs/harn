use std::time::Duration;

use super::*;

// The heuristic tokenizer counts four plain letters as one token, so each
// `WORD` is exactly one estimated token on a model tiktoken does not know.
const WORD: &str = "word";

fn meter(model: &str) -> InferenceMeter {
    let meter = InferenceMeter::new(None, "call-1", "test-provider", model);
    meter.start();
    meter
}

/// Stream `chunks` chunks of `tokens_per_chunk` tokens, one every `every`.
async fn stream(meter: &InferenceMeter, chunks: usize, tokens_per_chunk: usize, every: Duration) {
    for _ in 0..chunks {
        meter.observe(OutputKind::Text, &WORD.repeat(tokens_per_chunk));
        tokio::time::advance(every).await;
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn live_rate_measures_decode_and_leaves_out_the_wait_for_the_first_token() {
    let meter = meter("live-rate-model");
    tokio::time::advance(Duration::from_secs(2)).await;
    stream(&meter, 20, 10, Duration::from_millis(100)).await;

    let speed = meter.snapshot();
    assert_eq!(speed.time_to_first_token_ms, Some(2000));
    assert_eq!(speed.phase, InferencePhase::Responding);
    assert_eq!(speed.token_source, TokenCountSource::Estimated);
    // 10 tokens per 100ms. Dividing by the 4s since dispatch would read ~50.
    let rate = speed.output_tokens_per_second.expect("a measured rate");
    assert!((95.0..=105.0).contains(&rate), "rate {rate}");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn final_snapshot_uses_the_provider_count_over_the_streamed_span() {
    let meter = meter("final-rate-model");
    tokio::time::advance(Duration::from_secs(1)).await;
    stream(&meter, 21, 10, Duration::from_millis(100)).await;
    let mut result = LlmResult {
        output_tokens: 300,
        ..LlmResult::default()
    };

    meter.finish(&mut result);

    let speed = result.telemetry.inference_speed.expect("settled speed");
    assert!(speed.is_final);
    assert_eq!(speed.phase, InferencePhase::Complete);
    assert_eq!(speed.token_source, TokenCountSource::Provider);
    assert_eq!(speed.output_tokens, 300);
    assert_eq!(speed.time_to_first_token_ms, Some(1000));
    // 299 tokens after the first chunk over the 2.0s from first to last.
    assert_eq!(speed.output_tokens_per_second, Some(149.5));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_stream_that_arrives_in_one_burst_has_no_rate() {
    let meter = meter("burst-model");
    tokio::time::advance(Duration::from_secs(1)).await;
    meter.observe(OutputKind::Text, &WORD.repeat(125));
    let mut result = LlmResult {
        output_tokens: 125,
        ..LlmResult::default()
    };

    meter.finish(&mut result);

    let speed = result.telemetry.inference_speed.expect("settled speed");
    assert_eq!(speed.output_tokens, 125);
    assert_eq!(speed.output_tokens_per_second, None);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn server_generation_timings_outrank_the_client_clock() {
    let meter = meter("server-timing-model");
    stream(&meter, 10, 10, Duration::from_millis(100)).await;
    let mut result = LlmResult {
        output_tokens: 100,
        ..LlmResult::default()
    };
    result.telemetry.server_output_tokens = Some(100);
    result.telemetry.server_generation_ms = Some(500);

    meter.finish(&mut result);

    let speed = result.telemetry.inference_speed.expect("settled speed");
    assert_eq!(speed.token_source, TokenCountSource::Server);
    assert_eq!(speed.output_tokens_per_second, Some(200.0));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn reasoning_the_stream_never_showed_does_not_inflate_the_rate_or_calibration() {
    let model = "hidden-reasoning-model";
    let meter = meter(model);
    stream(&meter, 11, 5, Duration::from_millis(100)).await;
    let mut result = LlmResult {
        output_tokens: 2000,
        ..LlmResult::default()
    };

    meter.finish(&mut result);

    let speed = result.telemetry.inference_speed.expect("settled speed");
    assert_eq!(speed.output_tokens, 2000);
    // 54 streamed tokens after the first over 1.0s, not 1999 over 1.0s.
    assert_eq!(speed.output_tokens_per_second, Some(54.0));
    assert_eq!(route_calibration("test-provider", model), 1.0);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn exact_counts_calibrate_the_next_estimate_on_the_same_route() {
    let model = "calibrated-model";
    let first = meter(model);
    stream(&first, 10, 10, Duration::from_millis(100)).await;
    let mut result = LlmResult {
        output_tokens: 150,
        ..LlmResult::default()
    };
    first.finish(&mut result);

    let second = meter(model);
    second.observe(OutputKind::Text, &WORD.repeat(100));

    assert_eq!(second.snapshot().output_tokens, 150);
    assert_eq!(route_calibration("test-provider", "unrelated-model"), 1.0);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn reasoning_and_tool_arguments_count_as_output_and_set_the_phase() {
    let meter = meter("phase-model");
    meter.observe(OutputKind::Reasoning, &WORD.repeat(10));
    assert_eq!(meter.snapshot().phase, InferencePhase::Reasoning);
    meter.observe(OutputKind::ToolArguments, &WORD.repeat(10));
    let speed = meter.snapshot();
    assert_eq!(speed.phase, InferencePhase::CallingTool);
    assert_eq!(speed.output_tokens, 20);
}

#[test]
fn only_the_settled_snapshot_is_persisted() {
    let event = |is_final| AgentEvent::InferenceSpeed {
        session_id: "s".into(),
        call_id: "c".into(),
        provider: "p".into(),
        model: "m".into(),
        speed: InferenceSpeed {
            phase: InferencePhase::Responding,
            is_final,
            elapsed_ms: 10,
            time_to_first_token_ms: Some(5),
            output_tokens: 1,
            output_tokens_per_second: None,
            token_source: TokenCountSource::Estimated,
        },
    };
    let mut projector = agent_events::DurableAgentEventProjector::new();
    assert!(!projector.should_persist(&event(false)));
    assert!(projector.should_persist(&event(true)));
}

struct Phases(Arc<Mutex<Vec<InferencePhase>>>);
impl agent_events::AgentEventSink for Phases {
    fn handle_event(&self, event: &AgentEvent) {
        if let AgentEvent::InferenceSpeed { speed, .. } = event {
            self.0.lock().unwrap().push(speed.phase);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn live_snapshots_from_the_text_tee_reach_the_prompts_transport_without_holding_it() {
    let phases = Arc::new(Mutex::new(Vec::new()));
    let sink: Arc<dyn agent_events::AgentEventSink> = Arc::new(Phases(phases.clone()));
    let transport = agent_events::AgentEventTransport::new(sink.clone());
    let session = format!("inference-speed-tee-{}", uuid::Uuid::now_v7());
    let (downstream, mut forwarded) = tokio::sync::mpsc::unbounded_channel();
    let (meter, (text, tee)) = transport
        .scope(async {
            let meter = InferenceMeter::new(Some(session), "call-1", "test-provider", "tee-model");
            meter.start();
            let tee = meter.tee_text(downstream);
            (meter, tee)
        })
        .await;

    text.send(WORD.to_string()).unwrap();
    drop(text);
    tee.await.unwrap();

    assert_eq!(forwarded.recv().await.as_deref(), Some(WORD));
    assert_eq!(
        *phases.lock().unwrap(),
        [
            InferencePhase::AwaitingFirstToken,
            InferencePhase::Responding
        ]
    );
    // A meter that outlives its prompt must not keep the prompt's output open.
    drop(transport);
    assert_eq!(Arc::strong_count(&sink), 1);
    drop(meter);
}
