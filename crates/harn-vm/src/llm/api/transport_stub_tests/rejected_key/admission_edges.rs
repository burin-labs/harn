use super::*;

#[test]
fn revoked_model_access_does_not_reject_the_key_for_other_models() {
    assert_model_access_refusal("Access to model forbidden-model has been revoked");
}

#[test]
fn explicitly_revoked_api_key_blocks_other_models() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _durable = ScopedEnvVar::set("HARN_LLM_RATE_LIMIT_DURABLE", "0");
    crate::reset_thread_local_state();
    let _cleanup = Cleanup;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let server = model_access_stub(seen.clone(), "This API key has been revoked");
        install(server.addr(), crate::llm_config::AuthEnv::None);
        let mut opts = options("revoked-key");
        opts.model = "forbidden-model".into();
        let first = vm_call_llm_full(&opts).await.unwrap_err();
        assert_auth_failure(&first);
        assert_eq!(
            field(&first, "credential_scope").map(|v| v.display()),
            Some("key".to_string())
        );
        opts.model = "allowed-model".into();
        let second = vm_call_llm_full(&opts).await.unwrap_err();
        assert_auth_failure(&second);
        assert_eq!(
            field(&second, "attempt_count").and_then(|v| v.as_int()),
            Some(0)
        );
        assert_eq!(*seen.lock().unwrap(), ["forbidden-model"]);
    });
}

#[test]
fn a_queued_request_rechecks_the_key_after_its_sibling_is_rejected() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _durable = ScopedEnvVar::set("HARN_LLM_RATE_LIMIT_DURABLE", "0");
    crate::reset_thread_local_state();
    let _cleanup = Cleanup;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let accept = Arc::new(AtomicBool::new(false));
        let (received_tx, received_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = gated_auth_stub(seen.clone(), accept.clone(), Some((received_tx, release_rx)));
        let mut release = ReleaseBody(Some(release_tx));
        install(server.addr(), crate::llm_config::AuthEnv::None);
        crate::llm::rate_limit::set_rate_limits(PROVIDER, crate::llm_config::RateLimitsDef {
            concurrency: Some(1),
            ..Default::default()
        });
        let opts = options("key-a");
        let mut first = Box::pin(vm_call_llm_full(&opts));
        tokio::select! {
            received = received_rx => received.unwrap(),
            result = &mut first => panic!("first request ended before its response body: {result:?}"),
        }
        assert_eq!(*seen.lock().unwrap(), ["Bearer key-a"]);
        let mut second = Box::pin(vm_call_llm_full(&opts));
        assert!(futures::poll!(&mut second).is_pending(), "the sibling holds the only permit");
        assert_eq!(crate::llm::rate_limit::provider_request_usage(PROVIDER), 1);
        release.release();
        let (first, second) = tokio::join!(first, second);
        assert_auth_failure(&first.unwrap_err());
        assert_refused_locally(&second.unwrap_err());
        assert_eq!(*seen.lock().unwrap(), ["Bearer key-a"], "the queued key was never re-sent");

        accept.store(true, Ordering::SeqCst);
        assert_eq!(vm_call_llm_full(&options("key-b")).await.unwrap().text, "served");
        assert_eq!(*seen.lock().unwrap(), ["Bearer key-a", "Bearer key-b"],
            "the local refusal released its concurrency permit");
    });
}
