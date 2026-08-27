import { decodeHarnSessionRecapAvailability } from "../../spec/protocol-artifacts/harn-protocol";

declare const process: { argv: string[] };
declare function require(name: string): {
  readFileSync(path: string, encoding: string): string;
};

const { readFileSync } = require("node:fs");

const fixturePath = process.argv[2];
if (fixturePath === undefined) {
  throw new Error("usage: protocol_binding_session_recap.js FIXTURE");
}

const fixture = JSON.parse(readFileSync(fixturePath, "utf8"));
const recap = structuredClone(fixture.sessionRecapAvailability);
const decoded = decodeHarnSessionRecapAvailability(recap);
if (decoded.state !== "available" || decoded.snapshot.turns.length !== 1) {
  throw new Error("expected one non-vacuous generated recap turn");
}

recap.snapshot.futureTopLevel = true;
try {
  decodeHarnSessionRecapAvailability(recap);
  throw new Error(
    "unknown recap snapshot field was accepted before write-back",
  );
} catch (error) {
  if (
    error instanceof Error &&
    error.message ===
      "unknown recap snapshot field was accepted before write-back"
  ) {
    throw error;
  }
}

delete recap.snapshot.futureTopLevel;
recap.snapshot.turns[0].iterations[0].tools[0].verification.status =
  "future_status";
try {
  decodeHarnSessionRecapAvailability(recap);
  throw new Error("unknown recap verification status was accepted");
} catch (error) {
  if (
    error instanceof Error &&
    error.message === "unknown recap verification status was accepted"
  ) {
    throw error;
  }
}
