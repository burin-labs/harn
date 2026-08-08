// @ts-check
/* global HarnPortableRunner */

import init, { resume, start } from "./harn_wasm.js";

/** @returns {string} */
function snapshotKey() {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  return [...bytes]
    .map((value) => value.toString(16).padStart(2, "0"))
    .join("");
}

const ready = init().then(() =>
  HarnPortableRunner.create({
    start,
    resume,
    send(message) {
      self.postMessage(message);
    },
  }),
);

/** @param {unknown} error */
function failStartup(error) {
  self.postMessage({
    schema: HarnPortableRunner.schema,
    kind: "failed",
    diagnostic: {
      code: "portable_worker_start",
      message: error instanceof Error ? error.message : String(error),
    },
  });
}

/** @param {MessageEvent<PortableWorkerMessage>} event */
self.onmessage = async ({ data }) => {
  try {
    const runner = await ready;
    if (data.kind === "load" && Array.isArray(data.grants?.capabilities)) {
      runner.receive({
        ...data,
        grants: {
          capabilities: data.grants.capabilities,
          snapshotKey: snapshotKey(),
        },
      });
      return;
    }
    runner.receive(data);
  } catch (error) {
    failStartup(error);
  }
};
