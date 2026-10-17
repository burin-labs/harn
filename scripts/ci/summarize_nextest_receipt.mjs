#!/usr/bin/env node

import fs from "node:fs";
import { selectedInventory } from "./nextest_inventory.mjs";

function fail(message) {
  console.error(
    `thread_parity_receipt reason=receipt-invalid detail=${JSON.stringify(message)}`,
  );
  process.exit(2);
}

function parseArgs(argv) {
  const values = new Map();
  for (let index = 0; index < argv.length; index += 2) {
    const key = argv[index];
    const value = argv[index + 1];
    if (!key?.startsWith("--") || value === undefined) {
      fail(`invalid argument sequence at ${key ?? "end of arguments"}`);
    }
    values.set(key.slice(2), value);
  }
  for (const required of ["inventory", "events", "runner-status", "threads"]) {
    if (!values.has(required)) fail(`missing --${required}`);
  }
  return values;
}

function readJson(path, label) {
  try {
    return JSON.parse(fs.readFileSync(path, "utf8"));
  } catch (error) {
    fail(`${label} is not valid JSON: ${error.message}`);
  }
}

function finishedEvents(path, selected) {
  const finished = new Map();
  let text;
  try {
    text = fs.readFileSync(path, "utf8");
  } catch (error) {
    fail(`events could not be read: ${error.message}`);
  }
  for (const [index, line] of text.split(/\r?\n/u).entries()) {
    if (!line.trim()) continue;
    let event;
    try {
      event = JSON.parse(line);
    } catch (error) {
      fail(`events line ${index + 1} is not JSON: ${error.message}`);
    }
    if (event.type !== "test" || !["ok", "failed"].includes(event.event))
      continue;
    if (typeof event.name !== "string")
      fail(`events line ${index + 1} has no test name`);
    if (!selected.has(event.name))
      fail(`finished test is absent from inventory: ${event.name}`);
    if (finished.has(event.name))
      fail(`test has multiple terminal events: ${event.name}`);
    finished.set(event.name, event.event);
  }
  return finished;
}

const args = parseArgs(process.argv.slice(2));
const runnerStatus = Number.parseInt(args.get("runner-status"), 10);
const threads = Number.parseInt(args.get("threads"), 10);
if (!Number.isSafeInteger(runnerStatus) || runnerStatus < 0)
  fail("runner status is invalid");
if (!Number.isSafeInteger(threads) || threads < 1)
  fail("thread count is invalid");

let inventory;
try {
  inventory = selectedInventory(readJson(args.get("inventory"), "inventory"));
} catch (error) {
  fail(error.message);
}
const { selected, skipped } = inventory;
const finished = finishedEvents(args.get("events"), selected);
const passed = [...finished.values()].filter((event) => event === "ok").length;
const failed = finished.size - passed;
const missing = [...selected].filter((name) => !finished.has(name)).sort();
const reason =
  missing.length > 0
    ? "tests-not-run"
    : failed > 0
      ? "test-failure"
      : runnerStatus !== 0
        ? "runner-error"
        : "complete";

console.log(
  `thread_parity_receipt reason=${reason} threads=${threads} selected=${selected.size}` +
    ` run=${finished.size} passed=${passed} skipped=${skipped}` +
    ` not_run=${missing.length} failed=${failed} runner_status=${runnerStatus}`,
);
for (const name of missing) console.error(`thread_parity_missing_test ${name}`);

if (reason !== "complete") process.exit(1);
