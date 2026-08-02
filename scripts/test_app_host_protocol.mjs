import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const source = await readFile(
  new URL(
    "../crates/harn-cli/src/commands/app_host/protocol.js",
    import.meta.url,
  ),
  "utf8",
);
const context = vm.createContext({});
vm.runInContext(
  `${source}\nglobalThis.protocol = HarnAppHostProtocol;`,
  context,
);
const protocol = context.protocol;

function plain(value) {
  return JSON.parse(JSON.stringify(value));
}

test("successful tool calls send input, response, then result", async () => {
  const sent = [];
  const request = {
    jsonrpc: "2.0",
    id: 7,
    method: "tools/call",
    params: { name: "paint", arguments: { color: "blue" } },
  };
  const response = {
    jsonrpc: "2.0",
    id: 7,
    result: { content: [], structuredContent: { painted: true } },
  };

  await protocol.proxyServerRequest(
    request,
    async (proxied) => {
      assert.deepEqual(proxied, request);
      assert.equal(sent[0].method, "ui/notifications/tool-input");
      return response;
    },
    (message) => sent.push(message),
  );

  assert.deepEqual(plain(sent), [
    {
      jsonrpc: "2.0",
      method: "ui/notifications/tool-input",
      params: { arguments: { color: "blue" } },
    },
    response,
    {
      jsonrpc: "2.0",
      method: "ui/notifications/tool-result",
      params: response.result,
    },
  ]);
});

test("tool errors send input and the error response only", async () => {
  const sent = [];
  const response = {
    jsonrpc: "2.0",
    id: 8,
    error: { code: -32602, message: "invalid arguments" },
  };

  await protocol.proxyServerRequest(
    { jsonrpc: "2.0", id: 8, method: "tools/call", params: {} },
    async () => response,
    (message) => sent.push(message),
  );

  assert.deepEqual(plain(sent), [
    {
      jsonrpc: "2.0",
      method: "ui/notifications/tool-input",
      params: { arguments: {} },
    },
    response,
  ]);
});

test("transport failures do not claim a tool result", async () => {
  const sent = [];
  const failure = new Error("connection closed");

  await assert.rejects(
    protocol.proxyServerRequest(
      { jsonrpc: "2.0", id: 9, method: "tools/call", params: {} },
      async () => {
        throw failure;
      },
      (message) => sent.push(message),
    ),
    failure,
  );

  assert.equal(sent.length, 1);
  assert.equal(sent[0].method, "ui/notifications/tool-input");
});

test("non-tool requests receive only their JSON-RPC response", async () => {
  const sent = [];
  const response = { jsonrpc: "2.0", id: 10, result: { contents: [] } };

  await protocol.proxyServerRequest(
    {
      jsonrpc: "2.0",
      id: 10,
      method: "resources/read",
      params: { uri: "ui://example" },
    },
    async () => response,
    (message) => sent.push(message),
  );

  assert.deepEqual(sent, [response]);
});

test("View startup requires the stable handshake and preserves retry", () => {
  const connection = protocol.createViewConnection();

  assert.deepEqual(
    plain(connection.initialize({ protocolVersion: "2026-01-26" })),
    {
      ok: false,
      code: -32602,
      message: "ui/initialize requires appInfo.name and appInfo.version",
    },
  );
  assert.deepEqual(
    plain(
      connection.initialize({
        protocolVersion: "2026-01-26",
        appInfo: { name: "test-view", version: "1" },
      }),
    ),
    {
      ok: false,
      code: -32602,
      message: "ui/initialize requires appCapabilities",
    },
  );
  assert.equal(connection.isReady(), false);
  assert.deepEqual(
    plain(
      connection.initialize({
        protocolVersion: "2026-01-26",
        appInfo: { name: "test-view", version: "1" },
        appCapabilities: { availableDisplayModes: ["inline"] },
      }),
    ),
    {
      ok: false,
      code: -32602,
      message: "Standalone apps require fullscreen display support",
    },
  );
  assert.deepEqual(
    plain(
      connection.initialize({
        protocolVersion: "2026-01-26",
        appInfo: { name: "test-view", version: "1" },
        appCapabilities: { availableDisplayModes: ["fullscreen"] },
      }),
    ),
    { ok: true, protocolVersion: protocol.appProtocolVersion },
  );
  assert.equal(connection.isReady(), false);
  assert.equal(connection.markReady(), true);
  assert.equal(connection.isReady(), true);
  assert.equal(connection.markReady(), false);
  assert.equal(
    plain(
      connection.initialize({
        protocolVersion: "2026-01-26",
        appInfo: { name: "test-view", version: "1" },
        appCapabilities: {},
      }),
    ).code,
    -32600,
  );
});

test("only reserved sandbox methods are classified as sandbox messages", () => {
  assert.equal(
    protocol.isSandboxMessage({
      method: "ui/notifications/sandbox-proxy-ready",
    }),
    true,
  );
  assert.equal(
    protocol.isSandboxMessage({ method: "ui/notifications/initialized" }),
    false,
  );
  assert.equal(protocol.isSandboxMessage(null), false);
});

test("only server requests supported by the standalone host are forwarded", () => {
  for (const method of ["ping", "tools/call", "resources/read"]) {
    assert.equal(protocol.isServerRequestMethod(method), true, method);
  }
  assert.equal(protocol.isServerRequestMethod("tools/list"), false);
  assert.equal(protocol.isServerRequestMethod("resources/list"), false);
  assert.equal(protocol.isServerRequestMethod("sampling/createMessage"), false);
  assert.equal(
    protocol.isServerRequestMethod("notifications/cancelled"),
    false,
  );
  assert.equal(protocol.isServerRequestMethod(null), false);
});

test("request IDs use the MCP string-or-number shape", () => {
  assert.equal(protocol.hasRequestId({ id: "call-1" }), true);
  assert.equal(protocol.hasRequestId({ id: 1 }), true);
  assert.equal(protocol.hasRequestId({ id: null }), false);
  assert.equal(protocol.hasRequestId({ id: {} }), false);
  assert.equal(protocol.hasRequestId({}), false);
});

test("View notifications are identified by method and omit IDs", () => {
  for (const method of [
    "ui/notifications/sandbox-proxy-ready",
    "ui/notifications/initialized",
    "ui/notifications/size-changed",
    "notifications/message",
  ]) {
    assert.equal(protocol.isViewNotificationMethod(method), true, method);
  }
  assert.equal(protocol.isViewNotificationMethod("tools/call"), false);
  assert.equal(
    protocol.isNotification({ method: "notifications/message" }),
    true,
  );
  assert.equal(
    protocol.isNotification({ id: 1, method: "notifications/message" }),
    false,
  );
  assert.equal(
    protocol.isNotification({ id: null, method: "notifications/message" }),
    false,
  );
});
