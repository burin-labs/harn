#!/usr/bin/env node

import fs from "node:fs";

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

function selectedInventory(document) {
  const suites = document["rust-suites"];
  if (!suites || typeof suites !== "object" || Array.isArray(suites)) {
    fail("inventory has no rust-suites object");
  }

  const selected = new Set();
  let skipped = 0;
  for (const [binaryId, suite] of Object.entries(suites)) {
    if (suite.status !== "listed") continue;
    const packageName = suite["package-name"];
    const binaryName = suite["binary-name"];
    const testcases = suite.testcases;
    if (typeof packageName !== "string" || typeof binaryName !== "string") {
      fail(`inventory suite ${binaryId} has no package-name/binary-name`);
    }
    if (
      !testcases ||
      typeof testcases !== "object" ||
      Array.isArray(testcases)
    ) {
      fail(`inventory suite ${binaryId} has no testcases object`);
    }

    for (const [testName, testcase] of Object.entries(testcases)) {
      const match = testcase?.["filter-match"]?.status;
      if (match === "matches") {
        selected.add(`${packageName}::${binaryName}$${testName}`);
      } else if (match === "mismatch") {
        skipped += 1;
      } else {
        fail(
          `inventory testcase ${binaryId} ${testName} has unknown filter-match status`,
        );
      }
    }
  }
  if (selected.size === 0) fail("inventory selected zero tests");
  const declaredCount = document["test-count"];
  if (declaredCount !== selected.size + skipped) {
    fail(
      `inventory test-count ${declaredCount} does not equal selected+skipped ${selected.size + skipped}`,
    );
  }
  return { selected, skipped };
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

const { selected, skipped } = selectedInventory(
  readJson(args.get("inventory"), "inventory"),
);
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
