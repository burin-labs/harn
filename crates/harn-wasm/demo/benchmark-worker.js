import init, {
  benchmarkProvenanceJson,
  benchmarkSchemaVersion,
  benchmarkTerminalDigest,
  compile,
  normalizeBenchmarkReceiptJson,
  start,
  summarizeBenchmarkSamples,
} from "../pkg/harn_wasm.js";

const COMPILE_ITERATIONS = 30;
const START_ITERATIONS = 500;
const SOURCE_PATH = "crates/harn-wasm/demo/reducer.harn";
const ENTRY = "reduce";
const ENTRY_KIND = "function";

function equalBytes(left, right) {
  if (left.length !== right.length) return false;
  for (let index = 0; index < left.length; index += 1) {
    if (left[index] !== right[index]) return false;
  }
  return true;
}

function statistics(samples) {
  return JSON.parse(summarizeBenchmarkSamples(JSON.stringify(samples)));
}

function completedValue(result) {
  if (result.status === "completed") return result.valueJson();
  const detail = result.status === "failed" ? result.diagnosticJson() : result.requestJson();
  throw new Error(`${result.status}: ${detail}`);
}

try {
  const initializationStarted = performance.now();
  await init();
  const initializationMs = performance.now() - initializationStarted;
  const source = await fetch(new URL("./reducer.harn", import.meta.url)).then((response) => {
    if (!response.ok) throw new Error(`load reducer source: HTTP ${response.status}`);
    return response.text();
  });

  const firstCompileStarted = performance.now();
  const compiled = compile(source, ENTRY, ENTRY_KIND);
  const firstCompileMs = performance.now() - firstCompileStarted;
  if (!compiled.ok) throw new Error(compiled.diagnosticsJson());

  const artifact = compiled.artifactBytes();
  const artifactDigest = compiled.digest;
  const compileSamples = [];
  for (let index = 0; index < COMPILE_ITERATIONS; index += 1) {
    const started = performance.now();
    const sample = compile(source, ENTRY, ENTRY_KIND);
    compileSamples.push(performance.now() - started);
    if (!sample.ok) throw new Error(sample.diagnosticsJson());
    if (sample.digest !== artifactDigest || !equalBytes(sample.artifactBytes(), artifact)) {
      throw new Error("portable compiler emitted different artifact bytes for identical input");
    }
  }

  // Every start receives the exact same immutable JSON text and artifact bytes.
  // Browser start intentionally includes artifact decode and JSON/grant adaptation.
  const inputJson = JSON.stringify({
    state: { count: 0, history: [], label: "portable" },
    event: { kind: "increment", amount: 1 },
  });
  const firstStartStarted = performance.now();
  const grants = JSON.stringify({ capabilities: [] });
  const expectedTerminal = completedValue(start(artifact, inputJson, grants));
  const firstStartMs = performance.now() - firstStartStarted;

  const startSamples = [];
  const batchStarted = performance.now();
  for (let index = 0; index < START_ITERATIONS; index += 1) {
    const started = performance.now();
    const terminal = completedValue(start(artifact, inputJson, grants));
    startSamples.push(performance.now() - started);
    if (terminal !== expectedTerminal) {
      throw new Error("portable start returned a different terminal value for identical input");
    }
  }
  const batchWallMs = performance.now() - batchStarted;

  const adapterProvenance = JSON.parse(benchmarkProvenanceJson());
  const receipt = JSON.parse(normalizeBenchmarkReceiptJson(JSON.stringify({
    schemaVersion: benchmarkSchemaVersion(),
    target: "browser",
    provenance: {
      ...adapterProvenance,
      os: navigator.userAgentData?.platform ?? navigator.platform ?? "unknown",
      arch: "wasm32",
    },
    source: SOURCE_PATH,
    entry: ENTRY,
    entryKind: ENTRY_KIND,
    artifactBytes: artifact.length,
    artifactDigest,
    iterations: START_ITERATIONS,
    workers: 1,
    initializationMs,
    compile: {
      firstMs: firstCompileMs,
      repeated: statistics(compileSamples),
    },
    decode: null,
    dispatch: {
      firstMs: firstStartMs,
      repeated: statistics(startSamples),
      batchWallMs,
      throughputPerSecond: (START_ITERATIONS * 1000) / batchWallMs,
    },
    terminalDigest: benchmarkTerminalDigest(expectedTerminal),
  })));
  postMessage({ kind: "receipt", receipt });
} catch (error) {
  postMessage({
    kind: "error",
    message: error instanceof Error ? error.message : String(error),
  });
}
