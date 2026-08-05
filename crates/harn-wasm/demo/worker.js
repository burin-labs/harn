import init, { compilePackage, start } from "../pkg/harn_wasm.js";

let artifact;
let digest = "";
let state = { count: 0, history: [], label: "portable" };

function publish(kind, elapsedMs) {
  postMessage({ kind, state, digest, elapsedMs });
}

function dispatch(event) {
  const started = performance.now();
  const result = start(
    artifact,
    JSON.stringify({ state, event }),
    JSON.stringify({ capabilities: [] }),
  );
  if (result.status === "completed") {
    state = JSON.parse(result.valueJson());
    publish("state", performance.now() - started);
    return;
  }
  const detail = result.status === "failed" ? result.diagnosticJson() : result.requestJson();
  postMessage({ kind: "error", message: `${result.status}: ${detail}` });
}

onmessage = ({ data }) => {
  if (data.kind === "event") dispatch(data.event);
  if (data.kind === "restore") {
    state = structuredClone(data.state);
    publish("state", 0);
  }
};

try {
  const started = performance.now();
  await init();
  const manifest = await fetch(new URL("./package.json", import.meta.url)).then((response) => {
    if (!response.ok) throw new Error(`load reducer package: HTTP ${response.status}`);
    return response.text();
  });
  const result = compilePackage(manifest, "reduce", "function");
  if (!result.ok) throw new Error(result.diagnosticsJson());
  artifact = result.artifactBytes();
  digest = result.digest;
  publish("ready", performance.now() - started);
} catch (error) {
  postMessage({ kind: "error", message: error instanceof Error ? error.message : String(error) });
}
