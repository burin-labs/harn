use super::tests_runtime::run_harn;

#[test]
fn calling_returned_closures() {
    let (output, _) = run_harn(
        r"pipeline t(harness: Harness, task: unknown) {
  fn make(base) { return { value -> base + value } }
  harness.stdio.log(make(40)(2))
  harness.stdio.log((make(39))(3))
  harness.stdio.log(41 |> make(1)(_))
}",
    );
    assert_eq!(output, "[harn] 42\n[harn] 42\n[harn] 42\n");
}
