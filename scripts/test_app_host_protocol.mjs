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
