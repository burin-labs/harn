import { decodeHarnSessionRecapAvailability } from "../../spec/protocol-artifacts/harn-protocol";
import type { HarnPlanStep, HarnPlanApproval, HarnPlanCommentAnchor } from "../../spec/protocol-artifacts/harn-protocol";

const step: HarnPlanStep = { id: "step", content: "Verify", status: "pending" };
step.priority = null;
step.priority = "high";
const approval: HarnPlanApproval = { state: "unrequested" };
approval.reviewers = ["reviewer"];
// @ts-expect-error Omitted reviewers are supported; null is not a list.
approval.reviewers = null;
const anchor: HarnPlanCommentAnchor = { step_id: "step" };
anchor.range = { start: 0, end: 1 };
// @ts-expect-error Omitted range is supported; null is not a range.
anchor.range = null;

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
recap.snapshot.turns[0].iterations[0].tools[0].verification.futureNested = true;
try {
  decodeHarnSessionRecapAvailability(recap);
  throw new Error("unknown nested recap field was accepted before write-back");
} catch (error) {
  if (
    error instanceof Error &&
    error.message === "unknown nested recap field was accepted before write-back"
  ) {
    throw error;
  }
}

delete recap.snapshot.turns[0].iterations[0].tools[0].verification.futureNested;
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
