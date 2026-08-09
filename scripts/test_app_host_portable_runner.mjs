import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(
  new URL(
    "../crates/harn-cli/src/commands/app_host/portable_runner.js",
    import.meta.url,
  ),
  "utf8",
);

function loadRunner() {
  const context = vm.createContext({ JSON, structuredClone, Uint8Array });
  vm.runInContext(`${source}\nthis.runnerApi = HarnPortableRunner;`, context);
  return context.runnerApi;
}

function outcome(status, fields = {}) {
  return {
    status,
    valueJson: () => JSON.stringify(fields.value),
    requestJson: () => JSON.stringify(fields.request),
    snapshotBytes: () => fields.snapshot ?? new Uint8Array(),
    diagnosticJson: () => JSON.stringify(fields.diagnostic),
  };
}

test("portable runner preserves event order and restored state", () => {
  const sent = [];
  const api = loadRunner();
  const runner = api.create({
    start(_artifact, input) {
      const { state, event } = JSON.parse(input);
      return outcome("completed", {
        value: {
          state: { count: state.count + event.delta },
          update: {
            schema: "harn.ui_update.v1",
            revision: state.count + event.delta,
          },
        },
      });
    },
    resume() {
      throw new Error("resume must not run");
    },
    send(message) {
      sent.push(message);
    },
  });

  runner.receive({
    schema: api.schema,
    kind: "load",
    artifact: new Uint8Array([1]),
    state: { count: 1 },
    grants: { capabilities: [] },
  });
  runner.receive({ schema: api.schema, kind: "event", event: { delta: 2 } });
  runner.receive({ schema: api.schema, kind: "restore", state: { count: 10 } });
  runner.receive({ schema: api.schema, kind: "event", event: { delta: 5 } });

  assert.deepEqual(
    sent.map((message) => [message.kind, message.state?.count]),
    [
      ["ready", 1],
      ["update", 3],
      ["restored", 10],
      ["update", 15],
    ],
  );
});

test("portable runner suspends and resumes only the matching request", () => {
  const sent = [];
  const api = loadRunner();
  const request = {
    id: "request-1",
    capability: "tools",
    operation: "invoke",
    arguments: ["save", { name: "logo.png" }],
    expected: "any",
  };
  const runner = api.create({
    start() {
      return outcome("suspended", {
        request,
        snapshot: new Uint8Array([7]),
      });
    },
    resume(_artifact, snapshot, result) {
      assert.deepEqual([...snapshot], [7]);
      assert.equal(JSON.parse(result).value.path, "logo.png");
      return outcome("completed", {
        value: {
          state: { saved: true },
          update: { schema: "harn.ui_update.v1" },
        },
      });
    },
    send(message) {
      sent.push(message);
    },
  });

  runner.receive({
    schema: api.schema,
    kind: "load",
    artifact: new Uint8Array([1]),
    state: { saved: false },
    grants: { capabilities: ["tools.invoke"] },
  });
  runner.receive({
    schema: api.schema,
    kind: "event",
    event: { kind: "save" },
  });
  runner.receive({
    schema: api.schema,
    kind: "result",
    result: { status: "ok", request_id: "wrong", value: {} },
  });
  runner.receive({
    schema: api.schema,
    kind: "result",
    result: {
      status: "ok",
      request_id: "request-1",
      value: { path: "logo.png" },
    },
  });

  assert.deepEqual(
    sent.map((message) => message.kind),
    ["ready", "request", "failed", "update"],
  );
  assert.equal(sent[2].diagnostic.code, "portable_request_mismatch");
});

test("portable runner rejects malformed grants before becoming ready", () => {
  const sent = [];
  const api = loadRunner();
  const runner = api.create({
    start() {
      throw new Error("start must not run");
    },
    resume() {
      throw new Error("resume must not run");
    },
    send(message) {
      sent.push(message);
    },
  });

  runner.receive({
    schema: api.schema,
    kind: "load",
    artifact: new Uint8Array([1]),
    state: {},
    grants: { capabilities: ["tools.invoke", 3] },
  });

  assert.deepEqual(
    sent.map((message) => [message.kind, message.diagnostic?.code]),
    [["failed", "portable_worker_grants"]],
  );
});
