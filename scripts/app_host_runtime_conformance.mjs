import { isDeepStrictEqual } from "node:util";

let runtimeOrdinal = 0;

export async function loadRuntime(glue, wasm) {
  // Each wasm-bindgen module has its own mutable instance. A unique module URL
  // keeps the fresh and shipped runtimes from sharing that instance.
  const runtime = await import(
    `data:text/javascript;base64,${glue.toString("base64")}#runtime-${runtimeOrdinal++}`
  );
  runtime.initSync({ module: wasm });
  return runtime;
}

export function checkRuntimeConformance(runtime, corpus, label) {
  if (!Array.isArray(corpus.pure) || corpus.pure.length === 0) {
    throw new Error("portable completion corpus is empty or missing");
  }
  if (!Array.isArray(corpus.failures) || corpus.failures.length === 0) {
    throw new Error("portable failure corpus is empty or missing");
  }

  const failures = [];
  const cases = [...corpus.pure, ...corpus.failures];
  const ids = new Set();
  let completed = 0;
  for (const test of cases) {
    if (typeof test.id !== "string" || ids.has(test.id)) {
      throw new Error("portable corpus has a missing or duplicate case ID");
    }
    ids.add(test.id);
    let compiled;
    let outcome;
    try {
      compiled = runtime.compile(test.source, test.entry, "function");
      if (!compiled.ok) {
        throw new Error(`compile failed: ${compiled.diagnosticsJson()}`);
      }
      outcome = runtime.start(
        compiled.artifactBytes(),
        test.input_json,
        '{"capabilities":[]}',
      );
      if (Object.hasOwn(test, "expected_json")) {
        if (outcome.status !== "completed") {
          throw new Error(
            `expected completed, got ${outcome.status}: ${outcome.diagnosticJson()}`,
          );
        }
        const actual = JSON.parse(outcome.valueJson());
        const expected = JSON.parse(test.expected_json);
        if (!isDeepStrictEqual(actual, expected)) {
          throw new Error(
            `expected ${test.expected_json}, got ${outcome.valueJson()}`,
          );
        }
      } else {
        if (typeof test.expected_code !== "string" || !test.expected_code) {
          throw new Error("expected diagnostic code is missing");
        }
        if (outcome.status !== "failed") {
          throw new Error(`expected failed, got ${outcome.status}`);
        }
        const diagnostic = JSON.parse(outcome.diagnosticJson());
        if (diagnostic.code !== test.expected_code) {
          throw new Error(
            `expected ${test.expected_code}, got ${diagnostic.code}`,
          );
        }
      }
      completed++;
    } catch (cause) {
      failures.push(`  ${test.id}: ${cause.message}`);
    } finally {
      outcome?.free();
      compiled?.free();
    }
  }
  if (failures.length > 0) {
    throw new Error(
      `${label}: ${completed}/${cases.length} cases passed; ${failures.length} failed\n${failures.join("\n")}`,
    );
  }
  return `${label}: ${completed}/${cases.length} portable cases passed`;
}
