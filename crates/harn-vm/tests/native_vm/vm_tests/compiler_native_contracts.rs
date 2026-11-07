use crate::linked_program::link_program;
use crate::module_artifact::{
    compile_module_artifact_from_source, ModuleCompilationContext, ModuleProvenance,
};
use std::{fs, path::Path};

#[test]
fn ordinary_modules_cannot_name_privileged_wire_builtins() {
    let source = r#"fn probe() { return host_call("project.scan", {}) }"#;
    let error = compile_module_artifact_from_source(Path::new("<test>/user.harn"), source)
        .expect_err("a tail call must not acquire wire authority");
    assert!(
        error.to_string().contains("not callable source API"),
        "{error}"
    );
}

#[test]
fn embedded_stdlib_can_wrap_runtime_internal_builtins_without_exposing_them() {
    let source = r#"pub fn ansi_enabled() { return __ansi_enabled("stdout") }"#;
    let context = ModuleCompilationContext::default();
    harn_vm_core::vm::compile_stdlib_source_for_test(
        Path::new("<stdlib>/ansi.harn"),
        source,
        &context,
    )
    .expect("embedded stdlib wrapper compiles");

    let error = compile_module_artifact_from_source(Path::new("<test>/user.harn"), source)
        .expect_err("ordinary source must not acquire stdlib authority");
    assert!(
        error.to_string().contains("not callable source API"),
        "{error}"
    );
}

#[test]
fn closed_link_preserves_embedded_stdlib_authority() {
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("entry.harn");
    fs::write(
        &entry,
        r#"
        import { ansi_enabled } from "std/ansi"
        fn main() { ansi_enabled() }
        "#,
    )
    .unwrap();

    let linked = link_program(&entry, dir.path()).expect("embedded stdlib link succeeds");
    assert_eq!(
        linked.modules[Path::new("<std>/ansi")].provenance,
        ModuleProvenance::EmbeddedStdlib
    );
}
