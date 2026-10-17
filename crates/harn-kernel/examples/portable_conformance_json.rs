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
    let compilation: Vec<_> = package_compilation_cases()
        .map(|case| {
            let package = case.package();
            let diagnostics = case.compile().err().unwrap_or_default();
            serde_json::json!({
                "id": case.id,
                "package": package,
                "entry": case.entry,
                "expected_ok": case.expected_ok,
                "expected_diagnostics": diagnostics,
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::json!({"pure": pure, "failures": failures, "compilation": compilation})
    );
}
