// @ts-check

/**
 * Shared MCP Apps connection and message rules for the standalone host.
 *
 * Browser wiring supplies `proxy` and `reply`. Tests supply in-process
 * adapters, so the protocol contract does not depend on a DOM or network.
 */
/* exported HarnAppHostProtocol */
const HarnAppHostProtocol = (() => {
  "use strict";

  const appProtocolVersion = "2026-01-26";
  const displayModeNames = new Set(["inline", "fullscreen", "pip"]);
  const sandboxMethodPrefix = "ui/notifications/sandbox-";
  const portableWorkerMethod = "ui/notifications/harn-portable-worker";
  const serverRequestMethods = new Set([
    "ping",
    "tools/call",
    "resources/read",
  ]);
  const viewNotificationMethods = new Set([
    "ui/notifications/sandbox-proxy-ready",
    "ui/notifications/initialized",
    "ui/notifications/size-changed",
    "notifications/message",
  ]);

  /** @param {JsonRpcMessage} message */
  function toolArguments(message) {
    const value = message.params?.arguments;
    return typeof value === "object" && value !== null && !Array.isArray(value)
      ? value
      : {};
  }

  /** @param {unknown} value @returns {Record<string, unknown> | null} */
  function record(value) {
    return typeof value === "object" && value !== null && !Array.isArray(value)
      ? /** @type {Record<string, unknown>} */ (value)
      : null;
  }

  /** @param {unknown} value */
  function isImplementationInfo(value) {
    const info = record(value);
    return (
      info !== null &&
      typeof info.name === "string" &&
      typeof info.version === "string"
    );
  }

  /**
   * Track View startup order without depending on browser state.
   *
   * A rejected initialize request leaves the connection open for a retry.
   */
  function createViewConnection() {
    let phase = "new";

    /** @param {Record<string, unknown> | undefined} params */
    function initialize(params) {
      if (phase !== "new") {
        return {
          ok: false,
          code: -32600,
          message: "View initialization has already started",
        };
      }
      if (
        typeof params?.protocolVersion !== "string" ||
        params.protocolVersion.length === 0
      ) {
        return {
          ok: false,
          code: -32602,
          message: "ui/initialize requires protocolVersion",
        };
      }
      if (!isImplementationInfo(params.appInfo)) {
        return {
          ok: false,
          code: -32602,
          message: "ui/initialize requires appInfo.name and appInfo.version",
        };
      }
      const appCapabilities = record(params.appCapabilities);
      if (appCapabilities === null) {
        return {
          ok: false,
          code: -32602,
          message: "ui/initialize requires appCapabilities",
        };
      }
      const availableDisplayModes = appCapabilities.availableDisplayModes;
      if (
        availableDisplayModes !== undefined &&
        (!Array.isArray(availableDisplayModes) ||
          !availableDisplayModes.every(
            (mode) => typeof mode === "string" && displayModeNames.has(mode),
          ))
      ) {
        return {
          ok: false,
          code: -32602,
          message: "appCapabilities.availableDisplayModes is invalid",
        };
      }
      if (
        Array.isArray(availableDisplayModes) &&
        !availableDisplayModes.includes("fullscreen")
      ) {
        return {
          ok: false,
          code: -32602,
          message: "Standalone apps require fullscreen display support",
        };
      }
      phase = "initializing";
      return { ok: true, protocolVersion: appProtocolVersion };
    }

    function markReady() {
      if (phase !== "initializing") {
        return false;
      }
      phase = "ready";
      return true;
    }

    function isReady() {
      return phase === "ready";
    }

    return Object.freeze({ initialize, isReady, markReady });
  }

  /** @param {unknown} message */
  function isSandboxMessage(message) {
    const value = record(message);
    return (
      typeof value?.method === "string" &&
      (value.method.startsWith(sandboxMethodPrefix) ||
        value.method === portableWorkerMethod)
    );
  }

  /** @param {unknown} message */
  function hasRequestId(message) {
    const value = record(message);
    return (
      typeof value?.id === "string" ||
      (typeof value?.id === "number" && Number.isFinite(value.id))
    );
  }

  /** @param {unknown} message */
  function isNotification(message) {
    const value = record(message);
    return value !== null && !Object.hasOwn(value, "id");
  }

  /** @param {unknown} method */
  function isServerRequestMethod(method) {
    return typeof method === "string" && serverRequestMethods.has(method);
  }

  /** @param {unknown} method */
  function isViewNotificationMethod(method) {
    return typeof method === "string" && viewNotificationMethods.has(method);
  }

  /**
   * Proxy one View request and project the MCP Apps tool lifecycle.
   *
   * @param {JsonRpcMessage} message
   * @param {(message: JsonRpcMessage) => Promise<JsonRpcMessage>} proxy
   * @param {(message: JsonRpcMessage) => void} reply
   */
  async function proxyServerRequest(message, proxy, reply) {
    const isToolCall = message.method === "tools/call";
    if (isToolCall) {
      reply({
        jsonrpc: "2.0",
        method: "ui/notifications/tool-input",
        params: { arguments: toolArguments(message) },
      });
    }

    const response = await proxy(message);
    reply(response);

    const toolResult = record(response.result);
    if (isToolCall && response.error === undefined && toolResult !== null) {
      reply({
        jsonrpc: "2.0",
        method: "ui/notifications/tool-result",
        params: toolResult,
      });
    }
  }

  return Object.freeze({
    appProtocolVersion,
    createViewConnection,
    hasRequestId,
    isNotification,
    isSandboxMessage,
    isServerRequestMethod,
    isViewNotificationMethod,
    proxyServerRequest,
  });
})();
