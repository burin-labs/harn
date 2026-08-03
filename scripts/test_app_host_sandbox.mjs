import assert from "node:assert/strict";
import test from "node:test";

import { installProtocol, runSource } from "./app_host_test_support.mjs";

test("the production sandbox keeps reserved messages at its boundary", async () => {
  const hostMessages = [];
  const viewMessages = [];
  const workers = [];
  const listeners = new Map();
  const parent = {
    postMessage(message, origin) {
      hostMessages.push({ message, origin });
    },
  };
  const viewWindow = {
    postMessage(message, origin) {
      viewMessages.push({ message, origin });
    },
  };
  const view = { allow: "", contentWindow: viewWindow, srcdoc: "" };
  const hostOrigin = "http://127.0.0.1:7777";
  class FakeWorker {
    constructor(url, options) {
      this.url = url;
      this.options = options;
      this.messages = [];
      this.terminated = false;
      workers.push(this);
    }

    postMessage(message, transfer) {
      this.messages.push({ message, transfer });
    }

    terminate() {
      this.terminated = true;
    }
  }
  const context = await installProtocol({
    document: { getElementById: () => view },
    location: { search: `?host_origin=${encodeURIComponent(hostOrigin)}` },
    parent,
    Uint8Array,
    URLSearchParams,
    Worker: FakeWorker,
    window: {
      addEventListener(name, listener) {
        listeners.set(name, listener);
      },
    },
  });
  await runSource(context, "crates/harn-cli/src/commands/app_host/sandbox.js");

  assert.equal(
    hostMessages[0].message.method,
    "ui/notifications/sandbox-proxy-ready",
  );
  const receive = listeners.get("message");
  assert.equal(typeof receive, "function");

  receive({
    data: {
      jsonrpc: "2.0",
      method: "ui/notifications/sandbox-proxy-ready",
      params: {},
    },
    source: viewWindow,
  });
  assert.equal(hostMessages.length, 1);

  receive({
    data: { jsonrpc: "2.0", id: 8, method: "tools/call", params: {} },
    source: viewWindow,
  });
  assert.equal(hostMessages.length, 2);
  assert.equal(hostMessages.at(-1).message.method, "tools/call");

  receive({
    data: {
      jsonrpc: "2.0",
      method: "ui/notifications/sandbox-resource-ready",
      params: {
        html: "<!doctype html><html><head></head><body>Ready</body></html>",
        csp: {
          connectDomains: ["https://api.example.com"],
        },
        permissions: { clipboardWrite: {} },
      },
    },
    origin: hostOrigin,
    source: parent,
  });
  assert.match(view.srcdoc, /Content-Security-Policy/);
  assert.match(view.srcdoc, /connect-src https:\/\/api\.example\.com/);
  assert.match(view.srcdoc, /worker-src 'none'/);
  assert.match(
    view.srcdoc,
    /<meta name="harn-portable-worker" content="available">/,
  );
  assert.equal(view.allow, "clipboard-write");
  assert.equal(viewMessages.length, 0);

  const artifact = new Uint8Array([1, 2, 3]);
  receive({
    data: {
      jsonrpc: "2.0",
      method: "ui/notifications/harn-portable-worker",
      params: {
        schema: "harn.portable_worker.v1",
        kind: "load",
        artifact,
        state: { count: 0 },
        grants: { capabilities: [] },
      },
    },
    source: viewWindow,
  });
  assert.equal(hostMessages.length, 2);
  assert.equal(workers.length, 1);
  assert.equal(workers[0].url, "/runtime/portable-worker.js");
  assert.equal(workers[0].options.type, "module");
  assert.equal(workers[0].messages.length, 1);
  assert.equal(workers[0].messages[0].transfer[0], artifact.buffer);

  workers[0].onmessage({
    data: { schema: "harn.portable_worker.v1", kind: "ready" },
  });
  assert.equal(viewMessages.length, 1);
  assert.equal(
    viewMessages[0].message.method,
    "ui/notifications/harn-portable-worker",
  );
  workers[0].onerror({ message: "worker stopped" });
  assert.equal(workers[0].terminated, true);
  assert.equal(
    viewMessages[1].message.params.diagnostic.code,
    "portable_worker_start",
  );

  receive({
    data: {
      jsonrpc: "2.0",
      method: "ui/notifications/sandbox-resource-ready",
      params: {
        html: '<html><head><base href="https://assets.example/"></head></html>',
      },
    },
    origin: hostOrigin,
    source: parent,
  });
  assert.equal((view.srcdoc.match(/<base\b/gi) ?? []).length, 1);
  assert.match(view.srcdoc, /<base href="https:\/\/assets\.example\/">/);
  assert.equal(workers[0].terminated, true);

  receive({
    data: {
      jsonrpc: "2.0",
      method: "ui/notifications/sandbox-private",
      params: {},
    },
    origin: hostOrigin,
    source: parent,
  });
  assert.equal(viewMessages.length, 2);

  receive({
    data: { jsonrpc: "2.0", id: 8, result: {} },
    origin: hostOrigin,
    source: parent,
  });
  assert.equal(viewMessages.length, 3);
  assert.equal(viewMessages[2].message.id, 8);
});
