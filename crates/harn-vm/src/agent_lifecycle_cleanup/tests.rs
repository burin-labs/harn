use super::*;

fn key(runtime: usize, session: &str) -> LifecycleKey {
    LifecycleKey {
        execution_id: format!("execution-{runtime}"),
        session_runtime: runtime,
        host_runtime: runtime + 1,
        session_id: session.to_string(),
        run_id: "run".to_string(),
    }
}

#[test]
fn registry_distinguishes_same_task_id_across_runtime_owners() {
    let mut registry = LifecycleRegistry::default();
    registry.reserve(key(10, "a"), "task_root".into()).unwrap();
    registry.reserve(key(20, "b"), "task_root".into()).unwrap();

    assert_eq!(
        registry.activate_task(
            &RuntimeKey {
                execution_id: "execution-10".to_string(),
                session_runtime: 10,
                host_runtime: 11,
            },
            "task_root"
        ),
        CleanupActivation::Scheduled { sessions: 1 }
    );
    assert_eq!(
        registry.activate_task(
            &RuntimeKey {
                execution_id: "execution-20".to_string(),
                session_runtime: 20,
                host_runtime: 21,
            },
            "task_root"
        ),
        CleanupActivation::Scheduled { sessions: 1 }
    );
    assert_eq!(
        registry.activate_task(
            &RuntimeKey {
                execution_id: "execution-10".to_string(),
                session_runtime: 10,
                host_runtime: 11,
            },
            "task_root"
        ),
        CleanupActivation::AlreadyScheduled
    );
    assert_eq!(
        registry.task_reservation_count(
            &RuntimeKey {
                execution_id: "execution-10".to_string(),
                session_runtime: 10,
                host_runtime: 11,
            },
            "task_root"
        ),
        1,
        "deduplication must retain, rather than consume, the recovery owner"
    );
}

#[test]
fn registry_refuses_capacity_before_accepting_an_unowned_lifecycle() {
    let mut registry = LifecycleRegistry::default();
    for index in 0..MAX_LIFECYCLE_RESERVATIONS {
        registry
            .reserve(key(index * 2, &format!("session-{index}")), "task".into())
            .unwrap();
    }
    assert_eq!(
        registry.reserve(key(usize::MAX - 1, "overflow"), "task".into()),
        Err(LifecycleAdmissionError::AtCapacity {
            pending: MAX_LIFECYCLE_RESERVATIONS
        })
    );
}

#[tokio::test]
async fn recovery_keeps_its_owner_beyond_the_old_retry_threshold() {
    let task_id = format!("retry-threshold-{}", uuid::Uuid::now_v7());
    let key = LifecycleKey {
        execution_id: "retry-execution".to_string(),
        session_runtime: 0xCA11,
        host_runtime: 0xCA12,
        session_id: task_id.clone(),
        run_id: "run".to_string(),
    };
    let runtime = RuntimeKey::from(&key);
    lock_registry()
        .reserve(key.clone(), task_id.clone())
        .expect("reserve test lifecycle");
    assert!(matches!(
        lock_registry().activate_task(&runtime, &task_id),
        CleanupActivation::Scheduled { sessions: 1 }
    ));
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cleanup_attempts = attempts.clone();
    let cleanup_key = key.clone();
    retry_task_cleanup(
        task_id.clone(),
        runtime.clone(),
        move || {
            let attempt = cleanup_attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            let cleanup_key = cleanup_key.clone();
            async move {
                if attempt <= 17 {
                    return Err(VmError::Runtime("injected persistence failure".to_string()));
                }
                lock_registry().release(&cleanup_key);
                Ok(())
            }
        },
        |_| std::future::ready(()),
    )
    .await;

    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 18);
    assert_eq!(
        lock_registry().task_reservation_count(&runtime, &task_id),
        0
    );
}
