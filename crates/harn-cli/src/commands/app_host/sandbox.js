// @ts-check

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

  /** @param {unknown} message */
  function send(message) {
    parent.postMessage(message, hostOrigin);
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
      "object-src 'none'",
    ].join("; ");
  }

  /** @param {string} html @param {string} policy */
  function injectPolicy(html, policy) {
    const escaped = policy.replaceAll("&", "&amp;").replaceAll('"', "&quot;");
    const tag = `<meta http-equiv="Content-Security-Policy" content="${escaped}">`;
    return /<head[\s>]/i.test(html)
      ? html.replace(/<head([^>]*)>/i, `<head$1>${tag}`)
      : tag + html;
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
        view.allow = permissionPolicy(message.params.permissions ?? {});
        view.srcdoc = injectPolicy(
          message.params.html,
          contentSecurityPolicy(message.params.csp ?? {}),
        );
        return;
      }
      view.contentWindow?.postMessage(message, "*");
      return;
    }
    if (event.source === view.contentWindow) {
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
