use super::tests_runtime::run_harn;

fn mixed_pin_probe(binding_keyword: &str) -> String {
    let source = r#"pipeline default(task) {
let pins = []
for version in ["0.8.169", "0.8.168"] {
  __BINDING__ pin = {name: "harn-" + version, version: version}
  pins = pins + [pin]
}
let version = pins[0].version
let mixed = []
for pin in pins {
  if pin.version != version {
    mixed = mixed + [pin]
  }
}
const evidence = pins.map({ pin -> "${pin.name}=${pin.version}" }).join(", ")
if len(mixed) > 0 {
  log("nil|" + evidence)
} else {
  log(version + "|" + evidence)
}
}"#
    .replace("__BINDING__", binding_keyword);

    run_harn(&source).0.trim_end().to_string()
}

#[test]
fn shadowed_loop_bindings_preserve_let_to_const_semantics() {
    let mutable_output = mixed_pin_probe("let");
    let immutable_output = mixed_pin_probe("const");

    assert_eq!(immutable_output, mutable_output);
    assert_eq!(
        immutable_output,
        "[harn] nil|harn-0.8.169=0.8.169, harn-0.8.168=0.8.168"
    );
}

#[test]
fn inherited_pipeline_binding_is_captured_by_child_closure() {
    let (output, _) = run_harn(
        r"pipeline base(task) {
let value = 1
}
pipeline default(task) extends base {
const read = { -> value }
value = value + 1
log(read())
}",
    );

    assert_eq!(output.trim_end(), "[harn] 2");
}
