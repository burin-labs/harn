import assert from "node:assert/strict";
import test from "node:test";

import { languages, resolveLocale } from "../ui/localization.ts";
import { installProtocol, runSource } from "./app_host_test_support.mjs";

test("standalone locale resolution stays aligned with the website and portal", async () => {
  const context = await installProtocol();
  assert.deepEqual(
    [...context.localization.supportedLocales].sort(),
    Object.keys(languages).sort(),
  );
  for (const input of [
    "en-US",
    "es-MX",
    "fr-CA",
    "de-DE",
    "pt_PT",
    "it-IT",
    "ja-JP",
    "ko-KR",
    "zh-CN",
    "zh-Hans-CN",
    "zh-Hant-TW",
    "zh-HK",
    "unsupported",
    "__proto__",
  ]) {
    assert.equal(
      context.localization.resolve([input]).locale,
      resolveLocale(input) ?? "en",
      input,
    );
  }
  assert.equal(context.localization.resolve(["ja", "fr"]).locale, "ja");
});

for (const locale of Object.keys(languages)) {
  test(`the production host reaches localized connection states in ${locale}`, async () => {
    const listeners = new Map();
    const replies = [];
    const contentWindow = { postMessage: (message) => replies.push(message) };
    const frame = { contentWindow, title: "", src: "" };
    const status = { textContent: "" };
    const document = {
      documentElement: { lang: "en" },
      title: "",
      getElementById: (id) => (id === "sandbox" ? frame : status),
      querySelector: () => ({ textContent: "" }),
    };
    const html = "<h1>Application-owned copy stays unchanged</h1>";
    const context = await installProtocol({
      __HARN_TITLE__: "Card",
      __HARN_VERSION__: "test",
      __HARN_SANDBOX_ORIGIN__: "http://sandbox.invalid",
      document,
      navigator: {
        languages: [locale, "en-US"],
        userAgent: "test",
        maxTouchPoints: 0,
      },
      location: { origin: "http://host.invalid" },
      encodeURIComponent,
      Intl,
      innerWidth: 900,
      innerHeight: 600,
      matchMedia: () => ({ matches: false }),
      fetch: async () => ({
        json: async () => ({
          resourceUri: "ui://card",
          html,
          sandbox: { csp: {}, permissions: {} },
        }),
      }),
      window: {
        addEventListener: (name, callback) => listeners.set(name, callback),
      },
    });
    await runSource(context, "crates/harn-cli/src/commands/app_host/host.js");
    assert.equal(document.documentElement.lang, locale);
    assert.equal(new URL(frame.src).searchParams.get("locale"), locale);
    assert.ok(frame.title.includes("Harn"));
    const starting = status.textContent;
    const send = (message) =>
      listeners.get("message")({
        data: { jsonrpc: "2.0", ...message },
        source: contentWindow,
        origin: "http://sandbox.invalid",
      });
    await send({ method: "ui/notifications/sandbox-proxy-ready" });
    assert.notEqual(status.textContent, starting);
    assert.equal(replies.at(-1).params.html, html);
    const ready = status.textContent;
    if (locale !== "en") {
      assert.notEqual(ready, "ready");
    }
    if (locale === "fr") {
      assert.equal(ready, "Prête");
    }
    await send({
      id: 1,
      method: "ui/initialize",
      params: {
        protocolVersion: "2026-01-26",
        appInfo: { name: "card", version: "1" },
        appCapabilities: {},
      },
    });
    assert.equal(replies.at(-1).result.hostContext.locale, locale);
    await send({ method: "ui/notifications/initialized" });
    assert.notEqual(status.textContent, ready);
    if (locale === "ja") {
      assert.equal(status.textContent, "接続済み");
    }
  });
}

test("the production sandbox uses its host language for accessible frame names", async () => {
  const view = { title: "", contentWindow: { postMessage() {} } };
  const document = {
    documentElement: { lang: "en" },
    title: "",
    getElementById: () => view,
  };
  const context = await installProtocol({
    document,
    location: { search: "?host_origin=http%3A%2F%2Fhost.invalid&locale=ja" },
    navigator: { languages: ["fr"] },
    URLSearchParams,
    parent: { postMessage() {} },
    window: { addEventListener() {} },
  });
  await runSource(context, "crates/harn-cli/src/commands/app_host/sandbox.js");
  assert.equal(document.documentElement.lang, "ja");
  assert.equal(document.title, "Harn アプリのサンドボックス");
  assert.equal(view.title, "Harn アプリの内容");
});
