// @ts-check

/**
 * Deterministic message loop around the Portable Harn Kernel.
 *
 * The browser worker supplies the Wasm adapter. Tests supply an in-process
 * adapter, so ordering, restore, and capability handling need no browser or
 * clock.
 */
/* exported HarnPortableRunner */
const HarnPortableRunner = (() => {
  "use strict";

  const schema = "harn.portable_worker.v1";

  /** @param {unknown} value @returns {value is Record<string, unknown>} */
  function isRecord(value) {
    return typeof value === "object" && value !== null && !Array.isArray(value);
  }

  /**
   * @param {object} options
   * @param {(artifact: Uint8Array, input: string, grants: string) => PortableOutcome} options.start
   * @param {(artifact: Uint8Array, snapshot: Uint8Array, result: string, grants: string) => PortableOutcome} options.resume
   * @param {(message: Record<string, unknown>) => void} options.send
   */
  function create({ start, resume, send }) {
    /** @type {Uint8Array | null} */
    let artifact = null;
    /** @type {unknown} */
    let state = null;
    /** @type {Record<string, unknown>} */
    let grants = { capabilities: [] };
    /** @type {Uint8Array | null} */
    let snapshot = null;
    /** @type {PortableRequest | null} */
    let request = null;

    /** @param {string} code @param {unknown} detail */
    function fail(code, detail) {
      send({
        schema,
        kind: "failed",
        diagnostic: { code, message: String(detail) },
      });
    }

    /** @param {PortableOutcome} outcome */
    function apply(outcome) {
      if (outcome.status === "completed") {
        const value = /** @type {unknown} */ (JSON.parse(outcome.valueJson()));
        if (!isRecord(value) || !("state" in value) || !("update" in value)) {
          fail(
            "portable_ui_result",
            "portable UI reducer must return {state, update}",
          );
          return;
        }
        state = structuredClone(value.state);
        snapshot = null;
        request = null;
        send({
          schema,
          kind: "update",
          state: structuredClone(state),
          update: structuredClone(value.update),
        });
        return;
      }
      if (outcome.status === "suspended") {
        const nextRequest = /** @type {unknown} */ (
          JSON.parse(outcome.requestJson())
        );
        if (!isRecord(nextRequest) || typeof nextRequest.id !== "string") {
          fail(
            "portable_capability_request",
            "kernel returned an invalid request",
          );
          return;
        }
        request = /** @type {PortableRequest} */ (nextRequest);
        snapshot = outcome.snapshotBytes();
        send({ schema, kind: "request", request: structuredClone(request) });
        return;
      }
      const diagnostic = /** @type {unknown} */ (
        JSON.parse(outcome.diagnosticJson())
      );
      send({ schema, kind: "failed", diagnostic });
    }

    /** @param {PortableWorkerMessage} message */
    function receive(message) {
      if (message.schema !== schema) {
        fail("portable_worker_schema", "unsupported worker message schema");
        return;
      }
      try {
        if (message.kind === "load") {
          if (!(message.artifact instanceof Uint8Array)) {
            fail("portable_artifact", "load requires artifact bytes");
            return;
          }
          if (
            !isRecord(message.grants) ||
            !Array.isArray(message.grants.capabilities) ||
            !message.grants.capabilities.every(
              (capability) => typeof capability === "string",
            )
          ) {
            fail(
              "portable_worker_grants",
              "load requires a string list of capabilities",
            );
            return;
          }
          artifact = message.artifact;
          state = structuredClone(message.state);
          grants = structuredClone(message.grants);
          snapshot = null;
          request = null;
          send({ schema, kind: "ready", state: structuredClone(state) });
          return;
        }
        if (artifact === null) {
          fail(
            "portable_worker_not_ready",
            "load the program before sending work",
          );
          return;
        }
        if (message.kind === "restore") {
          if (snapshot !== null) {
            fail(
              "portable_worker_busy",
              "cannot restore while a request is pending",
            );
            return;
          }
          state = structuredClone(message.state);
          send({ schema, kind: "restored", state: structuredClone(state) });
          return;
        }
        if (message.kind === "event") {
          if (snapshot !== null) {
            fail("portable_worker_busy", "answer the pending request first");
            return;
          }
          apply(
            start(
              artifact,
              JSON.stringify({ state, event: message.event }),
              JSON.stringify(grants),
            ),
          );
          return;
        }
        if (message.kind === "result") {
          if (snapshot === null || request === null) {
            fail("portable_worker_idle", "no capability request is pending");
            return;
          }
          if (message.result.request_id !== request.id) {
            fail(
              "portable_request_mismatch",
              "result does not match the pending request",
            );
            return;
          }
          apply(
            resume(
              artifact,
              snapshot,
              JSON.stringify(message.result),
              JSON.stringify(grants),
            ),
          );
          return;
        }
        fail("portable_worker_message", "unsupported worker message");
      } catch (error) {
        fail(
          "portable_worker_error",
          error instanceof Error ? error.message : error,
        );
      }
    }

    return Object.freeze({ receive });
  }

  return Object.freeze({ create, schema });
})();
