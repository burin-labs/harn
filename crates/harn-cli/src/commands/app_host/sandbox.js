// @ts-check
/* global HarnAppHostProtocol */

/**
 * @typedef {object} ResourcePolicy
 * @property {string[]=} connectDomains
 * @property {string[]=} resourceDomains
 * @property {string[]=} frameDomains
 * @property {string[]=} baseUriDomains
 */
/**
 * @typedef {object} SandboxReadyParams
 * @property {string} html
 * @property {ResourcePolicy=} csp
 * @property {Record<string, unknown>=} permissions
 */
/**
 * @typedef {object} JsonRpcMessage
 * @property {'2.0'} jsonrpc
 * @property {string=} method
 * @property {SandboxReadyParams=} params
 */

(() => {
  "use strict";

  const portableMethod = "ui/notifications/harn-portable-worker";
  const portableSchema = "harn.portable_worker.v1";

  /** @returns {string} */
  function readHostOrigin() {
    const value = new URLSearchParams(location.search).get("host_origin");
    if (!value) {
      throw new Error("host_origin is required");
    }
    return value;
  }

  const hostOrigin = readHostOrigin();
  const view = /** @type {HTMLIFrameElement} */ (
    document.getElementById("view")
  );
  /** @type {Worker | null} */
  let portableWorker = null;

  /** @param {unknown} message */
  function send(message) {
    parent.postMessage(message, hostOrigin);
  }

  /** @param {unknown} params */
  function sendPortable(params) {
    view.contentWindow?.postMessage(
      { jsonrpc: "2.0", method: portableMethod, params },
      "*",
    );
  }

  function stopPortableWorker() {
    portableWorker?.terminate();
    portableWorker = null;
  }

  /** @param {string} code @param {unknown} detail */
  function failPortableWorker(code, detail) {
    sendPortable({
      schema: portableSchema,
      kind: "failed",
      diagnostic: { code, message: String(detail) },
    });
  }

  /** @param {unknown} value @returns {value is Record<string, unknown>} */
  function isRecord(value) {
    return typeof value === "object" && value !== null && !Array.isArray(value);
  }

  /** @param {unknown} value */
  function handlePortableWorker(value) {
    const message = isRecord(value) ? value : null;
    if (
      message?.schema !== portableSchema ||
      typeof message.kind !== "string"
    ) {
      failPortableWorker(
        "portable_worker_message",
        "invalid portable worker message",
      );
      return;
    }
    if (message.kind === "load") {
      stopPortableWorker();
      try {
        portableWorker = new Worker("/runtime/portable-worker.js", {
          type: "module",
        });
      } catch (error) {
        failPortableWorker("portable_worker_start", error);
        return;
      }
      portableWorker.onmessage = ({ data }) => sendPortable(data);
      portableWorker.onerror = (event) => {
        failPortableWorker(
          "portable_worker_start",
          event.message || "portable browser worker failed",
        );
        stopPortableWorker();
      };
      portableWorker.onmessageerror = () => {
        failPortableWorker(
          "portable_worker_message",
          "portable browser worker returned an unreadable message",
        );
        stopPortableWorker();
      };
    }
    if (portableWorker === null) {
      failPortableWorker(
        "portable_worker_not_ready",
        "load the portable program before sending work",
      );
      return;
    }
    const transfer =
      message.kind === "load" && message.artifact instanceof Uint8Array
        ? [message.artifact.buffer]
        : [];
    portableWorker.postMessage(message, transfer);
  }

  /** @param {ResourcePolicy} meta */
  function contentSecurityPolicy(meta) {
    const connect = meta.connectDomains ?? [];
    const resources = meta.resourceDomains ?? [];
    const frames = meta.frameDomains ?? [];
    const bases = meta.baseUriDomains ?? [];
    return [
      "default-src 'none'",
      `script-src 'self' 'unsafe-inline' ${resources.join(" ")}`,
      `style-src 'self' 'unsafe-inline' ${resources.join(" ")}`,
      `img-src 'self' data: blob: ${resources.join(" ")}`,
      `font-src 'self' data: ${resources.join(" ")}`,
      `media-src 'self' data: blob: ${resources.join(" ")}`,
      `connect-src ${connect.join(" ") || "'none'"}`,
      `frame-src ${frames.join(" ") || "'none'"}`,
      `base-uri ${bases.join(" ") || "'self'"}`,
      "worker-src 'none'",
      "object-src 'none'",
    ].join("; ");
  }

  /** @param {string} value */
  function escapeAttribute(value) {
    return value
      .replaceAll("&", "&amp;")
      .replaceAll('"', "&quot;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;");
  }

  /** @param {string} html @param {string} policy */
  function injectPolicy(html, policy) {
    const policyTag = `<meta http-equiv="Content-Security-Policy" content="${escapeAttribute(policy)}">`;
    const runtimeTag = '<meta name="harn-portable-worker" content="available">';
    return /<head[\s>]/i.test(html)
      ? html.replace(/<head([^>]*)>/i, `<head$1>${policyTag}${runtimeTag}`)
      : policyTag + runtimeTag + html;
  }

  /** @param {Record<string, unknown>} permissions */
  function permissionPolicy(permissions) {
    return Object.keys(permissions)
      .map((key) =>
        key.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`),
      )
      .join("; ");
  }

  /** @param {MessageEvent<JsonRpcMessage>} event */
  function receive(event) {
    if (event.source === parent && event.origin === hostOrigin) {
      const message = event.data;
      if (
        message?.method === "ui/notifications/sandbox-resource-ready" &&
        message.params
      ) {
        stopPortableWorker();
        view.allow = permissionPolicy(message.params.permissions ?? {});
        view.srcdoc = injectPolicy(
          message.params.html,
          contentSecurityPolicy(message.params.csp ?? {}),
        );
        return;
      }
      if (message?.method === "ui/resource-teardown") {
        stopPortableWorker();
      }
      if (HarnAppHostProtocol.isSandboxMessage(message)) {
        return;
      }
      view.contentWindow?.postMessage(message, "*");
      return;
    }
    if (event.source === view.contentWindow) {
      if (event.data?.method === portableMethod) {
        handlePortableWorker(event.data.params);
        return;
      }
      if (HarnAppHostProtocol.isSandboxMessage(event.data)) {
        return;
      }
      send(event.data);
    }
  }

  window.addEventListener("message", receive);
  send({
    jsonrpc: "2.0",
    method: "ui/notifications/sandbox-proxy-ready",
    params: {},
  });
})();
