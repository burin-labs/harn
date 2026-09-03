use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::value::{VmDictExt, VmError, VmTaskHandle, VmValue};

async fn install_claimed_journal(
    vm: &crate::vm::Vm,
    root: &std::path::Path,
    session_id: &str,
    task_id: &str,
) {
    let mut options = crate::value::DictMap::new();
    options.put_str("root", root.to_string_lossy().as_ref());
    let prepared = crate::agent_session_journal::prepare(
        session_id,
        &options,
        format!("run-{session_id}"),
        format!("turn-{session_id}"),
    )
    .await
    .expect("prepare journal");
    crate::agent_sessions::open_or_create_for_test(Some(session_id.to_string()));
    crate::agent_sessions::install_journal(session_id, prepared.state).expect("install journal");
    crate::agent_sessions::claim_journal_task(
        session_id,
        vm.execution_id(),
        task_id.to_string(),
        true,
    )
    .expect("claim lifecycle");
}

/// Wait on the recovery boundary itself. `progress` is subscribed before the
/// join that triggers cleanup, so no signal is missed, and the bound inside
/// `settle_cleanup` fails the test when recovery never releases the
/// reservation.
async fn assert_cleanup_released(
    mut progress: tokio::sync::watch::Receiver<u64>,
    session_id: &str,
) {
    crate::agent_lifecycle_cleanup::settle_cleanup(
        &mut progress,
        || {
            !crate::agent_sessions::has_journal(session_id)
                && !crate::agent_sessions::exists(session_id)
        },
        &format!("task join left the lifecycle reservation visible for `{session_id}`"),
    )
    .await;
    assert!(!crate::agent_sessions::has_journal(session_id));
    assert!(!crate::agent_sessions::exists(session_id));
}

fn failed_task(task_id: &str, message: &'static str) -> VmTaskHandle {
    VmTaskHandle {
        handle: tokio::spawn(async move { Err(VmError::Runtime(message.to_string())) }),
        cancel_token: Arc::new(AtomicBool::new(false)),
        wait_task_id: task_id.to_string(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn scoped_first_child_failure_releases_its_lifecycle_owner() {
    crate::agent_sessions::reset_session_store();
    crate::llm::agent_session_host::reset_agent_session_host_state();
    let root = tempfile::tempdir().expect("temp root");
    let mut vm = crate::vm::Vm::new();
    let session_id = "scope-failed-child-cleanup";
    let task_id = "task_scope_failed_child";
    install_claimed_journal(&vm, root.path(), session_id, task_id).await;

    let cleanup_progress = crate::agent_lifecycle_cleanup::subscribe_cleanup_progress();
    vm.spawned_tasks.insert(
        "public-scope-child".to_string(),
        failed_task(task_id, "scope boom"),
    );
    vm.task_scopes.push(crate::vm::TaskScope {
        task_ids: vec!["public-scope-child".to_string()],
        frame_depth: 0,
        env_scope_depth: 0,
    });

    let error = vm
        .execute_task_scope_exit()
        .await
        .expect_err("scope must propagate its first child failure");
    assert_eq!(error.to_string(), "Runtime error: scope boom");
    assert_cleanup_released(cleanup_progress, session_id).await;
    crate::agent_sessions::reset_session_store();
    crate::llm::agent_session_host::reset_agent_session_host_state();
}

#[tokio::test(flavor = "current_thread")]
async fn graceful_cancel_early_failure_releases_its_lifecycle_owner() {
    crate::agent_sessions::reset_session_store();
    crate::llm::agent_session_host::reset_agent_session_host_state();
    let root = tempfile::tempdir().expect("temp root");
    let mut vm = crate::vm::Vm::new();
    crate::stdlib::register_vm_stdlib(&mut vm);
    let session_id = "graceful-cancel-failed-child-cleanup";
    let task_id = "task_graceful_cancel_failed_child";
    install_claimed_journal(&vm, root.path(), session_id, task_id).await;

    let cleanup_progress = crate::agent_lifecycle_cleanup::subscribe_cleanup_progress();
    vm.spawned_tasks.insert(
        "public-graceful-child".to_string(),
        failed_task(task_id, "graceful boom"),
    );
    let handled = vm
        .try_call_special_name(
            "cancel_graceful",
            &[
                VmValue::task_handle("public-graceful-child"),
                VmValue::Duration(1_000),
            ],
        )
        .await
        .expect("cancel_graceful executes");
    assert!(handled);
    let result = vm.stack.last().expect("cancel result");
    assert!(matches!(
        result,
        VmValue::EnumVariant(variant) if variant.is_variant("Result", "Err")
    ));
    assert!(result.display().contains("graceful boom"));
    assert_cleanup_released(cleanup_progress, session_id).await;
    crate::agent_sessions::reset_session_store();
    crate::llm::agent_session_host::reset_agent_session_host_state();
}
