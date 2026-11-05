//! A prompt's own probe through the route registry and the per-poll owner slot.

use super::*;

#[test]
fn prompt_grant_reaches_every_route_key_and_a_failed_probe_fails_the_next_call_fast() {
    let _env = crate::llm::env_guard();
    let _clock =
        crate::clock_mock::install_override(crate::clock_mock::MockClock::at_wall_ms(1_000));
    let mut opts = crate::llm::api::options::base_opts("prompt-probe-route");
    opts.model = "prompt-probe-model".into();
    for _ in 0..NETWORK_BREAKER_FAILURE_THRESHOLD {
        observe_network_outcome_for_llm_call(&opts, true);
    }
    assert!(network_breaker_admission(&opts).is_some());

    let prompt = begin_interactive_prompt("prompt-session");
    assert!(network_breaker_admission(&opts).is_none(), "probe admitted");
    observe_network_outcome_for_llm_call(&opts, true);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime");
    let error = runtime
        .block_on(await_network_breaker_for_llm_call(&opts))
        .expect_err("the turn fails instead of waiting");
    assert_eq!(
        crate::value::error_to_category(&error),
        crate::value::ErrorCategory::CircuitOpen
    );
    assert!(error.to_string().contains("not answering"), "{error}");
    drop(prompt);

    let (_, reason) = network_breaker_admission(&opts).expect("still open");
    assert_eq!(
        reason,
        BreakerOpenReason::Network,
        "another caller on the route waits as usual"
    );
}

#[test]
fn prompts_interleaving_on_one_thread_keep_their_own_probe_and_verdict() {
    use tokio::sync::oneshot::channel;

    let _env = crate::llm::env_guard();
    let mut opts = crate::llm::api::options::base_opts("interleaved-route");
    opts.model = "interleaved-model".into();
    for _ in 0..NETWORK_BREAKER_FAILURE_THRESHOLD {
        observe_network_outcome_for_llm_call(&opts, true);
    }
    let (a_begun, a_begun_seen) = channel();
    let (b_begun, b_begun_seen) = channel();
    let (a_probing, a_probing_seen) = channel();
    let (b_waited, b_waited_seen) = channel();
    let (a_failed, a_failed_seen) = channel();

    let prompt_a = crate::orchestration::scope_ambient_context(async {
        let _prompt = begin_interactive_prompt("prompt-a");
        a_begun.send(()).unwrap();
        b_begun_seen.await.unwrap();
        assert!(
            network_breaker_admission(&opts).is_none(),
            "a's own request takes a's offer, with b's prompt begun on top of it"
        );
        a_probing.send(()).unwrap();
        b_waited_seen.await.unwrap();
        observe_network_outcome_for_llm_call(&opts, true);
        a_failed.send(()).unwrap();
        await_network_breaker_for_llm_call(&opts)
            .await
            .expect_err("a's turn fails instead of waiting")
    });
    let prompt_b = crate::orchestration::scope_ambient_context(async {
        a_begun_seen.await.unwrap();
        let _prompt = begin_interactive_prompt("prompt-b");
        b_begun.send(()).unwrap();
        a_probing_seen.await.unwrap();
        let (remaining, reason) = network_breaker_admission(&opts).expect("a's probe is in flight");
        assert_eq!(remaining, Duration::ZERO);
        assert_eq!(reason, BreakerOpenReason::Network);
        b_waited.send(()).unwrap();
        a_failed_seen.await.unwrap();
        assert!(
            network_breaker_admission(&opts).is_none(),
            "b is not handed a's verdict and probes with its own offer"
        );
    });

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime");
    let (error, ()) = runtime.block_on(async { tokio::join!(prompt_a, prompt_b) });
    assert_eq!(
        crate::value::error_to_category(&error),
        crate::value::ErrorCategory::CircuitOpen
    );
    assert!(error.to_string().contains("not answering"), "{error}");
}

#[test]
fn a_worker_outliving_its_turn_cannot_act_for_the_sessions_next_prompt() {
    let _env = crate::llm::env_guard();
    let _clock =
        crate::clock_mock::install_override(crate::clock_mock::MockClock::at_wall_ms(1_000));
    let mut opts = crate::llm::api::options::base_opts("late-worker-route");
    opts.model = "late-worker-model".into();
    for _ in 0..NETWORK_BREAKER_FAILURE_THRESHOLD {
        observe_network_outcome_for_llm_call(&opts, true);
    }

    let first = begin_interactive_prompt("session");
    let worker_owner = swap_prompt_probe_owner(None);
    assert!(worker_owner.is_some());
    swap_prompt_probe_owner(worker_owner.clone());
    assert!(network_breaker_admission(&opts).is_none(), "first probe");
    observe_network_outcome_for_llm_call(&opts, true);
    drop(first);

    let next = begin_interactive_prompt("session");
    let next_owner = swap_prompt_probe_owner(worker_owner);
    let (_, reason) = network_breaker_admission(&opts).expect("the worker waits");
    assert_eq!(
        reason,
        BreakerOpenReason::Network,
        "the old turn's verdict went with it, and the next prompt's offer stays"
    );
    swap_prompt_probe_owner(next_owner);
    assert!(
        network_breaker_admission(&opts).is_none(),
        "the next prompt still has its own probe"
    );
    drop(next);
}

#[test]
fn a_prompt_cancelled_mid_probe_frees_the_route_at_once() {
    let _env = crate::llm::env_guard();
    let _clock =
        crate::clock_mock::install_override(crate::clock_mock::MockClock::at_wall_ms(1_000));
    let mut opts = crate::llm::api::options::base_opts("cancelled-probe-route");
    opts.model = "cancelled-probe-model".into();
    for _ in 0..NETWORK_BREAKER_FAILURE_THRESHOLD {
        observe_network_outcome_for_llm_call(&opts, true);
    }

    let cancelled = begin_interactive_prompt("cancelled");
    assert!(network_breaker_admission(&opts).is_none(), "probe admitted");
    let waiting = begin_interactive_prompt("waiting");
    let (remaining, _) = network_breaker_admission(&opts).expect("one probe at a time");
    assert_eq!(remaining, Duration::ZERO);

    drop(cancelled);
    assert!(
        network_breaker_admission(&opts).is_none(),
        "the waiting prompt probes without waiting out a takeover"
    );
    drop(waiting);
}
