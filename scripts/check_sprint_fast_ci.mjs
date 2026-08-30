#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import process from "node:process";

const require = createRequire(import.meta.url);
const yaml = require("js-yaml");

const workflowPath = process.argv[2] ?? ".github/workflows/ci.yml";
const workflow = yaml.load(readFileSync(workflowPath, "utf8"));
const jobs = workflow?.jobs ?? {};
const errors = [];
const slowJobs = [
  "rust-security",
  "harn-audit",
  "windows-cross-check",
  "macos",
];

const changes = jobs.changes;
if (
  !String(changes?.outputs?.run_slow_ci ?? "").includes(
    "steps.sprint_fast_ci.outputs.run_slow_ci",
  )
) {
  errors.push("changes must publish the sprint_fast_ci run_slow_ci output");
}
const resolver = changes?.steps?.find((step) => step?.id === "sprint_fast_ci");
if (
  !String(resolver?.run ?? "").includes("scripts/ci_sprint_fast_ci.sh resolve")
) {
  errors.push(
    "changes must resolve sprint fast-CI policy through the owning script",
  );
}

for (const jobId of slowJobs) {
  const job = jobs[jobId];
  const needs = Array.isArray(job?.needs)
    ? job.needs
    : [job?.needs].filter(Boolean);
  if (!needs.includes("changes")) {
    errors.push(`${jobId} must depend on changes`);
  }
  if (
    !String(job?.if ?? "").includes(
      "needs.changes.outputs.run_slow_ci == 'true'",
    )
  ) {
    errors.push(`${jobId} must consume the typed run_slow_ci decision`);
  }
}

const status = jobs["ci-status"];
const statusNeeds = Array.isArray(status?.needs)
  ? status.needs
  : [status?.needs].filter(Boolean);
for (const jobId of slowJobs) {
  if (!statusNeeds.includes(jobId)) {
    errors.push(
      `ci-status must retain ${jobId} for one-switch full-mode restoration`,
    );
  }
}
const statusStep = status?.steps?.find(
  (step) => step?.name === "Verify required CI jobs",
);
if (
  statusStep?.env?.RUN_SLOW_CI !== "${{ needs.changes.outputs.run_slow_ci }}"
) {
  errors.push("ci-status must read the typed run_slow_ci output");
}
if (
  !String(statusStep?.run ?? "").includes("scripts/ci_sprint_fast_ci.sh verify")
) {
  errors.push(
    "ci-status must report the slow-check census through the owning script",
  );
}

if (errors.length > 0) {
  for (const error of errors) process.stderr.write(`error: ${error}\n`);
  process.exit(1);
}
process.stdout.write(
  `sprint fast-CI workflow contract: OK (${slowJobs.length} slow jobs)\n`,
);
