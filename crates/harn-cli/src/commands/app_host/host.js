// @ts-check
/* global HarnAppHostProtocol */

/**
 * @typedef {object} AppDescriptor
 * @property {string} resourceUri
 * @property {string} html
 * @property {{csp: ResourcePolicy, permissions: Record<string, object>}} sandbox
 */

(() => {
  "use strict";

  const title = __HARN_TITLE__;
  const sandboxOrigin = __HARN_SANDBOX_ORIGIN__;
  const hostVersion = __HARN_VERSION__;
  const viewConnection = HarnAppHostProtocol.createViewConnection();
  const frame = /** @type {HTMLIFrameElement} */ (
    document.getElementById("sandbox")
  );
  const status = /** @type {HTMLElement} */ (document.getElementById("status"));
  const name = /** @type {HTMLElement} */ (document.querySelector(".name"));
  const uri = /** @type {HTMLElement} */ (document.querySelector(".uri"));

  /** @type {AppDescriptor | null} */
  let descriptor = null;
  name.textContent = title;
  document.title = `${title} — Harn App`;

  /** @param {JsonRpcMessage} message */
  function reply(message) {
    frame.contentWindow?.postMessage(message, sandboxOrigin);
  }

  /** @param {JsonRpcId} id @param {unknown} value */
  function result(id, value) {
    reply({ jsonrpc: "2.0", id, result: value });
  }

  /** @param {JsonRpcId} id @param {number} code @param {string} message */
  function failure(id, code, message) {
    reply({ jsonrpc: "2.0", id, error: { code, message } });
  }

  /** @param {JsonRpcMessage} message */
  async function proxy(message) {
    const response = await fetch("/rpc", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(message),
    });
    const body = /** @type {unknown} */ (await response.json());
    if (!response.ok) {
      const responseError =
        typeof body === "object" &&
        body !== null &&
        "error" in body &&
        typeof body.error === "string"
          ? body.error
          : response.statusText;
      throw new Error(responseError);
    }
    return /** @type {JsonRpcMessage} */ (body);
  }

  /** @param {MessageEvent<JsonRpcMessage>} event */
  async function receive(event) {
    if (
      event.source !== frame.contentWindow ||
      event.origin !== sandboxOrigin
    ) {
      return;
    }
    const message = event.data;
    if (!message || message.jsonrpc !== "2.0") {
      return;
    }
    if (message.method === undefined) {
      return;
    }
    if (typeof message.method !== "string") {
      if (HarnAppHostProtocol.hasRequestId(message)) {
        failure(message.id, -32600, "Request method must be a string");
      }
      return;
    }
    if (
      HarnAppHostProtocol.isViewNotificationMethod(message.method) &&
      !HarnAppHostProtocol.isNotification(message)
    ) {
      if (HarnAppHostProtocol.hasRequestId(message)) {
        failure(message.id, -32600, "Notification must not include an ID");
      }
      return;
    }

    if (message.method === "ui/notifications/sandbox-proxy-ready") {
      descriptor = /** @type {AppDescriptor} */ (
        await fetch("/app").then((value) => value.json())
      );
      uri.textContent = descriptor.resourceUri;
      reply({
        jsonrpc: "2.0",
        method: "ui/notifications/sandbox-resource-ready",
        params: {
          html: descriptor.html,
          csp: descriptor.sandbox.csp,
          permissions: descriptor.sandbox.permissions,
        },
      });
      status.textContent = "ready";
      return;
    }

    if (HarnAppHostProtocol.isSandboxMessage(message)) {
      return;
    }

    if (
      message.method === "ui/initialize" &&
      HarnAppHostProtocol.hasRequestId(message)
    ) {
      const started = viewConnection.initialize(message.params);
      if (!started.ok) {
        failure(message.id, started.code, started.message);
        return;
      }
      result(message.id, {
        protocolVersion: started.protocolVersion,
        hostCapabilities: {
          serverTools: {},
          serverResources: {},
          logging: {},
          sandbox: {
            permissions: descriptor?.sandbox.permissions ?? {},
            csp: descriptor?.sandbox.csp ?? {},
          },
        },
        hostInfo: { name: "harn-app", version: hostVersion },
        hostContext: {
          theme: matchMedia("(prefers-color-scheme: dark)").matches
            ? "dark"
            : "light",
          displayMode: "fullscreen",
          availableDisplayModes: ["fullscreen"],
          containerDimensions: { width: innerWidth, height: innerHeight - 48 },
          locale: navigator.language,
          timeZone: Intl.DateTimeFormat().resolvedOptions().timeZone,
          userAgent: navigator.userAgent,
          platform: "web",
          deviceCapabilities: {
            touch: navigator.maxTouchPoints > 0,
            hover: matchMedia("(hover: hover)").matches,
          },
        },
      });
      return;
    }

    if (message.method === "ui/notifications/initialized") {
      if (viewConnection.markReady()) {
        status.textContent = "connected";
      }
      return;
    }
    if (!viewConnection.isReady()) {
      if (HarnAppHostProtocol.hasRequestId(message)) {
        failure(message.id, -32002, "View is not initialized");
      }
      return;
    }
    if (
      message.method === "ui/update-model-context" &&
      HarnAppHostProtocol.hasRequestId(message)
    ) {
      failure(
        message.id,
        -32000,
        "Model context is not available in standalone apps",
      );
      return;
    }
    if (message.method === "ui/notifications/size-changed") {
      return;
    }
    if (message.method === "notifications/message") {
      console.info("[app]", message.params);
      return;
    }
    if (message.method?.startsWith("ui/")) {
      if (HarnAppHostProtocol.hasRequestId(message)) {
        failure(message.id, -32601, "Host method not supported");
      }
      return;
    }
    if (HarnAppHostProtocol.isServerRequestMethod(message.method)) {
      if (!HarnAppHostProtocol.hasRequestId(message)) {
        return;
      }
      try {
        await HarnAppHostProtocol.proxyServerRequest(message, proxy, reply);
      } catch (error) {
        if (HarnAppHostProtocol.hasRequestId(message)) {
          failure(
            message.id,
            -32000,
            String(error instanceof Error ? error.message : error),
          );
        }
      }
      return;
    }
    if (HarnAppHostProtocol.hasRequestId(message)) {
      failure(message.id, -32601, "Server method not supported");
    }
  }

  window.addEventListener("message", receive);
  window.addEventListener("resize", () => {
    if (!viewConnection.isReady()) {
      return;
    }
    reply({
      jsonrpc: "2.0",
      method: "ui/notifications/host-context-changed",
      params: {
        containerDimensions: { width: innerWidth, height: innerHeight - 48 },
      },
    });
  });
  window.addEventListener("beforeunload", () => {
    if (!viewConnection.isReady()) {
      return;
    }
    reply({
      jsonrpc: "2.0",
      id: "harn-teardown",
      method: "ui/resource-teardown",
      params: {},
    });
  });

  frame.src = `${sandboxOrigin}/sandbox?host_origin=${encodeURIComponent(location.origin)}`;
})();
