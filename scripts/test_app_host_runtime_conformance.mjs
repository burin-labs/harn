import assert from "node:assert/strict";
import test from "node:test";
import { Buffer } from "node:buffer";
import {
  checkRuntimeConformance,
  loadRuntime,
} from "./app_host_runtime_conformance.mjs";

const { WebAssembly } = globalThis;

const corpus = {
  pure: [
    {
      id: "completion",
      source: "complete",
      entry: "project",
      input_json: "null",
      expected_json: "7",
    },
  ],
  failures: [
    {
      id: "failure",
      source: "fail",
      entry: "project",
      input_json: "null",
      expected_code: "known_failure",
    },
  ],
  compilation: [
    {
      id: "compile-rejection",
      package: { rootSource: "invalid", rootImports: [], modules: [] },
      entry: "project",
      expected_ok: false,
      expected_diagnostics: [{ code: "undefined_name", message: "undefined" }],
    },
    {
      id: "compile-acceptance",
      package: { rootSource: "valid", rootImports: [], modules: [] },
      entry: "project",
      expected_ok: true,
      expected_diagnostics: [],
    },
  ],
};

function runtime(value, failureStatus = "failed", diagnostic = "undefined") {
  return {
    compile(source) {
      return { ok: true, artifactBytes: () => source, free() {} };
    },
    compilePackage(packageJson) {
      const { rootSource } = JSON.parse(packageJson);
      const ok = rootSource === "valid";
      return {
        ok,
        diagnosticsJson: () =>
          JSON.stringify(
            ok ? [] : [{ code: "undefined_name", message: diagnostic }],
          ),
        free() {},
      };
    },
    start(artifact, input, grants) {
      assert.equal(input, "null");
      assert.deepEqual(JSON.parse(grants), { capabilities: [] });
      return {
        status: artifact === "complete" ? "completed" : failureStatus,
        valueJson: () => JSON.stringify(value),
        diagnosticJson: () => JSON.stringify({ code: "known_failure" }),
        free() {},
      };
    },
  };
}

test("conformance checks both successful values and runtime failures", () => {
  assert.equal(
    checkRuntimeConformance(runtime(7), corpus, "current"),
    "current: 4/4 portable cases passed",
  );
  assert.throws(
    () => checkRuntimeConformance(runtime(6), corpus, "stale"),
    /stale: 3\/4 cases passed; 1 failed\n {2}completion: expected 7, got 6/,
  );
  assert.throws(
    () => checkRuntimeConformance(runtime(7, "completed"), corpus, "stale"),
    /failure: expected failed, got completed/,
  );
});

test("compilation checks canonical diagnostics as well as rejection", () => {
  assert.throws(
    () =>
      checkRuntimeConformance(
        runtime(7, "failed", "use a removed method"),
        corpus,
        "stale",
      ),
    /stale: 3\/4 cases passed; 1 failed\n {2}compile-rejection: expected diagnostics/,
  );
});

test("missing or empty corpus cannot establish conformance", () => {
  assert.throws(
    () => checkRuntimeConformance(runtime(7), {}, "empty"),
    /completion corpus is empty or missing/,
  );
  assert.throws(
    () =>
      checkRuntimeConformance(
        runtime(7),
        { pure: [], failures: corpus.failures },
        "empty",
      ),
    /completion corpus is empty or missing/,
  );
  assert.throws(
    () =>
      checkRuntimeConformance(
        runtime(7),
        { pure: corpus.pure, failures: [] },
        "empty",
      ),
    /failure corpus is empty or missing/,
  );
  assert.throws(
    () =>
      checkRuntimeConformance(
        runtime(7),
        { ...corpus, compilation: [] },
        "empty",
      ),
    /compilation corpus is empty or missing/,
  );
  assert.throws(
    () =>
      checkRuntimeConformance(
        runtime(7),
        {
          ...corpus,
          compilation: [{ ...corpus.compilation[0], expected_diagnostics: [] }],
        },
        "empty",
      ),
    /expected compilation decision or diagnostics are missing/,
  );
});

test("same-ABI modules retain separate runtime instances", async () => {
  const glue = Buffer.from(`
    let instance;
    export function initSync({ module }) {
      instance = new WebAssembly.Instance(new WebAssembly.Module(module));
    }
    export function value() { return instance.exports.value(); }
  `);
  const bytes = Buffer.from([
    0, 97, 115, 109, 1, 0, 0, 0, 1, 5, 1, 96, 0, 1, 127, 3, 2, 1, 0, 7, 9, 1, 5,
    118, 97, 108, 117, 101, 0, 0, 10, 6, 1, 4, 0, 65, 1, 11,
  ]);
  const changed = Buffer.from(bytes);
  changed[changed.length - 2] = 2;
  assert.deepEqual(
    WebAssembly.Module.exports(new WebAssembly.Module(bytes)),
    WebAssembly.Module.exports(new WebAssembly.Module(changed)),
  );
  const old = await loadRuntime(glue, bytes);
  const fresh = await loadRuntime(glue, changed);
  assert.equal(old.value(), 1);
  assert.equal(fresh.value(), 2);
  assert.equal(old.value(), 1);
});
