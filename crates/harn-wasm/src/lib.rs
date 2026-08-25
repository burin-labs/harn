//! Browser adapter for the Portable Harn Kernel.
//!
//! Language semantics live in `harn-kernel`. This crate only converts the
//! generated JavaScript boundary into the kernel's versioned artifact and
//! execute/resume contracts.

use harn_kernel::{
    ArtifactLimits, BenchmarkBuildProfile, BenchmarkProvenance, BenchmarkStatistics,
    CapabilityResult, DataValue, Diagnostic, EntryKind, Execution, GrantSet,
    PortableBenchmarkReceipt, PortableSourcePackage, ProgramArtifact,
    PORTABLE_BENCHMARK_SCHEMA_VERSION, PORTABLE_MAX_DISPATCH_ITERATIONS,
    PORTABLE_MAX_GRANTS_JSON_BYTES, PORTABLE_MAX_PACKAGE_BYTES, PORTABLE_MAX_PACKAGE_MODULES,
    PORTABLE_MAX_SOURCE_BYTES, PORTABLE_MAX_VALUE_JSON_BYTES,
};
use wasm_bindgen::prelude::*;

const MAX_BENCHMARK_SAMPLES_JSON_BYTES: usize = 8 * 1024 * 1024;
const MAX_BENCHMARK_RECEIPT_JSON_BYTES: usize = 64 * 1024;

/// The result of compiling source with the canonical Harn frontend.
#[wasm_bindgen]
pub struct CompileOutcome {
    artifact: Vec<u8>,
    digest: String,
    diagnostics_json: String,
}

#[wasm_bindgen]
impl CompileOutcome {
    #[wasm_bindgen(getter)]
    pub fn ok(&self) -> bool {
        !self.artifact.is_empty()
    }

    #[wasm_bindgen(getter)]
    pub fn digest(&self) -> String {
        self.digest.clone()
    }

    /// Return an independent copy suitable for transfer to a Web Worker.
    #[wasm_bindgen(js_name = artifactBytes)]
    pub fn artifact_bytes(&self) -> Vec<u8> {
        self.artifact.clone()
    }

    #[wasm_bindgen(js_name = diagnosticsJson)]
    pub fn diagnostics_json(&self) -> String {
        self.diagnostics_json.clone()
    }
}

/// A stable projection of completed, suspended, or failed execution.
#[wasm_bindgen]
pub struct ExecutionOutcome {
    status: &'static str,
    value_json: String,
    request_json: String,
    snapshot: Vec<u8>,
    diagnostic_json: String,
}

#[wasm_bindgen]
impl ExecutionOutcome {
    #[wasm_bindgen(getter)]
    pub fn status(&self) -> String {
        self.status.to_string()
    }

    #[wasm_bindgen(js_name = valueJson)]
    pub fn value_json(&self) -> String {
        self.value_json.clone()
    }

    #[wasm_bindgen(js_name = requestJson)]
    pub fn request_json(&self) -> String {
        self.request_json.clone()
    }

    #[wasm_bindgen(js_name = snapshotBytes)]
    pub fn snapshot_bytes(&self) -> Vec<u8> {
        self.snapshot.clone()
    }

    #[wasm_bindgen(js_name = diagnosticJson)]
    pub fn diagnostic_json(&self) -> String {
        self.diagnostic_json.clone()
    }
}

/// Compile a function or pipeline through the canonical Harn frontend.
#[wasm_bindgen]
pub fn compile(source: &str, entry: &str, entry_kind: &str) -> CompileOutcome {
    if source.len() > PORTABLE_MAX_SOURCE_BYTES {
        return compile_failure(vec![Diagnostic::new(
            "source_too_large",
            "source exceeds the browser compiler's 1 MiB limit",
        )]);
    }
    let entry_kind = match entry_kind.parse::<EntryKind>() {
        Ok(kind) => kind,
        Err(diagnostic) => return compile_failure(vec![diagnostic]),
    };

    match harn_kernel::compile_program(source, entry, entry_kind) {
        Ok(program) => CompileOutcome {
            artifact: program.bytes().to_vec(),
            digest: program.digest_hex(),
            diagnostics_json: "[]".to_string(),
        },
        Err(diagnostics) => compile_failure(diagnostics),
    }
}

/// Compile a host-linked source package through the same canonical lexer,
/// parser, compiler, and artifact encoder as native Harn. The manifest is
/// deliberately data-only: import targets and public export projections are
/// resolved by the host build step, while this adapter has no filesystem or
/// package-loader authority.
#[wasm_bindgen(js_name = compilePackage)]
pub fn compile_package(
    manifest_json: &str,
    entry: &str,
    entry_kind: &str,
) -> Result<CompileOutcome, JsError> {
    if manifest_json.len() > PORTABLE_MAX_PACKAGE_BYTES {
        return Err(JsError::new("package manifest exceeds the 8 MiB limit"));
    }
    let entry_kind = entry_kind
        .parse::<EntryKind>()
        .map_err(|error| JsError::new(&format!("{}: {}", error.code, error.message)))?;
    let manifest: PortableSourcePackage = serde_json::from_str(manifest_json)
        .map_err(|error| JsError::new(&format!("invalid package manifest: {error}")))?;
    if manifest.modules.len() > PORTABLE_MAX_PACKAGE_MODULES {
        return Err(JsError::new("package module count exceeds the 1,024 limit"));
    }
    let source_bytes = manifest.root_source.len()
        + manifest
            .modules
            .iter()
            .map(|module| module.source.len())
            .sum::<usize>();
    if source_bytes > PORTABLE_MAX_PACKAGE_BYTES {
        return Err(JsError::new("package source exceeds the 8 MiB limit"));
    }
    Ok(
        match harn_kernel::compile_source_package(manifest, entry, entry_kind) {
            Ok(program) => CompileOutcome {
                artifact: program.bytes().to_vec(),
                digest: program.digest_hex(),
                diagnostics_json: "[]".to_string(),
            },
            Err(diagnostics) => compile_failure(diagnostics),
        },
    )
}

/// Start a fresh portable execution.
#[wasm_bindgen]
pub fn start(
    artifact: &[u8],
    input_json: &str,
    grants_json: &str,
) -> Result<ExecutionOutcome, JsError> {
    let program = decode_program(artifact)?;
    let input = decode_input(input_json)?;
    let grants = decode_grants(grants_json)?;
    Ok(ExecutionOutcome::from(harn_kernel::start(
        &program, input, &grants,
    )))
}

/// Resume a suspended execution with the matching typed capability result.
#[wasm_bindgen]
pub fn resume(
    artifact: &[u8],
    snapshot: &[u8],
    capability_result_json: &str,
    grants_json: &str,
) -> Result<ExecutionOutcome, JsError> {
    if capability_result_json.len() > PORTABLE_MAX_VALUE_JSON_BYTES {
        return Err(JsError::new("capability result exceeds the 1 MiB limit"));
    }
    let program = decode_program(artifact)?;
    let result: CapabilityResult = serde_json::from_str(capability_result_json)
        .map_err(|error| JsError::new(&format!("invalid capability result: {error}")))?;
    let grants = decode_grants(grants_json)?;
    Ok(ExecutionOutcome::from(harn_kernel::resume(
        &program, snapshot, result, &grants,
    )))
}

/// Aggregate host-recorded benchmark samples with the kernel's canonical
/// R-7 percentile and population-standard-deviation contract.
///
/// The host owns clock access. This projection accepts only a bounded JSON
/// array so exposing benchmark aggregation does not grant the kernel a clock or
/// create a second JavaScript statistics implementation.
#[wasm_bindgen(js_name = summarizeBenchmarkSamples)]
pub fn summarize_benchmark_samples(samples_json: &str) -> Result<String, JsError> {
    if samples_json.len() > MAX_BENCHMARK_SAMPLES_JSON_BYTES {
        return Err(JsError::new(
            "benchmark sample JSON exceeds the 8 MiB limit",
        ));
    }
    let samples: Vec<f64> = serde_json::from_str(samples_json)
        .map_err(|error| JsError::new(&format!("invalid benchmark sample JSON: {error}")))?;
    if samples.len() > PORTABLE_MAX_DISPATCH_ITERATIONS {
        return Err(JsError::new(
            "benchmark sample count exceeds the 1,000,000 sample limit",
        ));
    }
    let statistics = BenchmarkStatistics::from_samples(samples)
        .map_err(|error| JsError::new(&format!("{}: {error}", error.code())))?;
    serde_json::to_string(&statistics)
        .map_err(|error| JsError::new(&format!("serialize benchmark statistics: {error}")))
}

/// Return the kernel-owned receipt version instead of duplicating it in
/// browser configuration.
#[wasm_bindgen(js_name = benchmarkSchemaVersion)]
pub fn benchmark_schema_version() -> String {
    PORTABLE_BENCHMARK_SCHEMA_VERSION.to_string()
}

/// Return versioned build provenance for a portable browser benchmark receipt.
#[wasm_bindgen(js_name = benchmarkProvenanceJson)]
pub fn benchmark_provenance_json() -> String {
    let provenance = BenchmarkProvenance::current(
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) {
            BenchmarkBuildProfile::Debug
        } else {
            BenchmarkBuildProfile::Release
        },
        "wasm32-unknown",
        "wasm32",
    );
    serde_json::to_string(&provenance).expect("benchmark provenance serializes")
}

/// Validate and canonically serialize a browser-captured benchmark receipt
/// through the same closed Rust type used by the native CLI.
#[wasm_bindgen(js_name = normalizeBenchmarkReceiptJson)]
pub fn normalize_benchmark_receipt_json(receipt_json: &str) -> Result<String, JsError> {
    if receipt_json.len() > MAX_BENCHMARK_RECEIPT_JSON_BYTES {
        return Err(JsError::new(
            "benchmark receipt JSON exceeds the 64 KiB limit",
        ));
    }
    let receipt: PortableBenchmarkReceipt = serde_json::from_str(receipt_json)
        .map_err(|error| JsError::new(&format!("invalid benchmark receipt: {error}")))?;
    receipt
        .validate()
        .map_err(|error| JsError::new(&format!("invalid benchmark receipt: {error}")))?;
    serde_json::to_string(&receipt)
        .map_err(|error| JsError::new(&format!("serialize benchmark receipt: {error}")))
}

/// Hash a bounded portable terminal value with the same canonical JSON and
/// BLAKE3 contract used by the native benchmark receipt.
#[wasm_bindgen(js_name = benchmarkTerminalDigest)]
pub fn benchmark_terminal_digest(value_json: &str) -> Result<String, JsError> {
    if value_json.len() > PORTABLE_MAX_VALUE_JSON_BYTES {
        return Err(JsError::new(
            "benchmark terminal JSON exceeds the 1 MiB limit",
        ));
    }
    let json = serde_json::from_str(value_json)
        .map_err(|error| JsError::new(&format!("invalid benchmark terminal JSON: {error}")))?;
    let value = DataValue::from_json(json)
        .map_err(|error| JsError::new(&format!("{}: {}", error.code, error.message)))?;
    Ok(harn_kernel::benchmark_terminal_digest(&value))
}

fn compile_failure(diagnostics: Vec<Diagnostic>) -> CompileOutcome {
    CompileOutcome {
        artifact: Vec::new(),
        digest: String::new(),
        diagnostics_json: serde_json::to_string(&diagnostics).unwrap_or_else(|_| "[]".to_string()),
    }
}

fn decode_program(bytes: &[u8]) -> Result<ProgramArtifact, JsError> {
    ProgramArtifact::decode(bytes, ArtifactLimits::default())
        .map_err(|error| JsError::new(&format!("{}: {}", error.code, error.message)))
}

fn decode_input(json: &str) -> Result<DataValue, JsError> {
    if json.len() > PORTABLE_MAX_VALUE_JSON_BYTES {
        return Err(JsError::new("input JSON exceeds the 1 MiB limit"));
    }
    let value = serde_json::from_str(json)
        .map_err(|error| JsError::new(&format!("invalid input JSON: {error}")))?;
    DataValue::from_json(value)
        .map_err(|error| JsError::new(&format!("{}: {}", error.code, error.message)))
}

fn decode_grants(json: &str) -> Result<GrantSet, JsError> {
    if json.len() > PORTABLE_MAX_GRANTS_JSON_BYTES {
        return Err(JsError::new("grants JSON exceeds the 64 KiB limit"));
    }
    GrantSet::from_host_json(json)
        .map_err(|error| JsError::new(&format!("{}: {}", error.code, error.message)))
}

impl From<Execution> for ExecutionOutcome {
    fn from(execution: Execution) -> Self {
        match execution {
            Execution::Completed { value } => Self {
                status: "completed",
                value_json: value.to_json().to_string(),
                request_json: String::new(),
                snapshot: Vec::new(),
                diagnostic_json: String::new(),
            },
            Execution::Suspended { request, snapshot } => Self {
                status: "suspended",
                value_json: String::new(),
                request_json: serde_json::to_string(&request)
                    .expect("capability request has a stable JSON representation"),
                snapshot,
                diagnostic_json: String::new(),
            },
            Execution::Failed { diagnostic } => Self {
                status: "failed",
                value_json: String::new(),
                request_json: String::new(),
                snapshot: Vec::new(),
                diagnostic_json: serde_json::to_string(&diagnostic)
                    .expect("diagnostic has a stable JSON representation"),
            },
        }
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod browser_tests {
    use wasm_bindgen_test::*;

    use super::*;

    include!("../../harn-kernel/testdata/portable_conformance.rs");

    wasm_bindgen_test_configure!(run_in_dedicated_worker);

    const REDUCER: &str = r#"
        fn reduce(input: { count: int, delta: int }) -> { count: int } {
            return { count: input.count + input.delta }
        }
    "#;

    #[wasm_bindgen_test]
    fn canonical_compiler_and_kernel_execute_in_browser_wasm() {
        let compiled = compile(REDUCER, "reduce", "function");
        assert!(compiled.ok());
        assert!(start(
            &compiled.artifact_bytes(),
            r#"{"count":40,"delta":2}"#,
            "[]"
        )
        .is_err());
        let execution = start(
            &compiled.artifact_bytes(),
            r#"{"count":40,"delta":2}"#,
            r#"{"capabilities":[]}"#,
        )
        .expect("browser adapter starts");
        assert_eq!(execution.status(), "completed");
        assert_eq!(execution.value_json(), r#"{"count":42}"#);
    }

    #[wasm_bindgen_test]
    fn browser_worker_package_manifest_executes_the_same_imported_reducer() {
        let manifest = include_str!("../demo/package.json");
        let compiled = compile_package(manifest, "reduce", "function")
            .expect("browser package manifest parses");
        assert!(compiled.ok(), "{}", compiled.diagnostics_json());
        let repeated =
            compile_package(manifest, "reduce", "function").expect("browser package recompiles");
        assert!(repeated.ok(), "{}", repeated.diagnostics_json());
        assert_eq!(compiled.digest(), repeated.digest());
        assert_eq!(compiled.artifact_bytes(), repeated.artifact_bytes());
        let execution = start(
            &compiled.artifact_bytes(),
            r#"{"state":{"count":40,"history":[],"label":"portable"},"event":{"kind":"increment","amount":2}}"#,
            r#"{"capabilities":[]}"#,
        )
        .expect("browser package starts");
        assert_eq!(execution.status(), "completed");
        assert_eq!(
            execution.value_json(),
            r#"{"count":42,"history":[42],"label":"portable"}"#
        );
    }

    #[wasm_bindgen_test]
    fn browser_worker_matches_native_suspend_resume_and_denial() {
        const SOURCE: &str = r#"
            fn greet(harness: Harness, input: string) {
                return harness.interaction.ask(input)
            }
        "#;
        let compiled = compile(SOURCE, "greet", "function");
        assert!(compiled.ok(), "{}", compiled.diagnostics_json());
        let artifact = compiled.artifact_bytes();
        let grants = serde_json::json!({
            "capabilities": ["interaction.ask"],
            "snapshotKey": vec![7; 32],
        })
        .to_string();

        let suspended = start(&artifact, r#""name""#, &grants)
            .expect("browser capability call reaches the kernel");
        assert_eq!(suspended.status(), "suspended");
        let request: serde_json::Value = serde_json::from_str(&suspended.request_json()).unwrap();
        assert_eq!(request["capability"], "interaction");
        assert_eq!(request["operation"], "ask");

        let resumed = resume(
            &artifact,
            &suspended.snapshot_bytes(),
            &serde_json::json!({
                "status": "ok",
                "request_id": request["id"],
                "value": "Ada",
            })
            .to_string(),
            &grants,
        )
        .expect("matching typed result resumes the browser execution");
        assert_eq!(resumed.status(), "completed");
        assert_eq!(resumed.value_json(), r#""Ada""#);

        let denied = start(&artifact, r#""name""#, r#"{"capabilities":[]}"#)
            .expect("denial is a structured kernel result");
        assert_eq!(denied.status(), "failed");
        let browser_diagnostic: harn_kernel::Diagnostic =
            serde_json::from_str(&denied.diagnostic_json()).unwrap();
        let native_program =
            harn_kernel::ProgramArtifact::decode(&artifact, harn_kernel::ArtifactLimits::default())
                .unwrap();
        let harn_kernel::Execution::Failed {
            diagnostic: native_diagnostic,
        } = harn_kernel::start(
            &native_program,
            harn_kernel::DataValue::String("name".to_string()),
            &harn_kernel::GrantSet::pure(),
        )
        else {
            panic!("native denial did not fail")
        };
        assert_eq!(browser_diagnostic, native_diagnostic);
    }

    #[wasm_bindgen_test]
    fn invalid_programs_match_native_diagnostics_exactly() {
        for case in INVALID_CASES {
            let native = harn_kernel::compile_program(
                case.source,
                case.entry,
                harn_kernel::EntryKind::Function,
            )
            .expect_err("invalid case must fail natively");
            assert_eq!(native[0].code, case.expected_code, "{} drifted", case.id);

            let compiled = compile(case.source, case.entry, "function");
            assert!(!compiled.ok(), "{} unexpectedly compiled", case.id);
            assert_eq!(
                compiled.diagnostics_json(),
                serde_json::to_string(&native).unwrap(),
                "{} browser diagnostics diverged byte-for-byte",
                case.id,
            );
        }
    }

    #[wasm_bindgen_test]
    fn typed_host_input_matches_native_kernel_diagnostic_exactly() {
        let native_program =
            harn_kernel::compile_program(REDUCER, "reduce", harn_kernel::EntryKind::Function)
                .expect("typed reducer compiles");
        let input = harn_kernel::DataValue::from_json(serde_json::json!({
            "count": "forty",
            "delta": 2
        }))
        .unwrap();
        let harn_kernel::Execution::Failed { diagnostic } =
            harn_kernel::start(&native_program, input, &harn_kernel::GrantSet::pure())
        else {
            panic!("native kernel accepted invalid typed input")
        };

        let compiled = compile(REDUCER, "reduce", "function");
        let browser = start(
            &compiled.artifact_bytes(),
            r#"{"count":"forty","delta":2}"#,
            r#"{"capabilities":[]}"#,
        )
        .expect("browser adapter returns a structured failure");
        assert_eq!(browser.status(), "failed");
        assert_eq!(
            browser.diagnostic_json(),
            serde_json::to_string(&diagnostic).unwrap()
        );
    }

    #[wasm_bindgen_test]
    fn browser_worker_matches_native_portable_corpus_exactly() {
        for case in PURE_CASES {
            let compiled = compile(case.source, case.entry, "function");
            assert!(compiled.ok(), "{} did not compile", case.id);
            let execution = start(
                &compiled.artifact_bytes(),
                case.input_json,
                r#"{"capabilities":[]}"#,
            )
            .unwrap_or_else(|error| panic!("{} failed to start: {error:?}", case.id));
            assert_eq!(
                execution.status(),
                "completed",
                "{} did not complete",
                case.id
            );
            let actual: serde_json::Value = serde_json::from_str(&execution.value_json()).unwrap();
            let expected: serde_json::Value = serde_json::from_str(case.expected_json).unwrap();
            assert_eq!(actual, expected, "{} diverged", case.id);
        }
    }

    #[wasm_bindgen_test]
    fn browser_worker_matches_native_runtime_failure_corpus_exactly() {
        // Closes the numeric drift gap: a float into an int parameter must
        // fail identically in the browser worker and the native portable
        // kernel (harn#6267). PURE_CASES alone cannot catch this, because
        // they only assert successful completions.
        for case in RUNTIME_FAILURE_CASES {
            let native_program = harn_kernel::compile_program(
                case.source,
                case.entry,
                harn_kernel::EntryKind::Function,
            )
            .unwrap_or_else(|diagnostics| panic!("{} did not compile: {diagnostics:?}", case.id));
            let input =
                harn_kernel::DataValue::from_json(serde_json::from_str(case.input_json).unwrap())
                    .unwrap();
            let harn_kernel::Execution::Failed {
                diagnostic: native_diagnostic,
            } = harn_kernel::start(&native_program, input, &harn_kernel::GrantSet::pure())
            else {
                panic!(
                    "{} completed natively; expected {}",
                    case.id, case.expected_code
                )
            };
            assert_eq!(
                native_diagnostic.code, case.expected_code,
                "{} native code drifted",
                case.id
            );

            let compiled = compile(case.source, case.entry, "function");
            assert!(compiled.ok(), "{} did not compile in browser", case.id);
            let browser = start(
                &compiled.artifact_bytes(),
                case.input_json,
                r#"{"capabilities":[]}"#,
            )
            .unwrap_or_else(|error| panic!("{} failed to start: {error:?}", case.id));
            assert_eq!(
                browser.status(),
                "failed",
                "{} did not fail in browser",
                case.id
            );
            assert_eq!(
                browser.diagnostic_json(),
                serde_json::to_string(&native_diagnostic).unwrap(),
                "{} browser diagnostics diverged",
                case.id
            );
        }
    }

    #[wasm_bindgen_test]
    fn browser_benchmark_statistics_use_the_kernel_contract() {
        let statistics: serde_json::Value = serde_json::from_str(
            &summarize_benchmark_samples("[30,10,40,20]")
                .expect("bounded browser samples aggregate"),
        )
        .unwrap();
        assert_eq!(statistics["iterations"], 4);
        assert_eq!(statistics["mean_ms"], 25.0);
        assert_eq!(statistics["p50_ms"], 25.0);
        assert_eq!(statistics["p95_ms"], 38.5);
        assert_eq!(statistics["total_ms"], 100.0);
        assert!(summarize_benchmark_samples("[]").is_err());
        assert!(summarize_benchmark_samples("[-0.1]").is_err());
        assert!(
            summarize_benchmark_samples(&" ".repeat(MAX_BENCHMARK_SAMPLES_JSON_BYTES + 1)).is_err()
        );

        let digest =
            benchmark_terminal_digest(r#"{"count":42}"#).expect("portable terminal value hashes");
        assert_eq!(digest.len(), 64);
        assert_eq!(
            digest,
            benchmark_terminal_digest(r#"{"count":42}"#).unwrap()
        );
        assert_eq!(
            benchmark_terminal_digest(r#"{"b":2,"a":1}"#).unwrap(),
            benchmark_terminal_digest(r#"{"a":1,"b":2}"#).unwrap()
        );

        let provenance: serde_json::Value =
            serde_json::from_str(&benchmark_provenance_json()).unwrap();
        assert_eq!(
            provenance["artifactFormatVersion"],
            harn_kernel::ARTIFACT_VERSION
        );
        assert_eq!(
            provenance["semanticAbiFingerprint"].as_str().unwrap().len(),
            64
        );
        assert_eq!(
            provenance["opcodeAbiFingerprint"].as_str().unwrap().len(),
            64
        );

        let mut receipt = serde_json::json!({
            "schemaVersion": harn_kernel::PORTABLE_BENCHMARK_SCHEMA_VERSION,
            "target": "browser",
            "source": "demo/reducer.harn",
            "entry": "reduce",
            "entryKind": "function",
            "artifactBytes": 128,
            "artifactDigest": "a".repeat(64),
            "iterations": 4,
            "workers": 1,
            "provenance": provenance,
            "initializationMs": 1.0,
            "compile": { "firstMs": 2.0, "repeated": statistics.clone() },
            "decode": null,
            "dispatch": {
                "firstMs": 3.0,
                "repeated": statistics,
                "batchWallMs": 40.0,
                "throughputPerSecond": 100.0
            },
            "terminalDigest": "b".repeat(64)
        });
        let normalized = normalize_benchmark_receipt_json(&receipt.to_string())
            .expect("shared receipt type accepts browser measurements");
        let normalized: serde_json::Value = serde_json::from_str(&normalized).unwrap();
        assert_eq!(
            normalized["schemaVersion"],
            harn_kernel::PORTABLE_BENCHMARK_SCHEMA_VERSION
        );

        receipt["undocumented"] = serde_json::Value::Bool(true);
        assert!(normalize_benchmark_receipt_json(&receipt.to_string()).is_err());
    }
}
