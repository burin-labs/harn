//! Export the owning portable corpus for checks of the packaged Wasm runtime.

include!("../testdata/portable_conformance.rs");

fn main() {
    let pure: Vec<_> = PURE_CASES
        .iter()
        .map(|case| {
            serde_json::json!({
                "id": case.id,
                "source": case.source,
                "entry": case.entry,
                "input_json": case.input_json,
                "expected_json": case.expected_json,
            })
        })
        .collect();
    let failures: Vec<_> = RUNTIME_FAILURE_CASES
        .iter()
        .map(|case| {
            serde_json::json!({
                "id": case.id,
                "source": case.source,
                "entry": case.entry,
                "input_json": case.input_json,
                "expected_code": case.expected_code,
            })
        })
        .collect();
    let compilation: Vec<_> = INVALID_CASES
        .iter()
        .map(|case| (case.id, case.source, case.entry, false))
        .chain(
            PACKAGE_COMPILATION_CASES
                .iter()
                .map(|case| (case.id, case.source, case.entry, case.expected_ok)),
        )
        .map(|(id, source, entry, expected_ok)| {
            let package = harn_kernel::PortableSourcePackage {
                root_source: source.to_string(),
                root_imports: Vec::new(),
                modules: Vec::new(),
            };
            let result = harn_kernel::compile_source_package(
                package.clone(),
                entry,
                harn_kernel::EntryKind::Function,
            );
            assert_eq!(
                result.is_ok(),
                expected_ok,
                "{id} compilation decision drifted"
            );
            let diagnostics = result.err().unwrap_or_default();
            assert!(
                expected_ok || !diagnostics.is_empty(),
                "{id} has no failure diagnostic"
            );
            serde_json::json!({
                "id": id,
                "package": package,
                "entry": entry,
                "expected_ok": expected_ok,
                "expected_diagnostics": diagnostics,
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::json!({"pure": pure, "failures": failures, "compilation": compilation})
    );
}
