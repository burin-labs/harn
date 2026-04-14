//! Integration tests for `tool_ref` / `tool_def` / `tool_bind`.

use harn_vm::value::VmError;

fn run(source: &str) -> Result<String, String> {
    harn_vm::reset_thread_local_state();
    let chunk = harn_vm::compile_source(source)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                vm.execute(&chunk)
                    .await
                    .map_err(|e: VmError| format!("{e:?}"))?;
                Ok(vm.output().to_string())
            })
            .await
    })
}

fn out(source: &str) -> Vec<String> {
    let raw = run(source).unwrap();
    raw.lines()
        .filter_map(|l| l.strip_prefix("[harn] "))
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn tool_ref_returns_name_when_registered() {
    let lines = out(r#"
pipeline main(task) {
  var r = tool_registry()
  r = tool_define(r, "edit", "Edit a file", {parameters: {path: "string"}})
  tool_bind(r)
  log(tool_ref("edit"))
}
"#);
    assert_eq!(lines, vec!["edit"]);
}

#[test]
fn tool_ref_throws_on_unknown_name() {
    let err = run(r#"
pipeline main(task) {
  var r = tool_registry()
  r = tool_define(r, "edit", "Edit a file", {parameters: {path: "string"}})
  tool_bind(r)
  log(tool_ref("nonexistent"))
}
"#)
    .unwrap_err();
    assert!(
        err.contains("nonexistent"),
        "expected error to mention missing name, got: {err}"
    );
    assert!(
        err.contains("edit"),
        "expected error to list registered tools, got: {err}"
    );
}

#[test]
fn tool_ref_throws_without_binding() {
    let err = run(r#"
pipeline main(task) {
  log(tool_ref("edit"))
}
"#)
    .unwrap_err();
    assert!(
        err.contains("no tool registry bound"),
        "expected error about missing registry binding, got: {err}"
    );
}

#[test]
fn tool_def_returns_registered_entry() {
    let lines = out(r#"
pipeline main(task) {
  var r = tool_registry()
  r = tool_define(r, "edit", "Edit a file in place", {parameters: {path: "string"}})
  tool_bind(r)
  let def = tool_def("edit")
  log(def.name)
  log(def.description)
}
"#);
    assert_eq!(lines, vec!["edit", "Edit a file in place"]);
}

#[test]
fn tool_def_throws_on_unknown_name() {
    let err = run(r#"
pipeline main(task) {
  var r = tool_registry()
  r = tool_define(r, "edit", "Edit a file", {parameters: {path: "string"}})
  tool_bind(r)
  log(tool_def("nonexistent"))
}
"#)
    .unwrap_err();
    assert!(
        err.contains("nonexistent"),
        "expected error to mention missing name, got: {err}"
    );
}

#[test]
fn tool_def_throws_without_binding() {
    let err = run(r#"
pipeline main(task) {
  log(tool_def("edit"))
}
"#)
    .unwrap_err();
    assert!(
        err.contains("no tool registry bound"),
        "expected error about missing registry binding, got: {err}"
    );
}

#[test]
fn tool_bind_nil_clears_registry() {
    let err = run(r#"
pipeline main(task) {
  var r = tool_registry()
  r = tool_define(r, "edit", "Edit a file", {parameters: {path: "string"}})
  tool_bind(r)
  tool_bind(nil)
  log(tool_ref("edit"))
}
"#)
    .unwrap_err();
    assert!(
        err.contains("no tool registry bound"),
        "expected cleared-registry error, got: {err}"
    );
}
