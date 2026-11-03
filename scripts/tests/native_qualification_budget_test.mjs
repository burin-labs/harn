import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import test from "node:test";
import vm from "node:vm";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));
const workflow = JSON.parse(execFileSync("bash", [
  `${root}/scripts/harn_bin.sh`, "--", "run", "-e",
  'import { read_yaml } from "std/fs"\nfn main(harness: Harness) { harness.stdio.println(json_stringify(read_yaml(harness.fs, ".github/workflows/macos-nightly.yml", nil))) }',
], { cwd: root, env: { ...process.env, HARN_BIN_NO_BUILD: "1" }, encoding: "utf8" }));
const steps = workflow.jobs.nextest.steps;

function evaluate(expression, environment, event) {
  const source = expression.replace(/^\$\{\{\s*|\s*\}\}$/g, "");
  return String(vm.runInNewContext(source, {
    runner: { environment },
    github: { event_name: event },
  }));
}

for (const environment of ["self-hosted", "github-hosted"]) {
  for (const event of ["workflow_dispatch", "schedule", "pull_request"]) {
    test(`${environment} ${event} uses the measured compiler budget`, () => {
      const budget = steps.filter((step) => step.id === "resource-budget");
      assert.equal(budget.length, 1);
      assert.equal(budget[0].run, "bash scripts/ci/rust_resource_budget.sh");
      assert.equal(budget[0].env.HARN_BUDGET_PROFILE, "producer");
      assert.equal(evaluate(budget[0].env.RUNNER_ENVIRONMENT, environment, event), environment);
      assert.equal(budget[0]["continue-on-error"], undefined);
      if (budget[0].if !== undefined) {
        assert.equal(evaluate(budget[0].if, environment, event), "true");
      }
      const compile = steps.filter((step) => step.id === "workspace-nextest");
      assert.equal(compile.length, 1);
      assert.ok(steps.indexOf(budget[0]) < steps.indexOf(compile[0]));
      assert.equal(compile[0].env.CARGO_BUILD_JOBS, "${{ steps.resource-budget.outputs.build_jobs }}");
    });
  }
}
