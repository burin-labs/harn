import assert from "node:assert/strict";
import test from "node:test";
import { capacityFor, idleCapacity } from "./capacity.ts";
import { workers } from "./workers.ts";

test("every worker has capacity", () => assert.ok(capacityFor(workers) >= workers));
test("capacity stays within budget", () => assert.ok(capacityFor(workers) <= 10));
test("reviewed idle limit is preserved", () => {
  const expected = Number(process.env.EXPECTED_IDLE_CAPACITY);
  assert.equal(idleCapacity, expected);
  assert.equal(capacityFor(1), expected);
});
test("controller environment does not reach tests", () =>
  assert.equal(process.env.SMART_SHIP_PARENT_ONLY_CANARY, undefined));
