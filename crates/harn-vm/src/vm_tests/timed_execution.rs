use super::*;
use crate::compiler::Compiler;
use crate::stdlib::register_vm_stdlib;
use harn_lexer::Lexer;
use harn_parser::Parser;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{sync::Arc, time::Duration};

struct ChildLifetime(Option<tokio::sync::oneshot::Sender<()>>);

impl Drop for ChildLifetime {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

fn compile_harn(source: &str) -> Chunk {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    Compiler::new().compile(&program).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_timed_execution_restores_ambient_state_and_poison_vm_reuse() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            crate::reset_thread_local_state();
            let baseline_dir = tempfile::tempdir().unwrap();
            let imported_dir = tempfile::tempdir().unwrap();
            let poisoned_dir = tempfile::tempdir().unwrap();
            let quick = compile_harn("pipeline default(harness: Harness) { return 42 }");
            let mut vm = crate::new_vm();
            register_vm_stdlib(&mut vm);
            crate::tracing::set_tracing_enabled(true);

            let child_started = Arc::new(AtomicBool::new(false));
            let child_effect = Arc::new(AtomicBool::new(false));
            let child_release = Arc::new(tokio::sync::Notify::new());
            let (child_stopped_tx, child_stopped_rx) = tokio::sync::oneshot::channel();
            let child_stopped_tx = Arc::new(std::sync::Mutex::new(Some(child_stopped_tx)));
            let (ambient_started_tx, ambient_started_rx) = tokio::sync::oneshot::channel();
            let ambient_started_tx = Arc::new(std::sync::Mutex::new(Some(ambient_started_tx)));
            let started_for_builtin = Arc::clone(&child_started);
            vm.register_builtin("child_started", move |_args, _output| {
                started_for_builtin.store(true, Ordering::Release);
                Ok(VmValue::resource(crate::VmResourceHandle::new(
                    "cancellation-child",
                    ChildLifetime(Some(child_stopped_tx.lock().unwrap().take().unwrap())),
                )))
            });
            let effect_for_builtin = Arc::clone(&child_effect);
            vm.register_builtin("child_effect", move |_args, _output| {
                effect_for_builtin.store(true, Ordering::Release);
                Ok(VmValue::Nil)
            });
            let release_for_builtin = Arc::clone(&child_release);
            vm.register_async_builtin("wait_for_child_release", move |_ctx, _args| {
                let release = Arc::clone(&release_for_builtin);
                async move {
                    release.notified().await;
                    Ok(VmValue::Nil)
                }
            });
            vm.register_async_builtin("wait_forever", |_ctx, _args| async move {
                std::future::pending::<()>().await;
                Ok(VmValue::Nil)
            });

            let imported_path = imported_dir.path().join("cancelled.harn");
            let imported_source_dir = imported_dir.path().to_path_buf();
            let poisoned_source_dir = poisoned_dir.path().to_path_buf();
            let ambient_started_for_builtin = Arc::clone(&ambient_started_tx);
            vm.register_builtin("strand_ambient_state", move |_args, _output| {
                crate::step_runtime::register_persona(
                    "cancellation_entry",
                    crate::step_runtime::PersonaDefinition {
                        name: "cancel_persona".into(),
                        stages: vec![crate::personas::StageDecl {
                            name: "cancel_step".into(),
                            allowed_tools: Some(vec!["cancel_tool".into()]),
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                );
                crate::step_runtime::register_step(
                    "cancelled_step",
                    crate::step_runtime::StepDefinition {
                        name: "cancel_step".into(),
                        function: "cancelled_step".into(),
                        model: Some("cancel-model".into()),
                        ..Default::default()
                    },
                );
                assert!(crate::step_runtime::maybe_push_active_persona(
                    "cancellation_entry",
                    1
                ));
                assert!(crate::step_runtime::maybe_push_active_step(
                    "cancelled_step",
                    2,
                    &[]
                ));
                assert_eq!(
                    crate::stdlib::process::source_root_path(),
                    imported_source_dir
                );
                assert_eq!(
                    crate::step_runtime::current_persona_name().as_deref(),
                    Some("cancel_persona")
                );
                assert_eq!(
                    crate::step_runtime::active_step_model_default().as_deref(),
                    Some("cancel-model")
                );
                assert_eq!(
                    crate::orchestration::current_execution_policy()
                        .unwrap()
                        .tools,
                    vec!["cancel_tool"]
                );
                crate::stdlib::process::set_thread_source_dir(&poisoned_source_dir);
                crate::stdlib::process::set_thread_execution_context(Some(
                    crate::orchestration::RunExecutionRecord {
                        adapter: Some("cancelled".into()),
                        ..Default::default()
                    },
                ));
                crate::orchestration::push_approval_policy(
                    crate::orchestration::ToolApprovalPolicy {
                        auto_deny: vec!["cancel_tool".into()],
                        ..Default::default()
                    },
                );
                if let Some(sender) = ambient_started_for_builtin.lock().unwrap().take() {
                    let _ = sender.send(());
                }
                Ok(VmValue::Nil)
            });

            std::fs::write(
                &imported_path,
                r#"
@persona(name: "cancel_persona", stages: [{name: "cancel_step", allowed_tools: ["cancel_tool"]}])
pub fn cancellation_entry(agent: HarnessAgent) {
  return cancelled_step(agent)
}

@step(name: "cancel_step", model: "cancel-model")
fn cancelled_step(agent: HarnessAgent) {
  agent.pipeline_on_finish({ _h, value -> value })
  const child = spawn {
    const lifetime = child_started()
wait_for_child_release()
    child_effect(lifetime)
  }
  strand_ambient_state()
  wait_forever()
}
"#,
            )
            .unwrap();
            let mut helper_exports = vm
                .load_module_exports_from_source(
                    "<cancellation-helper>",
                    "pub fn outer_callback(_h, value) { return value }\n\
                     pub fn answer() { return 42 }",
                )
                .await
                .unwrap();
            let callable = helper_exports.remove("answer").unwrap();
            let outer_callback = helper_exports.remove("outer_callback").unwrap();
            let slow = compile_harn(&format!(
                "import {{ cancellation_entry }} from \"{}\"\n\
                 pipeline default(harness: Harness) {{ return cancellation_entry(harness.agent) }}",
                imported_path.display()
            ));

            let baseline_execution = crate::orchestration::RunExecutionRecord {
                cwd: Some(baseline_dir.path().display().to_string()),
                source_dir: Some(baseline_dir.path().display().to_string()),
                adapter: Some("baseline".into()),
                ..Default::default()
            };
            let baseline_policy = crate::orchestration::CapabilityPolicy {
                tools: vec!["baseline_tool".into(), "cancel_tool".into()],
                ..Default::default()
            };
            let baseline_approval = crate::orchestration::ToolApprovalPolicy {
                auto_approve: vec!["baseline_tool".into()],
                ..Default::default()
            };
            crate::stdlib::process::set_thread_source_dir(baseline_dir.path());
            crate::stdlib::process::set_thread_execution_context(Some(
                baseline_execution.clone(),
            ));
            crate::orchestration::push_execution_policy(baseline_policy.clone());
            crate::orchestration::push_approval_policy(baseline_approval.clone());
            crate::orchestration::set_pipeline_on_finish(Arc::clone(&outer_callback));
            let outer_span =
                crate::tracing::span_start(crate::tracing::SpanKind::Pipeline, "outer".into());

            let mut execution =
                Box::pin(vm.execute_with_timeout(&slow, Duration::from_secs(30)));
            tokio::select! {
                biased;
                result = &mut execution => panic!("slow execution unexpectedly finished: {result:?}"),
                started = async {
                    ambient_started_rx.await.expect("step did not reach cancellation point");
                    while !child_started.load(Ordering::Acquire) {
                        tokio::task::yield_now().await;
                    }
                } => started,
            }
            drop(execution);


            assert!(vm.frame_count() > 0, "fixture must abandon a live frame");
            assert_eq!(
                crate::stdlib::process::source_root_path(),
                baseline_dir.path()
            );
            assert_eq!(
                crate::stdlib::process::current_execution_context(),
                Some(baseline_execution.clone())
            );
            assert_eq!(
                crate::orchestration::current_execution_policy(),
                Some(baseline_policy.clone())
            );
            assert_eq!(
                crate::orchestration::current_approval_policy(),
                Some(baseline_approval.clone())
            );
            assert!(crate::step_runtime::current_persona_name().is_none());
            assert!(crate::step_runtime::active_step_model_default().is_none());
            assert_eq!(crate::tracing::current_span_id(), Some(outer_span));
            let restored_callback =
                crate::orchestration::take_pipeline_on_finish().unwrap();
            assert!(Arc::ptr_eq(&restored_callback, &outer_callback));
            crate::orchestration::set_pipeline_on_finish(restored_callback);
            let abandoned_spans = crate::tracing::peek_spans();
            assert!(abandoned_spans.iter().any(|span| {
                span.name == "cancel_step"
                    && span.metadata.get("status") == Some(&serde_json::json!("abandoned"))
            }));
            assert!(abandoned_spans.iter().any(|span| {
                span.name == "main"
                    && span.metadata.get("status") == Some(&serde_json::json!("abandoned"))
            }));

            let frame_depth = vm.frame_count();
            let output = vm.output().to_string();
            let error = vm.execute(&quick).await.unwrap_err();
            assert!(matches!(error, VmError::AbandonedExecution));
            let closure_error = vm.call_closure_pub(&callable, &[]).await.unwrap_err();
            assert!(matches!(closure_error, VmError::AbandonedExecution));
            let loaded_sources = vm.debug_loaded_source_paths();

            let module_error = vm
                .load_module_exports_from_source(
                    "<poisoned-module-load>",
                    "pub fn poisoned() { return 0 }",
                )
                .await
                .unwrap_err();
            assert!(matches!(module_error, VmError::AbandonedExecution));
            assert_eq!(vm.debug_loaded_source_paths(), loaded_sources);

            let start_error = vm.start(&quick).unwrap_err();
            assert!(matches!(start_error, VmError::AbandonedExecution));
            let restart_error = vm.restart_frame(0).unwrap_err();
            assert!(matches!(restart_error, VmError::AbandonedExecution));
            assert_eq!(vm.frame_count(), frame_depth);
            assert_eq!(vm.output(), output);

            let mut vm_b = crate::new_vm();
            register_vm_stdlib(&mut vm_b);
            let observed_callback = Arc::clone(&outer_callback);
            let observed_dir = baseline_dir.path().to_path_buf();
            vm_b.register_builtin("observe_baseline", move |_args, _output| {
                assert_eq!(crate::stdlib::process::source_root_path(), observed_dir);
                assert_eq!(
                    crate::stdlib::process::current_execution_context(),
                    Some(baseline_execution.clone())
                );
                assert_eq!(
                    crate::orchestration::current_execution_policy(),
                    Some(baseline_policy.clone())
                );
                assert_eq!(
                    crate::orchestration::current_approval_policy(),
                    Some(baseline_approval.clone())
                );
                assert!(crate::step_runtime::current_persona_name().is_none());
                assert!(crate::step_runtime::active_step_model_default().is_none());
                let callback = crate::orchestration::take_pipeline_on_finish().unwrap();
                assert!(Arc::ptr_eq(&callback, &observed_callback));
                crate::orchestration::set_pipeline_on_finish(callback);
                Ok(VmValue::Nil)
            });
            let vm_b_chunk =
                compile_harn("pipeline default(harness: Harness) { observe_baseline(); return 42 }");
            assert!(matches!(
                vm_b.execute(&vm_b_chunk).await.unwrap(),
                VmValue::Int(42)
            ));
            assert_eq!(crate::tracing::current_span_id(), Some(outer_span));

            drop(vm);
            child_release.notify_one();
            child_stopped_rx.await.expect("the child lifetime must terminate");
            assert!(
                !child_effect.load(Ordering::Acquire),
                "dropping an abandoned VM must abort spawned side effects"
            );
            crate::reset_thread_local_state();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn natural_host_deadline_is_terminal_not_abandoned() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let infinite = compile_harn("pipeline default(harness: Harness) { while true {} }");
            let quick = compile_harn("pipeline default(harness: Harness) { return 42 }");
            let mut vm = crate::new_vm();
            register_vm_stdlib(&mut vm);

            let error = vm
                .execute_with_timeout(&infinite, Duration::ZERO)
                .await
                .unwrap_err();
            assert!(matches!(error, VmError::ExecutionDeadlineExceeded));

            assert!(matches!(
                vm.execute(&quick).await.unwrap(),
                VmValue::Int(42)
            ));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn host_admission_extends_execution_deadline_by_injected_clock_delta() {
    let chunk = compile_harn(
        r"
pipeline default(harness: Harness) {
  wait_for_admission()
  return 42
}
",
    );
    let mut vm = crate::new_vm();
    register_vm_stdlib(&mut vm);
    let clock = harn_clock::PausedClock::new(time::OffsetDateTime::UNIX_EPOCH);
    let admission_clock: Arc<dyn harn_clock::Clock> = clock.clone();
    vm.register_async_builtin("wait_for_admission", move |ctx, _args| {
        let clock = Arc::clone(&clock);
        let admission_clock = Arc::clone(&admission_clock);
        async move {
            let before = ctx.execution_deadline_offset_for_test();
            let pause = ctx
                .pause_execution_deadline(admission_clock)
                .expect("timed execution exposes its outer deadline to inline host work");
            clock.advance(Duration::from_millis(25));
            drop(pause);
            let after = ctx.execution_deadline_offset_for_test();
            assert_eq!(after.saturating_sub(before), 25_000_000);
            Ok(VmValue::Nil)
        }
    });

    let value = vm
        .execute_with_timeout(&chunk, Duration::from_secs(5))
        .await
        .expect("host admission extends rather than spends the execution budget");
    assert!(matches!(value, VmValue::Int(42)));
}

#[tokio::test(flavor = "current_thread")]
async fn timed_finite_loop_keeps_sync_opcodes_on_direct_dispatch() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let chunk = compile_harn(
                r"
pipeline default(harness: Harness) {
  let total = 0
  for i in 0..=10000 {
total = total + i
  }
  return total
}
",
            );
            let mut vm = crate::new_vm();
            register_vm_stdlib(&mut vm);
            harn_vm_core::vm::reset_scope_interrupt_async_dispatches();

            let value = vm
                .execute_with_timeout(&chunk, Duration::from_secs(1))
                .await
                .unwrap();

            assert!(matches!(value, VmValue::Int(_)));
            assert!(
                harn_vm_core::vm::scope_interrupt_async_dispatches() <= 4,
                "finite sync loop fell back to per-op async dispatch"
            );
        })
        .await;
}
