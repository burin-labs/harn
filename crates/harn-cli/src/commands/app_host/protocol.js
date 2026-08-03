// @ts-check

/**
 * Shared MCP Apps message ordering for the standalone host.
 *
 * Browser wiring supplies `proxy` and `reply`. Tests supply in-process
 * adapters, so the protocol contract does not depend on a DOM or network.
 */
/* exported HarnAppHostProtocol */
const HarnAppHostProtocol = (() => {
  "use strict";

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

  return Object.freeze({ proxyServerRequest });
})();
