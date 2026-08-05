import assert from "node:assert/strict";
import test from "node:test";

import { installProtocol, plain, runSource } from "./app_host_test_support.mjs";

test("the production host waits for initialization and reports exact capabilities", async () => {
  const replies = [];
  const rpcCalls = [];
  const listeners = new Map();
  const viewWindow = {
    postMessage(message, origin) {
      replies.push({ message, origin });
    },
  };
  const frame = { contentWindow: viewWindow, src: "" };
  const status = { textContent: "starting" };
  const name = { textContent: "" };
  const uri = { textContent: "" };
  const descriptor = {
    resourceUri: "ui://test/card",
    html: "<!doctype html><title>Card</title>",
    sandbox: {
      csp: {
        connectDomains: ["https://api.example.com"],
        resourceDomains: [],
        frameDomains: [],
        baseUriDomains: [],
      },
      permissions: { clipboardWrite: {} },
    },
  };

  const context = await installProtocol({
    __HARN_SANDBOX_ORIGIN__: "http://localhost:7777",
    __HARN_TITLE__: "Card",
    __HARN_VERSION__: "0.0.0-test",
    document: {
      getElementById(id) {
        return id === "sandbox" ? frame : status;
      },
      querySelector(selector) {
        return selector === ".name" ? name : uri;
      },
      title: "",
    },
    encodeURIComponent,
    async fetch(path, options = {}) {
      if (path === "/app") {
        return { json: async () => descriptor };
      }
      assert.equal(path, "/rpc");
      const request = JSON.parse(options.body);
      rpcCalls.push(request);
      return {
        ok: true,
        statusText: "OK",
        json: async () => ({
          jsonrpc: "2.0",
          id: request.id,
          result: { content: [], structuredContent: { updated: true } },
        }),
      };
    },
    innerHeight: 640,
    innerWidth: 960,
    Intl,
    location: { origin: "http://127.0.0.1:7777" },
    matchMedia: () => ({ matches: false }),
    navigator: {
      language: "en-US",
      maxTouchPoints: 0,
      userAgent: "test",
    },
    window: {
      addEventListener(name, listener) {
        listeners.set(name, listener);
      },
    },
  });
  await runSource(context, "crates/harn-cli/src/commands/app_host/host.js");

  const receive = listeners.get("message");
  assert.equal(typeof receive, "function");
  const send = async (message) => {
    await receive({
      data: message,
      origin: "http://localhost:7777",
      source: viewWindow,
    });
  };

  await send({
    jsonrpc: "2.0",
    method: "ui/notifications/sandbox-proxy-ready",
    params: {},
  });
  assert.equal(status.textContent, "ready");
  assert.deepEqual(plain(replies.at(-1).message.params), {
    html: descriptor.html,
    ...descriptor.sandbox,
  });
  replies.length = 0;

  await send({ jsonrpc: "2.0", id: 1, method: "tools/call", params: {} });
  assert.equal(rpcCalls.length, 0);
  assert.equal(replies.at(-1).message.error.code, -32002);

  await send({
    jsonrpc: "2.0",
    id: 2,
    method: "ui/initialize",
    params: {
      protocolVersion: "2026-01-26",
      appInfo: { name: "test-view", version: "1" },
    },
  });
  assert.equal(replies.at(-1).message.error.code, -32602);

  await send({
    jsonrpc: "2.0",
    id: 3,
    method: "ui/initialize",
    params: {
      protocolVersion: "2026-01-26",
      appInfo: { name: "test-view", version: "1" },
      appCapabilities: {},
    },
  });
  const initialized = plain(replies.at(-1).message.result);
  assert.equal(
    initialized.protocolVersion,
    context.protocol.appProtocolVersion,
  );
  assert.deepEqual(initialized.hostCapabilities.serverTools, {});
  assert.deepEqual(initialized.hostCapabilities.serverResources, {});
  assert.deepEqual(initialized.hostCapabilities.sandbox, {
    csp: {
      connectDomains: ["https://api.example.com"],
      resourceDomains: [],
      frameDomains: [],
      baseUriDomains: [],
    },
    permissions: { clipboardWrite: {} },
  });

  await send({ jsonrpc: "2.0", id: 4, method: "ping", params: {} });
  assert.equal(rpcCalls.length, 0);
  assert.equal(replies.at(-1).message.error.code, -32002);

  await send({
    jsonrpc: "2.0",
    method: "ui/notifications/initialized",
    params: {},
  });
  assert.equal(status.textContent, "connected");

  await send({ jsonrpc: "2.0", id: 29, method: 7, params: {} });
  assert.equal(rpcCalls.length, 0);
  assert.equal(replies.at(-1).message.error.code, -32600);

  await send({
    jsonrpc: "2.0",
    id: 28,
    method: "notifications/message",
    params: { level: "info", data: "not a notification" },
  });
  assert.equal(rpcCalls.length, 0);
  assert.equal(replies.at(-1).message.error.code, -32600);

  await send({
    jsonrpc: "2.0",
    id: 30,
    method: "sampling/createMessage",
    params: {},
  });
  assert.equal(rpcCalls.length, 0);
  assert.equal(replies.at(-1).message.error.code, -32601);

  const replyCount = replies.length;
  await send({
    jsonrpc: "2.0",
    method: "tools/call",
    params: { name: "card.update", arguments: {} },
  });
  assert.equal(rpcCalls.length, 0);
  assert.equal(replies.length, replyCount);

  await send({
    jsonrpc: "2.0",
    id: 5,
    method: "ui/update-model-context",
    params: { structuredContent: { selection: "blue" } },
  });
  assert.equal(replies.at(-1).message.error.code, -32000);
  assert.match(replies.at(-1).message.error.message, /not available/);

  replies.length = 0;
  await send({
    jsonrpc: "2.0",
    id: 6,
    method: "tools/call",
    params: { name: "card.update", arguments: { color: "blue" } },
  });
  assert.equal(rpcCalls.length, 1);
  assert.deepEqual(
    plain(replies.map(({ message }) => message.method ?? message.id)),
    ["ui/notifications/tool-input", 6, "ui/notifications/tool-result"],
  );
});
