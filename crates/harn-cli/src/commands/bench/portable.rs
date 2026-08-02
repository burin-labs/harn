use std::fs;
use std::path::Path;
use std::sync::{Condvar, Mutex};
use std::time::Instant;

use harn_kernel::{
    benchmark_terminal_digest, ArtifactLimits, BenchmarkBuildProfile, BenchmarkEntryKind,
    BenchmarkProvenance, BenchmarkStatistics, BenchmarkTarget, CompileMeasurements, DataValue,
    DispatchMeasurements, EntryKind, Execution, GrantSet, PortableBenchmarkReceipt,
    ProgramArtifact, PORTABLE_BENCHMARK_SCHEMA_VERSION, PORTABLE_MAX_COMPILE_ITERATIONS,
    PORTABLE_MAX_DISPATCH_ITERATIONS, PORTABLE_MAX_WORKERS,
};

use super::display_path;
use crate::cli::{BenchPortableArgs, PortableEntryKindArg, ProfileArgs};

#[cfg(test)]
const SCHEMA_PATH: &str = "spec/schemas/portable-kernel-benchmark.v1.schema.json";

#[derive(Debug)]
struct StartState {
    ready: usize,
    released: bool,
    cancelled: bool,
}

pub(super) fn run(args: BenchPortableArgs, profile: &ProfileArgs) -> Result<(), String> {
    if profile.text || profile.json_path.is_some() {
        return Err(
            "`harn bench portable` does not support --profile, --profile-json, HARN_PROFILE, or HARN_PROFILE_JSON; use its versioned receipt or profile a full VM benchmark"
                .to_string(),
        );
    }
    let receipt = collect(&args)?;
    let json = serde_json::to_string_pretty(&receipt)
        .map_err(|error| format!("serialize benchmark receipt: {error}"))?;
    if let Some(path) = args.output.as_deref() {
        write(path, &json)?;
    }
    if args.json {
        println!("{json}");
    } else {
        println!(
            "Portable kernel: {}::{} ({} bytes, {} workers, {} dispatches)",
            receipt.source,
            receipt.entry,
            receipt.artifact_bytes,
            receipt.workers,
            receipt.iterations
        );
        println!(
            "First compile: {:.3} ms | repeated compile p50/p95: {:.3}/{:.3} ms | decode: {:.3}/{:.3} ms",
            receipt.compile.first_ms,
            receipt.compile.repeated.p50_ms,
            receipt.compile.repeated.p95_ms,
            receipt.decode.as_ref().expect("native decode samples").p50_ms,
            receipt.decode.as_ref().expect("native decode samples").p95_ms,
        );
        println!(
            "First dispatch: {:.3} ms | repeated dispatch p50/p95: {:.3}/{:.3} ms | batch: {:.3} ms ({:.1} dispatches/s)",
            receipt.dispatch.first_ms,
            receipt.dispatch.repeated.p50_ms,
            receipt.dispatch.repeated.p95_ms,
            receipt.dispatch.batch_wall_ms,
            receipt.dispatch.throughput_per_second,
        );
        if let Some(path) = args.output {
            println!("Receipt JSON: {}", path.display());
        }
    }
    Ok(())
}

fn collect(args: &BenchPortableArgs) -> Result<PortableBenchmarkReceipt, String> {
    if args.iterations == 0 || args.compile_iterations == 0 {
        return Err("portable benchmark iteration counts must be at least one".to_string());
    }
    if args.threads == 0 {
        return Err("portable benchmark thread count must be at least one".to_string());
    }
    if args.threads > PORTABLE_MAX_WORKERS {
        return Err(format!(
            "portable benchmark worker count must not exceed {PORTABLE_MAX_WORKERS}"
        ));
    }
    if args.iterations > PORTABLE_MAX_DISPATCH_ITERATIONS {
        return Err(format!(
            "portable benchmark dispatch iterations must not exceed {PORTABLE_MAX_DISPATCH_ITERATIONS}"
        ));
    }
    if args.compile_iterations > PORTABLE_MAX_COMPILE_ITERATIONS {
        return Err(format!(
            "portable benchmark compile iterations must not exceed {PORTABLE_MAX_COMPILE_ITERATIONS}"
        ));
    }

    let source = read(&args.source, "source")?;
    let input_json = read(&args.input, "input")?;
    let input = DataValue::from_json(
        serde_json::from_str(&input_json)
            .map_err(|error| format!("invalid JSON in {}: {error}", args.input.display()))?,
    )
    .map_err(|error| format!("{}: {}", error.code, error.message))?;
    let entry_kind = match args.entry_kind {
        PortableEntryKindArg::Function => EntryKind::Function,
        PortableEntryKindArg::Pipeline => EntryKind::Pipeline,
    };

    let first_compile_started = Instant::now();
    let program = harn_kernel::compile_program(&source, &args.entry, entry_kind.clone())
        .map_err(render_diagnostics)?;
    let first_compile_ms = elapsed_ms(first_compile_started);

    let mut compile_samples = Vec::with_capacity(args.compile_iterations);
    for _ in 0..args.compile_iterations {
        let started = Instant::now();
        let repeated = harn_kernel::compile_program(&source, &args.entry, entry_kind.clone())
            .map_err(render_diagnostics)?;
        compile_samples.push(elapsed_ms(started));
        if repeated.bytes() != program.bytes() {
            return Err(
                "portable compiler emitted different artifact bytes for identical input"
                    .to_string(),
            );
        }
    }

    let mut decode_samples = Vec::with_capacity(args.compile_iterations);
    for _ in 0..args.compile_iterations {
        let started = Instant::now();
        ProgramArtifact::decode(program.bytes(), ArtifactLimits::default())
            .map_err(|error| format!("{}: {}", error.code, error.message))?;
        decode_samples.push(elapsed_ms(started));
    }

    let first_started = Instant::now();
    let first = harn_kernel::start(&program, input.clone(), &GrantSet::pure());
    let first_dispatch_ms = elapsed_ms(first_started);
    let expected = completed_value(first)?;

    let workers = args.threads.min(args.iterations);
    let (dispatch_samples, batch_wall_ms) =
        measure_dispatch_batch(&program, &input, &expected, args.iterations, workers)?;
    if batch_wall_ms == 0.0 {
        return Err("benchmark clock did not advance for the dispatch batch".to_string());
    }
    let throughput_per_second = args.iterations as f64 * 1_000.0 / batch_wall_ms;

    let receipt = PortableBenchmarkReceipt {
        schema_version: PORTABLE_BENCHMARK_SCHEMA_VERSION.to_string(),
        target: BenchmarkTarget::Native,
        source: display_path(&args.source),
        entry: args.entry.clone(),
        entry_kind: match args.entry_kind {
            PortableEntryKindArg::Function => BenchmarkEntryKind::Function,
            PortableEntryKindArg::Pipeline => BenchmarkEntryKind::Pipeline,
        },
        artifact_bytes: program.bytes().len(),
        artifact_digest: program.digest_hex(),
        iterations: args.iterations,
        workers,
        provenance: provenance(),
        initialization_ms: None,
        compile: CompileMeasurements {
            first_ms: first_compile_ms,
            repeated: summarize(compile_samples)?,
        },
        decode: Some(summarize(decode_samples)?),
        dispatch: DispatchMeasurements {
            first_ms: first_dispatch_ms,
            repeated: summarize(dispatch_samples)?,
            batch_wall_ms,
            throughput_per_second,
        },
        terminal_digest: benchmark_terminal_digest(&expected),
    };
    receipt.validate()?;
    Ok(receipt)
}

fn measure_dispatch_batch(
    program: &ProgramArtifact,
    input: &DataValue,
    expected: &DataValue,
    iterations: usize,
    workers: usize,
) -> Result<(Vec<f64>, f64), String> {
    let gate = (
        Mutex::new(StartState {
            ready: 0,
            released: false,
            cancelled: false,
        }),
        Condvar::new(),
    );

    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for worker in 0..workers {
            let count = iterations / workers + usize::from(worker < iterations % workers);
            let gate = &gate;
            let handle = std::thread::Builder::new()
                .name(format!("harn-portable-bench-{worker}"))
                .spawn_scoped(scope, move || {
                    let (state_lock, start_signal) = gate;
                    let mut state = state_lock.lock().unwrap_or_else(|error| error.into_inner());
                    state.ready += 1;
                    // Workers and the coordinator share this condition
                    // variable, so wake every waiter when readiness changes.
                    start_signal.notify_all();
                    while !state.released {
                        state = start_signal
                            .wait(state)
                            .unwrap_or_else(|error| error.into_inner());
                    }
                    if state.cancelled {
                        return Err("worker startup cancelled".to_string());
                    }
                    drop(state);

                    let mut local_samples = Vec::with_capacity(count);
                    for _ in 0..count {
                        let started = Instant::now();
                        let execution =
                            harn_kernel::start(program, input.clone(), &GrantSet::pure());
                        let elapsed = elapsed_ms(started);
                        match completed_value(execution) {
                            Ok(value) if value == *expected => local_samples.push(elapsed),
                            Ok(_) => {
                                return Err("terminal value changed".to_string());
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    Ok(local_samples)
                });
            match handle {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    let (state_lock, start_signal) = &gate;
                    let mut state = state_lock.lock().unwrap_or_else(|error| error.into_inner());
                    state.released = true;
                    state.cancelled = true;
                    start_signal.notify_all();
                    drop(state);
                    for handle in handles {
                        let _ = handle.join();
                    }
                    return Err(format!(
                        "failed to create portable benchmark worker {worker}: {error}"
                    ));
                }
            }
        }

        let (state_lock, start_signal) = &gate;
        let mut state = state_lock.lock().unwrap_or_else(|error| error.into_inner());
        while state.ready < workers {
            state = start_signal
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
        let batch_started = Instant::now();
        state.released = true;
        start_signal.notify_all();
        drop(state);

        let mut samples = Vec::with_capacity(iterations);
        for handle in handles {
            match handle.join() {
                Ok(Ok(worker_samples)) => samples.extend(worker_samples),
                Ok(Err(error)) => {
                    return Err(format!("portable dispatch was not deterministic: {error}"));
                }
                Err(_) => return Err("portable benchmark worker panicked".to_string()),
            }
        }
        let batch_wall_ms = elapsed_ms(batch_started);
        if samples.len() != iterations {
            return Err("portable benchmark did not record every dispatch".to_string());
        }
        Ok((samples, batch_wall_ms))
    })
}

fn provenance() -> BenchmarkProvenance {
    BenchmarkProvenance::current(
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) {
            BenchmarkBuildProfile::Debug
        } else {
            BenchmarkBuildProfile::Release
        },
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

fn completed_value(execution: Execution) -> Result<DataValue, String> {
    match execution {
        Execution::Completed { value } => Ok(value),
        Execution::Suspended { request, .. } => Err(format!(
            "execution suspended for {}.{}",
            request.capability, request.operation
        )),
        Execution::Failed { diagnostic } => {
            Err(format!("{}: {}", diagnostic.code, diagnostic.message))
        }
    }
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

fn summarize(samples: Vec<f64>) -> Result<BenchmarkStatistics, String> {
    BenchmarkStatistics::from_samples(samples).map_err(|error| format!("{}: {error}", error.code()))
}

fn read(path: &Path, kind: &str) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("read {kind} {}: {error}", path.display()))
}

fn write(path: &Path, json: &str) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    fs::write(path, format!("{json}\n"))
        .map_err(|error| format!("write {}: {error}", path.display()))
}

fn render_diagnostics(diagnostics: Vec<harn_kernel::Diagnostic>) -> String {
    diagnostics
        .into_iter()
        .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object_keys(value: &serde_json::Value) -> std::collections::BTreeSet<&str> {
        value
            .as_object()
            .expect("schema/value object")
            .keys()
            .map(String::as_str)
            .collect()
    }

    fn benchmark_args(dir: &Path, threads: usize) -> BenchPortableArgs {
        let source = dir.join("reducer.harn");
        let input = dir.join("event.json");
        fs::write(
            &source,
            "fn reduce(input) { return {count: input.count + 1} }",
        )
        .unwrap();
        fs::write(&input, r#"{"count": 41}"#).unwrap();
        BenchPortableArgs {
            source,
            entry: "reduce".to_string(),
            entry_kind: PortableEntryKindArg::Function,
            input,
            iterations: 8,
            threads,
            compile_iterations: 2,
            json: false,
            output: None,
        }
    }

    #[test]
    fn receipt_identity_is_stable_across_thread_counts() {
        let dir = tempfile::tempdir().unwrap();
        let serial = collect(&benchmark_args(dir.path(), 1)).unwrap();
        let parallel = collect(&benchmark_args(dir.path(), 4)).unwrap();

        assert_eq!(serial.schema_version, PORTABLE_BENCHMARK_SCHEMA_VERSION);
        assert_eq!(serial.artifact_digest, parallel.artifact_digest);
        assert_eq!(serial.terminal_digest, parallel.terminal_digest);
        assert_eq!(serial.compile.repeated.iterations, 2);
        assert_eq!(parallel.compile.repeated.iterations, 2);
        assert_eq!(serial.dispatch.repeated.iterations, 8);
        assert_eq!(parallel.dispatch.repeated.iterations, 8);
        assert_eq!(serial.workers, 1);
        assert_eq!(parallel.workers, 4);
        assert!(parallel.dispatch.batch_wall_ms > 0.0);
        assert!(parallel.dispatch.throughput_per_second > 0.0);
        assert_eq!(
            parallel.provenance.artifact_format_version,
            harn_kernel::ARTIFACT_VERSION
        );
        assert_eq!(parallel.provenance.semantic_abi_fingerprint.len(), 64);
        assert_eq!(parallel.provenance.opcode_abi_fingerprint.len(), 64);

        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
        let schema: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(repo_root.join(SCHEMA_PATH)).unwrap())
                .unwrap();
        jsonschema::draft202012::meta::validate(&schema).unwrap();
        let validator = jsonschema::draft202012::new(&schema).unwrap();
        let instance = serde_json::to_value(&parallel).unwrap();
        validator.validate(&instance).unwrap();
        assert_eq!(object_keys(&instance), object_keys(&schema["properties"]));
        assert_eq!(
            object_keys(&instance["provenance"]),
            object_keys(&schema["$defs"]["provenance"]["properties"])
        );
        assert_eq!(
            object_keys(&instance["compile"]),
            object_keys(&schema["$defs"]["compileMeasurements"]["properties"])
        );
        assert_eq!(
            object_keys(&instance["dispatch"]),
            object_keys(&schema["$defs"]["dispatchMeasurements"]["properties"])
        );
        assert_eq!(
            object_keys(&instance["dispatch"]["repeated"]),
            object_keys(&schema["$defs"]["statistics"]["properties"])
        );

        let mut browser_instance = instance.clone();
        browser_instance["target"] = serde_json::json!("browser");
        browser_instance["initializationMs"] = serde_json::json!(1.0);
        browser_instance["decode"] = serde_json::Value::Null;
        validator.validate(&browser_instance).unwrap();

        let mut unexpected_field = instance.clone();
        unexpected_field
            .as_object_mut()
            .unwrap()
            .insert("undocumented".to_string(), serde_json::Value::Bool(true));
        assert!(!validator.is_valid(&unexpected_field));

        let mut too_many_workers = instance.clone();
        too_many_workers["workers"] = serde_json::json!(PORTABLE_MAX_WORKERS + 1);
        assert!(!validator.is_valid(&too_many_workers));

        let mut too_many_dispatches = instance.clone();
        too_many_dispatches["iterations"] = serde_json::json!(PORTABLE_MAX_DISPATCH_ITERATIONS + 1);
        assert!(!validator.is_valid(&too_many_dispatches));

        let mut too_many_compiles = instance;
        too_many_compiles["compile"]["repeated"]["iterations"] =
            serde_json::json!(PORTABLE_MAX_COMPILE_ITERATIONS + 1);
        assert!(!validator.is_valid(&too_many_compiles));
    }

    #[test]
    fn rejects_unbounded_thread_requests_before_reading_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let args = benchmark_args(dir.path(), PORTABLE_MAX_WORKERS + 1);

        assert_eq!(
            collect(&args).unwrap_err(),
            "portable benchmark worker count must not exceed 256"
        );
    }

    #[test]
    fn rejects_full_vm_profile_outputs_explicitly() {
        let dir = tempfile::tempdir().unwrap();
        let text_profile = ProfileArgs {
            text: true,
            json_path: None,
        };

        assert!(run(benchmark_args(dir.path(), 1), &text_profile)
            .unwrap_err()
            .contains("does not support --profile"));

        let json_profile = ProfileArgs {
            text: false,
            json_path: Some(dir.path().join("profile.json")),
        };
        assert!(run(benchmark_args(dir.path(), 1), &json_profile)
            .unwrap_err()
            .contains("does not support --profile"));
    }
}
