// The dispatcher runs from the base branch, so its `if` is the gate. This evaluates the shipped
// expression for each pull request shape and checks the job never reaches pull-request code.
import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { Evaluator, Lexer, Parser, data } from "@actions/expressions";
import { load } from "js-yaml";

const root = fileURLToPath(new URL("../../", import.meta.url));
const read = (name) => load(fs.readFileSync(`${root}/.github/workflows/${name}`, "utf8"));
const dispatcher = read("macos-pr-dispatch.yml");
const tests = read("macos-pr-tests.yml");
const job = dispatcher.jobs.dispatch;

function expressionValue(value) {
  if (value == null) return new data.Null();
  if (typeof value === "string") return new data.StringData(value);
  if (typeof value === "number") return new data.NumberData(value);
  if (typeof value === "boolean") return new data.BooleanData(value);
  if (Array.isArray(value)) return new data.Array(...value.map(expressionValue));
  return new data.Dictionary(
    ...Object.entries(value).map(([key, child]) => ({ key, value: expressionValue(child) })),
  );
}

function admits(github) {
  const expression = String(job.if).replace(/^\$\{\{\s*|\s*\}\}$/g, "");
  const tokens = new Lexer(expression).lex();
  const tree = new Parser(tokens.tokens, ["github"], []).parse();
  const context = new data.Dictionary({ key: "github", value: expressionValue(github) });
  return new Evaluator(tree, context).evaluate().coerceString() === "true";
}

const event = (headRepo, draft = false) => ({
  repository: "burin-labs/harn",
  event: { pull_request: { draft, head: { repo: { full_name: headRepo } } } },
});

test("a pull request from a branch of this repository is dispatched", () => {
  assert.equal(admits(event("burin-labs/harn")), true);
});

test("a fork-shaped event is never dispatched", () => {
  for (const head of ["someone/harn", "burin-labs/harn-fork", "burin-labs/harn2", ""]) {
    assert.equal(admits(event(head)), false, JSON.stringify(head));
  }
  assert.equal(admits({ repository: "burin-labs/harn", event: { pull_request: { draft: false } } }), false);
});

test("a draft pull request is not dispatched", () => {
  assert.equal(admits(event("burin-labs/harn", true)), false);
});

test("the dispatcher never checks out or runs pull-request code", () => {
  assert.deepEqual(Object.keys(dispatcher.on), ["pull_request_target"]);
  for (const step of job.steps) {
    assert.equal(step.uses, undefined, "no action, in particular no checkout");
    // Event fields reach the shell through env, never by being spliced into the script text.
    assert.ok(!/\$\{\{/.test(step.run), "no expression inside the script");
  }
  assert.deepEqual(job.permissions, { actions: "write" });
});

test("the tests workflow is dispatch-only and its runner choice is pinned to main", () => {
  assert.deepEqual(Object.keys(tests.on), ["workflow_dispatch"]);
  const runsOn = String(tests.jobs.nextest["runs-on"]);
  assert.match(runsOn, /github\.ref == 'refs\/heads\/main' && fromJSON/);
  assert.match(runsOn, /harn-pr-macos/);
});

test("the check is reported by a hosted job that holds the only write permission", () => {
  assert.equal(tests.jobs.report["runs-on"], "ubuntu-24.04");
  assert.deepEqual(tests.jobs.report.permissions, { checks: "write" });
  assert.equal(tests.jobs.nextest.permissions, undefined, "the owned job keeps the read-only default");
  assert.equal(tests.permissions.contents, "read");
});

test("the isolation probe is fetched from main and runs before any pull-request code", () => {
  const steps = tests.jobs.nextest.steps;
  assert.equal(steps[0].with.ref, "main");
  assert.equal(steps[0].with.path, "trusted");
  assert.match(steps[1].run, /trusted\/scripts\/ci\/runner_isolation_probe\.sh/);
  const firstPullRequestFetch = steps.findIndex((step) => String(step.with?.ref).includes("inputs.sha"));
  assert.ok(firstPullRequestFetch > 1, "the pull request head is checked out only after the probe");
});
