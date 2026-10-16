use super::*;

#[test]
fn vm_construction_initializes_shared_secret_patterns() {
    let _vm = crate::new_vm();
    assert!(crate::secret_patterns::default_secret_patterns_initialized());
}

fn baseline_with_stdlib(source: &str) -> VmBaseline {
    let mut vm = crate::new_vm();
    crate::register_vm_stdlib(&mut vm);
    vm.set_source_info("baseline_test.harn", source);
    vm.set_global(
        "stable_global",
        VmValue::String(arcstr::ArcStr::from("baseline")),
    );
    vm.baseline()
}

#[tokio::test(flavor = "current_thread")]
async fn vm_baseline_rebinds_shared_state_builtins_per_instance() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let source = r#"
pipeline main(harness: Harness) {
  const cell = harness.runtime.shared_cell({scope: "task_group", key: "turn", initial: 0})
  harness.stdio.println(harness.runtime.shared_get(cell))
  harness.runtime.shared_set(cell, harness.runtime.shared_get(cell) + 1)
}"#;
            let chunk = crate::compile_source(source).expect("compile");
            let baseline = baseline_with_stdlib(source);

            let mut first = baseline.instantiate();
            first.execute(&chunk).await.expect("first execute");
            assert_eq!(first.output(), "0\n");

            let mut second = baseline.instantiate();
            second.execute(&chunk).await.expect("second execute");
            assert_eq!(
                second.output(),
                "0\n",
                "shared state created by the first VM must not leak into the next baseline instance"
            );
        })
        .await;
}
