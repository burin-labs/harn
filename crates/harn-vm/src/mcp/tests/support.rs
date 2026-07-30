pub(super) async fn execute_test_harn(source: &str) {
    let program = harn_parser::parse_source(source).expect("trusted test fixture should parse");
    let chunk = crate::Compiler::with_options(crate::CompilerOptions::privileged_wire())
        .compile(&program)
        .expect("trusted test fixture should compile");
    let mut vm = crate::Vm::new();
    crate::register_vm_stdlib(&mut vm);
    vm.execute(&chunk)
        .await
        .expect("test Harn source should execute");
}
