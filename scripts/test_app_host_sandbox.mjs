import assert from "node:assert/strict";
import test from "node:test";

import { installProtocol, runSource } from "./app_host_test_support.mjs";

test("the production sandbox keeps reserved messages at its boundary", async () => {
  const hostMessages = [];
  const viewMessages = [];
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
  const context = await installProtocol({
    document: { getElementById: () => view },
    location: { search: `?host_origin=${encodeURIComponent(hostOrigin)}` },
    parent,
    URLSearchParams,
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
  assert.equal(view.allow, "clipboard-write");
  assert.equal(viewMessages.length, 0);

  receive({
    data: {
      jsonrpc: "2.0",
      method: "ui/notifications/sandbox-private",
      params: {},
    },
    origin: hostOrigin,
    source: parent,
  });
  assert.equal(viewMessages.length, 0);

  receive({
    data: { jsonrpc: "2.0", id: 8, result: {} },
    origin: hostOrigin,
    source: parent,
  });
  assert.equal(viewMessages.length, 1);
  assert.equal(viewMessages[0].message.id, 8);
});
