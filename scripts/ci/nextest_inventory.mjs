#!/usr/bin/env node

import fs from "node:fs";
import { pathToFileURL } from "node:url";

// Default nextest runs exclude ignored cases, even when their filter matches.
export function selectedInventory(document) {
  const suites = document["rust-suites"];
  if (!suites || typeof suites !== "object" || Array.isArray(suites)) {
    throw new Error("inventory has no rust-suites object");
  }
  const selected = new Set();
  const unlisted = [];
  let skipped = 0;
  let ignored = 0;
  for (const [binaryId, suite] of Object.entries(suites)) {
    if (suite.status !== "listed") {
      unlisted.push(binaryId);
      continue;
    }
    const packageName = suite["package-name"];
    const binaryName = suite["binary-name"];
    const testcases = suite.testcases;
    if (typeof packageName !== "string" || typeof binaryName !== "string") {
      throw new Error(
        `inventory suite ${binaryId} has no package-name/binary-name`,
      );
    }
    if (
      !testcases ||
      typeof testcases !== "object" ||
      Array.isArray(testcases)
    ) {
      throw new Error(`inventory suite ${binaryId} has no testcases object`);
    }
    for (const [testName, testcase] of Object.entries(testcases)) {
      const match = testcase?.["filter-match"]?.status;
      if (!["matches", "mismatch"].includes(match)) {
        throw new Error(
          `inventory testcase ${binaryId} ${testName} has unknown filter-match status`,
        );
      }
      if (typeof testcase.ignored !== "boolean") {
        throw new Error(
          `inventory testcase ${binaryId} ${testName} has no ignored flag`,
        );
      }
      if (testcase.ignored) ignored += 1;
      if (match === "matches" && !testcase.ignored) {
        const id = `${packageName}::${binaryName}$${testName}`;
        if (selected.has(id))
          throw new Error(`inventory has duplicate testcase ${id}`);
        selected.add(id);
      } else {
        skipped += 1;
      }
    }
  }
  const declaredCount = document["test-count"];
  if (declaredCount !== selected.size + skipped) {
    throw new Error(
      `inventory test-count ${declaredCount} does not equal selected+skipped ${selected.size + skipped}`,
    );
  }
  if (selected.size === 0) throw new Error("inventory selected zero tests");
  return { selected, skipped, ignored, unlisted };
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  try {
    const inventory = selectedInventory(
      JSON.parse(fs.readFileSync(process.argv[2], "utf8")),
    );
    console.error(
      `nextest_inventory eligible=${inventory.selected.size} excluded=${inventory.skipped} ignored=${inventory.ignored} unlisted_suites=${JSON.stringify(inventory.unlisted)}`,
    );
    console.log(inventory.selected.size);
  } catch (error) {
    console.error(`nextest_inventory invalid=${JSON.stringify(error.message)}`);
    process.exitCode = 2;
  }
}
