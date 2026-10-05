use super::*;
use harn_clock::test_support::within;

#[test]
fn typed_200k_tpm_receipt_paces_growing_requests_before_provider() {
    let mut limiter = RouteLimiter::new(EffectiveRateLimits::default());
    let mut now_ms = 0u128;
    limiter.observe_token_quota(
        now_ms,
        ProviderTokenQuotaReceipt {
            limit: 200_000,
            used: 190_000,
            requested: 17_000,
            window_ms: 60_000,
        },
    );

    for requested in [17_000, 27_000, 37_000, 47_000] {
        let request = RateLimitRequest {
            input_tokens: requested,
            output_tokens: 0,
        };
        let wait = limiter
            .check(now_ms, request)
            .expect("growing request must pace before exceeding observed quota");
        now_ms = now_ms.saturating_add(wait.as_millis());
        let live = limiter
            .observed_token_quota
            .expect("quota retained")
            .usage_at(now_ms);
        assert!(
            live.saturating_add(requested) <= 200_000,
            "provider would still 429 after proactive wait: used={live} requested={requested}"
        );
        limiter.record(now_ms, request);
    }
}

fn install_quota_overlay() {
    let overlay = crate::llm_config::parse_config_toml(
            "[providers.quota]\n\
             base_url = \"https://quota.invalid/v1\"\n\
             chat_endpoint = \"/chat/completions\"\n\
             rate_limits = { rpm = 9, tpm = 900, concurrency = 2 }\n\
             \n\
             [models.\"quota-model\"]\n\
             name = \"Quota Model\"\n\
             provider = \"quota\"\n\
             context_window = 32768\n\
             rate_limits = { rpm = 7, tpm = 700, input_tpm = 300, output_tpm = 400, concurrency = 1 }\n",
        )
        .expect("quota overlay parses");
    crate::llm_config::set_user_overrides(Some(overlay));
}

fn install_concurrency_overlay() {
    let overlay = crate::llm_config::parse_config_toml(
        "[providers.queue]\n\
             base_url = \"https://queue.invalid/v1\"\n\
             chat_endpoint = \"/chat/completions\"\n\
             rate_limits = { rpm = 2, concurrency = 1 }\n",
    )
    .expect("queue overlay parses");
    crate::llm_config::set_user_overrides(Some(overlay));
}

fn install_durable_overlay() {
    let overlay = crate::llm_config::parse_config_toml(
        "[providers.durable]\n\
             base_url = \"https://durable.invalid/v1\"\n\
             chat_endpoint = \"/chat/completions\"\n\
             rate_limits = { rpm = 1 }\n",
    )
    .expect("durable overlay parses");
    crate::llm_config::set_user_overrides(Some(overlay));
}

fn reset_test_rate_limit_state() {
    reset_rate_limit_state();
    crate::llm_config::clear_user_overrides();
}

struct EnvVarGuard {
    key: &'static str,
    old: Option<String>,
}

impl EnvVarGuard {
    fn set_value(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let old = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, old }
    }

    fn set_path(key: &'static str, value: &std::path::Path) -> Self {
        Self::set_value(key, value)
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = self.old.as_ref() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn durable_usage(path: &std::path::Path, key: &str) -> u64 {
    if !path.exists() {
        return 0;
    }
    let conn = rusqlite::Connection::open(path).expect("open durable rate limit db");
    conn.query_row(
        "SELECT COALESCE(SUM(units), 0)
             FROM durable_rate_limit_entries
             WHERE bucket_key = ?1",
        rusqlite::params![key],
        |row| row.get::<_, i64>(0),
    )
    .expect("query durable usage")
    .max(0) as u64
}

#[test]
fn sliding_window_allows_weighted_tokens_within_limit() {
    let mut window = SlidingWindow::new(10);
    assert!(window.check(0, 4).is_none());
    window.record(0, 4);
    assert!(window.check(0, 6).is_none());
    window.record(0, 6);
    assert!(window.check(0, 1).is_some());
}

#[test]
fn sliding_window_waits_until_enough_weight_expires() {
    let mut window = SlidingWindow::new(10);
    window.record(0, 4);
    window.record(10_000, 6);
    let wait = window.check(10_000, 4).expect("window should be full");
    assert_eq!(wait.as_secs(), 50);
}

#[test]
fn sliding_window_expires_entries_at_window_boundary() {
    let mut window = SlidingWindow::new(10);
    window.record(0, 10);
    assert!(window.check(59_999, 1).is_some());
    assert!(window.check(60_000, 1).is_none());
}

#[test]
fn oversized_token_reservation_charges_one_full_window() {
    let mut window = SlidingWindow::new(10);
    assert!(window.check(0, 25).is_none());
    window.record(0, 25);
    assert_eq!(window.usage(), 10);
    assert!(window.check(0, 1).is_some());
}

#[test]
fn retry_after_cooldown_blocks_route_without_catalog_limit() {
    let mut limiter = RouteLimiter::new(EffectiveRateLimits::default());
    limiter.observe_retry_after(1_000, 2_500);

    let wait = limiter
        .check(1_000, RateLimitRequest::default())
        .expect("cooldown should block route");
    assert_eq!(wait.as_millis(), 2_500);
    assert!(
        limiter.check(3_500, RateLimitRequest::default()).is_none(),
        "cooldown should expire exactly at the provider-supplied deadline"
    );
}

#[test]
fn retry_after_cooldown_extends_existing_route_cooldown() {
    let mut limiter = RouteLimiter::new(EffectiveRateLimits::default());
    limiter.observe_retry_after(1_000, 1_000);
    limiter.observe_retry_after(1_500, 3_000);

    let wait = limiter
        .check(2_000, RateLimitRequest::default())
        .expect("extended cooldown should block route");
    assert_eq!(wait.as_millis(), 2_500);
}

#[test]
fn init_from_config_loads_model_rate_limits_from_catalog_overlay() {
    let _guard = crate::llm::env_guard();
    reset_test_rate_limit_state();
    install_quota_overlay();
    init_from_config();
    let provider_limits = get_rate_limits("quota").expect("provider limits");
    assert_eq!(provider_limits.rpm, Some(9));
    assert_eq!(provider_limits.tpm, Some(900));
    assert_eq!(provider_limits.concurrency, Some(2));
    let model_limits = get_model_rate_limits("quota", "quota-model").expect("model limits");
    assert_eq!(model_limits.rpm, Some(7));
    assert_eq!(model_limits.tpm, Some(700));
    assert_eq!(model_limits.input_tpm, Some(300));
    assert_eq!(model_limits.output_tpm, Some(400));
    assert_eq!(model_limits.concurrency, Some(1));
    reset_test_rate_limit_state();
}

#[test]
fn lazy_config_initialization_has_one_winner_and_preserves_live_state() {
    let _guard = crate::llm::env_guard();
    reset_test_rate_limit_state();
    let _queue_limit = EnvVarGuard::set_value("HARN_RATE_LIMIT_QUEUE", "2");

    let candidate_ready = std::sync::Arc::new(std::sync::Barrier::new(2));
    let resume_stale_initializer = std::sync::Arc::new(std::sync::Barrier::new(2));
    let stale_initializer = {
        let candidate_ready = std::sync::Arc::clone(&candidate_ready);
        let resume_stale_initializer = std::sync::Arc::clone(&resume_stale_initializer);
        crate::runtime_stack::spawn(move || {
            ensure_initialized_from_config_with(|| {
                let candidate = limiters_from_config_and_runtime_overrides();
                candidate_ready.wait();
                resume_stale_initializer.wait();
                candidate
            });
        })
    };

    // The spawned caller has observed an uninitialized registry and built
    // the candidate it would install. Let the main caller win initialization,
    // then open a live route breaker before the stale caller resumes.
    candidate_ready.wait();
    ensure_initialized_from_config();
    let route_key = provider_key("queue");
    let now_ms = 1_000;
    let breaker_was_opened = {
        let mut registry = registry().lock().expect("rate limiter mutex poisoned");
        let limiter = registry
            .limiters
            .get_mut(&route_key)
            .expect("winning initializer installed queue route");
        for _ in 0..UNPRODUCTIVE_COMPLETION_BREAKER_THRESHOLD {
            limiter.observe_unproductive_completion(now_ms);
        }
        limiter.breaker_block(now_ms).is_some()
    };

    resume_stale_initializer.wait();
    stale_initializer
        .join()
        .expect("stale initializer thread completed");

    let breaker_survived_stale_initializer = registry()
        .lock()
        .expect("rate limiter mutex poisoned")
        .limiters
        .get_mut(&route_key)
        .expect("queue route remains installed")
        .breaker_block(now_ms)
        .is_some();
    assert!(
        breaker_was_opened,
        "winning caller must open the live breaker"
    );
    assert!(
        breaker_survived_stale_initializer,
        "a stale lazy initializer must not replace live route state"
    );

    // Explicit reload remains destructive by design; only lazy
    // initialization is single-winner and non-destructive.
    init_from_config();
    assert!(
        registry()
            .lock()
            .expect("rate limiter mutex poisoned")
            .limiters
            .get_mut(&route_key)
            .expect("explicit reload reinstalls queue route")
            .breaker_block(now_ms)
            .is_none(),
        "explicit init_from_config must continue to reload route state"
    );

    reset_test_rate_limit_state();
}

#[test]
fn provider_env_override_sets_tpm() {
    let _guard = crate::llm::env_guard();
    reset_test_rate_limit_state();
    install_quota_overlay();
    std::env::set_var("HARN_RATE_LIMIT_QUOTA_TPM", "1000000");
    init_from_config();
    let limits = get_rate_limits("quota").expect("provider limits");
    assert_eq!(limits.rpm, Some(9));
    assert_eq!(limits.tpm, Some(1_000_000));
    std::env::remove_var("HARN_RATE_LIMIT_QUOTA_TPM");
    reset_test_rate_limit_state();
}

#[test]
fn legacy_provider_rpm_env_still_sets_provider_bucket() {
    let _guard = crate::llm::env_guard();
    reset_rate_limit_state();
    std::env::set_var("HARN_RATE_LIMIT_TESTPROVIDER", "42");
    init_from_config();
    assert_eq!(get_rate_limit("testprovider"), Some(42));
    std::env::remove_var("HARN_RATE_LIMIT_TESTPROVIDER");
    reset_test_rate_limit_state();
}

#[test]
fn concurrency_queue_does_not_consume_request_quota_until_started() {
    let _guard = crate::llm::env_guard();
    let _durable_disabled = EnvVarGuard::set_value(DURABLE_RATE_LIMIT_ENABLED_ENV, "0");
    reset_test_rate_limit_state();
    install_concurrency_overlay();
    init_from_config();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("current-thread runtime");

    runtime.block_on(async {
        let first = acquire_permit("queue").await.expect("first permit");
        assert_eq!(provider_request_usage("queue"), 1);

        let mut second = tokio::spawn(async { acquire_permit("queue").await });
        // The second acquire must stay parked behind the first permit.
        // Poll under a real-time timeout instead of counting yields —
        // yield counting is scheduler-sensitive, and when this fires it
        // also surfaces *what* completed instead of a bare is_finished.
        if let Ok(join) =
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut second).await
        {
            let outcome = match join {
                Ok(Ok(_permit)) => "a second permit was granted".to_string(),
                Ok(Err(error)) => format!("acquire failed: {error:?}"),
                Err(join_error) => format!("task panicked: {join_error}"),
            };
            panic!("second acquire completed while the first permit was held ({outcome})");
        }
        assert_eq!(provider_request_usage("queue"), 1);

        drop(first);
        let second = within("second task acquiring after the first permit drops", second)
            .await
            .expect("second task completed")
            .expect("second permit");
        assert_eq!(provider_request_usage("queue"), 2);
        drop(second);
    });

    reset_test_rate_limit_state();
}

#[test]
fn durable_concurrency_queue_does_not_consume_request_quota_until_started() {
    let _guard = crate::llm::env_guard();
    reset_test_rate_limit_state();
    install_concurrency_overlay();
    let temp = tempfile::tempdir().expect("tempdir");
    let state_path = temp.path().join("llm-rate-limits.sqlite");
    let _env = EnvVarGuard::set_path(DURABLE_RATE_LIMIT_STATE_PATH_ENV, &state_path);
    let _clock =
        crate::clock_mock::install_override(crate::clock_mock::MockClock::at_wall_ms(1_000));
    init_from_config();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("current-thread runtime");

    runtime.block_on(async {
        let first = acquire_permit("queue").await.expect("first permit");
        assert_eq!(durable_usage(&state_path, "llm:provider:queue:rpm"), 1);

        let mut second = tokio::spawn(async { acquire_permit("queue").await });
        // See concurrency_queue_does_not_consume_request_quota_until_started
        // for why this polls under a timeout instead of counting yields.
        if let Ok(join) =
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut second).await
        {
            let outcome = match join {
                Ok(Ok(_permit)) => "a second permit was granted".to_string(),
                Ok(Err(error)) => format!("acquire failed: {error:?}"),
                Err(join_error) => format!("task panicked: {join_error}"),
            };
            panic!("second acquire completed while the first permit was held ({outcome})");
        }
        assert_eq!(durable_usage(&state_path, "llm:provider:queue:rpm"), 1);

        drop(first);
        let second = within("second task acquiring after the first permit drops", second)
            .await
            .expect("second task completed")
            .expect("second permit");
        assert_eq!(durable_usage(&state_path, "llm:provider:queue:rpm"), 2);
        drop(second);
    });

    reset_test_rate_limit_state();
}

#[test]
fn durable_state_path_coordinates_after_process_local_reset() {
    let _guard = crate::llm::env_guard();
    reset_test_rate_limit_state();
    install_durable_overlay();
    let temp = tempfile::tempdir().expect("tempdir");
    let _env = EnvVarGuard::set_path(
        DURABLE_RATE_LIMIT_STATE_PATH_ENV,
        &temp.path().join("llm-rate-limits.sqlite"),
    );
    // This test asserts the UNCAPPED full-window coordination wait, so it
    // explicitly disables the per-acquire backoff clamp (default-ON, see
    // durable_backoff_is_clamped_per_acquire_so_one_route_cannot_eat_the_wall).
    let _cap = EnvVarGuard::set_value(DURABLE_RATE_LIMIT_MAX_WAIT_MS_ENV, "0");
    let _clock =
        crate::clock_mock::install_override(crate::clock_mock::MockClock::at_wall_ms(1_000));
    init_from_config();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("current-thread runtime");

    runtime.block_on(async {
        let first = acquire_permit("durable").await.expect("first permit");
        drop(first);

        reset_rate_limit_state();
        init_from_config();

        let before = crate::clock_mock::now_ms();
        let second = acquire_permit("durable").await.expect("second permit");
        let after = crate::clock_mock::now_ms();
        drop(second);

        assert!(
            after.saturating_sub(before) >= 60_000,
            "second process-local registry should wait on durable SQLite state"
        );
    });

    reset_test_rate_limit_state();
}

// ---------------------------------------------------------------------
// Mechanism-fitness: durable rate-limit backoff is BOUNDED per acquire.
//
// A sustained-quota provider (the field case: cerebras:gpt-oss-120b
// returning a full per-minute window wait on nearly every call) must NOT be
// able to make one LLM call block out the whole sliding window. Without the
// clamp the durable acquire waits the full ~60s; with the default-ON cap it
// returns after the ceiling so the call can attempt the provider and let a
// real 429 drive the retry/escalation path. This pins that the wait cannot
// dominate: a saturated bucket waits ~cap, never the full window.
//
// Sibling `durable_state_path_coordinates_after_process_local_reset` pins
// the UNCAPPED >= 60_000 wait (it sets the cap env to 0); the two together
// bracket the mechanism so a refactor cannot silently drop the clamp.
// ---------------------------------------------------------------------
#[test]
fn durable_backoff_is_clamped_per_acquire_so_one_route_cannot_eat_the_wall() {
    let _guard = crate::llm::env_guard();
    reset_test_rate_limit_state();
    install_durable_overlay();
    let temp = tempfile::tempdir().expect("tempdir");
    let _env = EnvVarGuard::set_path(
        DURABLE_RATE_LIMIT_STATE_PATH_ENV,
        &temp.path().join("llm-rate-limits.sqlite"),
    );
    // Bound any one acquire's durable backoff to 5s, well under the 60s
    // sliding window the rpm=1 overlay would otherwise force a second call
    // to wait out.
    let cap_ms: u64 = 5_000;
    let _cap = EnvVarGuard::set_value(DURABLE_RATE_LIMIT_MAX_WAIT_MS_ENV, cap_ms.to_string());
    let _clock =
        crate::clock_mock::install_override(crate::clock_mock::MockClock::at_wall_ms(1_000));
    init_from_config();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("current-thread runtime");

    runtime.block_on(async {
        // First acquire consumes the single rpm slot for the window.
        let first = acquire_permit("durable").await.expect("first permit");
        drop(first);

        // The second acquire saturates the durable rpm bucket and would, with
        // an unbounded wait, sleep the full 60s window. The clamp must cap it.
        let before = crate::clock_mock::now_ms();
        let second = acquire_permit("durable").await.expect("second permit");
        let after = crate::clock_mock::now_ms();
        drop(second);

        let waited = after.saturating_sub(before).max(0) as u64;
        assert!(
            waited <= cap_ms.saturating_add(1_000),
            "durable backoff must be clamped to ~{cap_ms}ms, but one acquire waited {waited}ms \
                 — a sustained-quota route would otherwise dominate the trial wall"
        );
        assert!(
            waited < 60_000,
            "durable backoff must NOT wait the full {WINDOW_SECS}s window (waited {waited}ms)"
        );
    });

    reset_test_rate_limit_state();
}

// ---------------------------------------------------------------------
// Reset-scope contract.
//
// The rate-limiter registry is process-global. Two reset levels exist and
// MUST stay decoupled:
//
//   * `reset_llm_state` (runs from parallel in-process unit tests) only
//     scrubs leaked *runtime overrides* — it must NOT wipe a config-derived
//     registry, or it would corrupt a concurrently asserting sibling test.
//   * `reset_rate_limit_state` (the full wipe, reached in sequential /
//     separate-process contexts via `reset_thread_local_state`) clears
//     everything, including retry-after cooldowns.
//
// These two tests pin each half of that contract so a future refactor can't
// silently re-merge them — the failure mode was a leaked cooldown stalling a
// later conformance test's mocked LLM call under a paused clock for the full
// per-test timeout.
// ---------------------------------------------------------------------

#[test]
fn full_registry_reset_clears_retry_after_cooldown() {
    let _guard = crate::llm::env_guard();
    reset_test_rate_limit_state();
    install_quota_overlay();
    init_from_config();

    let keys = limiter_keys("quota", "quota-model");
    let request = RateLimitRequest::default();
    let now_ms = 0;

    // A provider 429 installs a long retry-after cooldown on the route.
    {
        let mut registry = registry().lock().expect("registry");
        for key in &keys {
            limiter_for_key(&mut registry.limiters, key).observe_retry_after(now_ms, 60_000);
        }
        assert!(
            check_wait_for_keys(&mut registry, &keys, request, now_ms).is_some(),
            "cooldown should force a wait before reset"
        );
    }

    // The full wipe (what `reset_thread_local_state` performs between
    // sequential tests) must clear the cooldown so the next test's call
    // doesn't stall on a paused clock.
    reset_rate_limit_state();
    init_from_config();
    {
        let mut registry = registry().lock().expect("registry");
        assert!(
            check_wait_for_keys(&mut registry, &keys, request, now_ms).is_none(),
            "cooldown must not survive a full registry reset"
        );
    }

    reset_test_rate_limit_state();
}

#[test]
fn runtime_override_reset_preserves_config_registry() {
    let _guard = crate::llm::env_guard();
    reset_test_rate_limit_state();
    install_quota_overlay();
    init_from_config();

    let provider_bucket = provider_key("quota");
    assert!(
        registry()
            .lock()
            .expect("registry")
            .limiters
            .contains_key(&provider_bucket),
        "config init should populate the provider limiter"
    );

    // The override-scoped reset (what `reset_llm_state` runs from parallel
    // unit tests, with no runtime override installed) must leave the
    // config-derived registry untouched — otherwise it would wipe usage
    // counters a concurrent rate-limit test is asserting on.
    reset_runtime_rate_limit_overrides();

    let registry = registry().lock().expect("registry");
    assert!(
        registry.initialized_from_config,
        "config-init flag must survive an override-only reset"
    );
    assert!(
        registry.limiters.contains_key(&provider_bucket),
        "config limiters must survive an override-only reset"
    );
    drop(registry);

    reset_test_rate_limit_state();
}

#[test]
fn runtime_override_reset_preserves_unrelated_provider_usage() {
    let _guard = crate::llm::env_guard();
    let _durable_disabled = EnvVarGuard::set_value(DURABLE_RATE_LIMIT_ENABLED_ENV, "0");
    reset_test_rate_limit_state();
    install_concurrency_overlay();
    init_from_config();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("current-thread runtime");

    runtime.block_on(async {
        let first = acquire_permit("queue").await.expect("first permit");
        assert_eq!(provider_request_usage("queue"), 1);

        let limits = crate::llm_config::RateLimitsDef {
            rpm: Some(1),
            ..Default::default()
        };
        set_rate_limits("runtime-only-provider", limits);
        reset_runtime_rate_limit_overrides();

        assert_eq!(
            provider_request_usage("queue"),
            1,
            "clearing an unrelated runtime override must not wipe queued provider usage"
        );

        let mut second = tokio::spawn(async { acquire_permit("queue").await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut second)
                .await
                .is_err(),
            "second acquire should remain queued behind the held concurrency permit"
        );

        drop(first);
        let second = within("second task acquiring after the first permit drops", second)
            .await
            .expect("second task completed")
            .expect("second permit");
        assert_eq!(provider_request_usage("queue"), 2);
        drop(second);
    });

    reset_test_rate_limit_state();
}

// ---- network circuit breaker (pure state-machine tests) ----------------

#[test]
fn breaker_opens_after_threshold_consecutive_network_failures() {
    let mut b = NetworkBreaker::default();
    // Below threshold: still closed, calls admitted.
    for i in 1..NETWORK_BREAKER_FAILURE_THRESHOLD {
        b.record_network_failure(u128::from(i));
        assert!(
            b.admit(u128::from(i)).is_none(),
            "must stay closed below threshold ({i} failures)"
        );
    }
    // Crossing the threshold opens it.
    b.record_network_failure(100);
    let (blocked, reason) = b.admit(100).expect("breaker must be open at threshold");
    assert_eq!(reason, BreakerOpenReason::Network);
    assert!(
        blocked.as_millis() > 0 && blocked.as_millis() <= u128::from(NETWORK_BREAKER_OPEN_MS),
        "open window remaining {}ms out of (0, {NETWORK_BREAKER_OPEN_MS}]",
        blocked.as_millis()
    );
}

#[test]
fn breaker_opens_after_threshold_consecutive_unproductive_completions() {
    let mut b = NetworkBreaker::default();
    // Below threshold: still closed, calls admitted.
    for i in 1..UNPRODUCTIVE_COMPLETION_BREAKER_THRESHOLD {
        assert_eq!(b.record_unproductive_completion(u128::from(i)), None);
        assert!(
            b.admit(u128::from(i)).is_none(),
            "must stay closed below threshold ({i} empties)"
        );
    }
    // Crossing the threshold opens it with the circuit_open-flavored reason.
    assert_eq!(
        b.record_unproductive_completion(100),
        Some(UNPRODUCTIVE_BREAKER_OPEN_MS)
    );
    let (blocked, reason) = b.admit(100).expect("breaker must be open at threshold");
    assert_eq!(reason, BreakerOpenReason::UnproductiveCompletion);
    assert!(
        blocked.as_millis() > 0 && blocked.as_millis() <= u128::from(UNPRODUCTIVE_BREAKER_OPEN_MS),
        "open window remaining {}ms out of (0, {UNPRODUCTIVE_BREAKER_OPEN_MS}]",
        blocked.as_millis()
    );
}

#[test]
fn unproductive_breaker_open_window_grows_per_reopen() {
    let mut b = NetworkBreaker::default();
    // First trip: base window (measured at the instant it opened).
    for attempt in 0..UNPRODUCTIVE_COMPLETION_BREAKER_THRESHOLD {
        let expected = (attempt + 1 == UNPRODUCTIVE_COMPLETION_BREAKER_THRESHOLD)
            .then_some(UNPRODUCTIVE_BREAKER_OPEN_MS);
        assert_eq!(b.record_unproductive_completion(0), expected);
    }
    let (first, _) = b.admit(0).expect("open after first trip");
    assert_eq!(first.as_millis(), u128::from(UNPRODUCTIVE_BREAKER_OPEN_MS));
    // Elapse the window, admit the half-open probe, then have it empty again:
    // the window must have grown (doubling), not stayed flat, so a dead lane
    // is probed a bounded number of times instead of every base window.
    let after = u128::from(UNPRODUCTIVE_BREAKER_OPEN_MS) + 1;
    assert!(b.admit(after).is_none(), "half-open probe admitted");
    assert_eq!(
        b.record_unproductive_completion(after),
        Some(UNPRODUCTIVE_BREAKER_OPEN_MS.saturating_mul(2))
    );
    let (second, reason) = b.admit(after).expect("re-open after failed probe");
    assert_eq!(reason, BreakerOpenReason::UnproductiveCompletion);
    assert_eq!(
        second.as_millis(),
        u128::from(UNPRODUCTIVE_BREAKER_OPEN_MS.saturating_mul(2)),
        "window must double on the second unproductive open"
    );
}

#[test]
fn unproductive_breaker_success_resets_streak_and_reason() {
    let mut b = NetworkBreaker::default();
    assert_eq!(b.record_unproductive_completion(0), None);
    assert_eq!(b.record_unproductive_completion(0), None);
    b.record_success();
    assert_eq!(b.consecutive_unproductive_completions, 0);
    assert_eq!(b.unproductive_opens, 0);
    assert_eq!(b.open_reason, BreakerOpenReason::Network);
    // One post-success empty must not be enough to re-open (streak reset).
    assert_eq!(b.record_unproductive_completion(0), None);
    assert!(
        b.admit(0).is_none(),
        "single empty after a productive serve must stay closed"
    );
}

/// End-to-end wiring for the empty-completion storm fix: feeding a route the
/// same terminal unproductive completions `observed_llm_call` surfaces on a
/// dead escalation/judge lane opens the always-on route breaker, so the
/// NEXT dispatch fails fast with a `circuit_open` error (failover-eligible;
/// the agent loop degrades onto the primary/cheap result) instead of
/// re-dispatching the lane 18-43x per trial. Governor-independent: no
/// `HARN_LLM_RATE_GOVERNOR` here, unlike harn#4023's throttle-gated path.
#[test]
fn unproductive_completion_feed_opens_route_breaker_and_fails_fast_with_circuit_open() {
    // Serialize with the other env-guarded rate-limit tests (some of which
    // globally clear the registry) so none wipes this route mid-run. Use a
    // dedicated provider id so we never touch — or need to reset — another
    // test's limiter state (a global reset here would race the concurrency
    // quota tests, exactly what `reset_runtime_rate_limit_overrides` warns
    // about).
    let _guard = crate::llm::env_guard();
    let transcript_dir = tempfile::tempdir().expect("transcript tempdir");
    crate::llm::agent_observe::push_llm_transcript_dir(
        transcript_dir.path().to_str().expect("utf8 tempdir"),
    );

    let mut opts = crate::llm::api::options::base_opts("storm-test-provider");
    opts.model = "storm-test-model".to_string();

    // Below the threshold the route stays admitted (a couple of empties is
    // tolerated before the lane is declared dead).
    for _ in 0..UNPRODUCTIVE_COMPLETION_BREAKER_THRESHOLD - 1 {
        observe_unproductive_completion_for_llm_call(&opts, "empty_generation");
        assert!(
            check_network_breaker_for_llm_call(&opts).is_ok(),
            "route must stay admitted below the unproductive-completion threshold"
        );
    }

    // Crossing the threshold trips the breaker: the next dispatch fails fast
    // with `circuit_open`, having made zero further provider calls.
    observe_unproductive_completion_for_llm_call(&opts, "empty_generation");
    crate::llm::agent_observe::pop_llm_transcript_dir();
    let err = check_network_breaker_for_llm_call(&opts)
        .expect_err("route breaker must fail fast after the unproductive streak");
    assert_eq!(
        crate::value::error_to_category(&err),
        crate::value::ErrorCategory::CircuitOpen,
        "sustained empty completions must surface as circuit_open (failover-eligible)"
    );
    let crate::value::VmError::Thrown(crate::value::VmValue::Dict(fields)) = &err else {
        panic!("quarantine must be a structured zero-dispatch error: {err:?}");
    };
    assert_eq!(
        fields.get("code").map(crate::value::VmValue::display),
        Some("route_quarantined".to_string())
    );
    assert_eq!(
        fields
            .get("attempt_count")
            .and_then(crate::value::VmValue::as_int),
        Some(0),
        "an open breaker performs no provider request"
    );

    let mut sibling = opts.clone();
    sibling.model = "storm-test-model-alternate".to_string();
    assert!(
        check_network_breaker_for_llm_call(&sibling).is_ok(),
        "empty generations quarantine one provider/model route, not its provider siblings"
    );

    let transcript = std::fs::read_to_string(transcript_dir.path().join("llm_transcript.jsonl"))
        .expect("quarantine receipt");
    let receipts: Vec<serde_json::Value> = transcript
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid receipt JSON"))
        .filter(|event: &serde_json::Value| event["type"] == "route_quarantined")
        .collect();
    assert_eq!(receipts.len(), 1, "one receipt per open transition");
    assert_eq!(receipts[0]["schema"], "harn.llm.route_quarantine.v1");
    assert_eq!(receipts[0]["receipt_kind"], "route_quarantined");
    assert_eq!(receipts[0]["provider"], "storm-test-provider");
    assert_eq!(receipts[0]["model"], "storm-test-model");
    assert_eq!(receipts[0]["reason"], "empty_generation");
    assert_eq!(receipts[0]["cooldown_ms"], UNPRODUCTIVE_BREAKER_OPEN_MS);

    // A genuinely productive serve heals the lane and closes the breaker.
    observe_network_outcome_for_llm_call(&opts, false);
    assert!(
        check_network_breaker_for_llm_call(&opts).is_ok(),
        "a productive serve must close the breaker"
    );
}

#[test]
fn network_recovery_reserves_probes_only_when_every_route_key_is_ready() {
    let _env = crate::llm::env_guard();
    let _clock =
        crate::clock_mock::install_override(crate::clock_mock::MockClock::at_wall_ms(1_000));
    ensure_initialized_from_config();
    let mut opts = crate::llm::api::options::base_opts("atomic-network-probe");
    opts.model = "delayed-model".into();
    let keys = limiter_keys(&opts.provider, &opts.model);
    let fail = |key: &str| {
        let mut registry = registry().lock().expect("registry");
        let breaker = &mut limiter_for_key(&mut registry.limiters, key).breaker;
        for _ in 0..NETWORK_BREAKER_FAILURE_THRESHOLD {
            breaker.record_network_failure(crate::clock_mock::instant_now().as_millis());
        }
    };
    fail(&keys[0]);
    crate::clock_mock::advance(Duration::from_millis(NETWORK_BREAKER_OPEN_MS));
    fail(&keys[1]);
    assert!(network_breaker_admission(&opts).is_some());
    assert!(
        matches!(
            registry().lock().expect("registry").limiters[&keys[0]]
                .breaker
                .state,
            BreakerState::Open { .. }
        ),
        "a blocked model must not strand a provider probe reservation"
    );
    crate::clock_mock::advance(Duration::from_millis(NETWORK_BREAKER_OPEN_MS));
    assert!(network_breaker_admission(&opts).is_none());
    assert!(
        network_breaker_admission(&opts).is_some(),
        "only one probe is admitted"
    );
}

#[test]
fn breaker_fails_fast_while_open_then_half_opens_then_closes_on_probe_success() {
    let mut b = NetworkBreaker::default();
    let open_at = 1_000u128;
    for _ in 0..NETWORK_BREAKER_FAILURE_THRESHOLD {
        b.record_network_failure(open_at);
    }
    // Fail fast inside the open window.
    assert!(
        b.admit(open_at + 1).is_some(),
        "must fail fast while open window is active"
    );
    // After the window elapses, exactly one half-open probe is admitted...
    let after = open_at + u128::from(NETWORK_BREAKER_OPEN_MS) + 1;
    assert!(
        b.admit(after).is_none(),
        "half-open probe must be admitted once the window elapses"
    );
    // ...and a concurrent caller while the probe is in flight is blocked.
    assert!(
        b.admit(after).is_some(),
        "second concurrent call must not get a second half-open probe"
    );
    // A successful probe closes the breaker and clears the failure count.
    b.record_success();
    assert!(
        b.admit(after).is_none(),
        "breaker must close after probe success"
    );
    assert_eq!(b.consecutive_network_failures, 0);
}

#[test]
fn breaker_reopens_when_half_open_probe_fails() {
    let mut b = NetworkBreaker::default();
    let open_at = 0u128;
    for _ in 0..NETWORK_BREAKER_FAILURE_THRESHOLD {
        b.record_network_failure(open_at);
    }
    let after = u128::from(NETWORK_BREAKER_OPEN_MS) + 1;
    assert!(b.admit(after).is_none(), "half-open probe admitted");
    // The probe fails (still no network): the breaker re-opens immediately,
    // even though the *count* logic alone is irrelevant in half-open state.
    b.record_network_failure(after);
    assert!(
        b.admit(after + 1).is_some(),
        "a failed half-open probe must re-open the breaker"
    );
}

#[test]
fn network_breaker_open_window_grows_per_reopen() {
    let mut b = NetworkBreaker::default();
    for _ in 0..NETWORK_BREAKER_FAILURE_THRESHOLD {
        b.record_network_failure(0);
    }
    let (first, reason) = b.admit(0).expect("first trip opens breaker");
    assert_eq!(reason, BreakerOpenReason::Network);
    assert_eq!(first.as_millis(), u128::from(NETWORK_BREAKER_OPEN_MS));

    let after_first = u128::from(NETWORK_BREAKER_OPEN_MS) + 1;
    assert!(
        b.admit(after_first).is_none(),
        "first half-open probe admitted"
    );
    b.record_network_failure(after_first);
    let (second, _) = b
        .admit(after_first)
        .expect("second failed probe reopens breaker");
    assert_eq!(
        second.as_millis(),
        u128::from(NETWORK_BREAKER_OPEN_MS_SECOND)
    );

    let after_second = after_first + u128::from(NETWORK_BREAKER_OPEN_MS_SECOND) + 1;
    assert!(
        b.admit(after_second).is_none(),
        "second half-open probe admitted"
    );
    b.record_network_failure(after_second);
    let (third, _) = b
        .admit(after_second)
        .expect("third failed probe reopens breaker");
    assert_eq!(third.as_millis(), u128::from(NETWORK_BREAKER_OPEN_MS_MAX));
}

#[test]
fn breaker_success_resets_failure_streak() {
    let mut b = NetworkBreaker::default();
    b.record_network_failure(0);
    b.record_network_failure(0);
    b.record_success();
    assert_eq!(b.consecutive_network_failures, 0);
    assert_eq!(b.network_opens, 0);
    // One post-success failure must not be enough to re-open (streak reset).
    b.record_network_failure(0);
    assert!(
        b.admit(0).is_none(),
        "single failure after reset must stay closed"
    );
}

#[test]
fn breaker_does_not_open_on_rate_limit_or_server_errors() {
    // 429 / 5xx must NOT feed the breaker. Drive the same number of NON-network
    // outcomes well past the threshold and assert it never opens. (The wiring
    // in agent_observe only calls `observe_network_failure` for true network
    // failures; here we assert the classifier that gates that call.)
    use super::super::agent_observe::is_network_failure_llm_error;
    use crate::value::{ErrorCategory, VmError, VmValue};

    let rate_limited = VmError::CategorizedError {
        message: "429 too many requests".to_string(),
        category: ErrorCategory::RateLimit,
    };
    let server_error = VmError::CategorizedError {
        message: "503 service unavailable".to_string(),
        category: ErrorCategory::ServerError,
    };
    let thrown_429 = VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
        "[rate_limited] too many requests",
    )));
    assert!(
        !is_network_failure_llm_error(&rate_limited),
        "429 is not a network failure"
    );
    assert!(
        !is_network_failure_llm_error(&server_error),
        "5xx is not a network failure"
    );
    assert!(
        !is_network_failure_llm_error(&thrown_429),
        "thrown 429 is not a network failure"
    );

    // A genuine network/timeout failure IS one.
    let connect = VmError::CategorizedError {
        message: "openai request error (connect): connection refused".to_string(),
        category: ErrorCategory::TransientNetwork,
    };
    let timeout = VmError::CategorizedError {
        message: "openai request error (timeout): operation timed out".to_string(),
        category: ErrorCategory::Timeout,
    };
    assert!(
        is_network_failure_llm_error(&connect),
        "connect failure is a network failure"
    );
    assert!(
        is_network_failure_llm_error(&timeout),
        "timeout is a network failure"
    );
}
