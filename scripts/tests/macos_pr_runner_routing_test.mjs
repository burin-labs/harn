// Evaluates the real `runs-on` of the macOS workspace tests job for each event GitHub can send,
// so the claim "fork code never reaches a self-hosted runner" is read from the shipped expression.
import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { Evaluator, Lexer, Parser, data } from "@actions/expressions";
import { load } from "js-yaml";

const root = fileURLToPath(new URL("../../", import.meta.url));
const workflow = load(
  fs.readFileSync(`${root}/.github/workflows/macos-nightly.yml`, "utf8"),
);
const job = workflow.jobs.nextest;

function expressionValue(value) {
  if (value == null) return new data.Null();
  if (typeof value === "string") return new data.StringData(value);
  if (typeof value === "number") return new data.NumberData(value);
  if (typeof value === "boolean") return new data.BooleanData(value);
  if (Array.isArray(value))
    return new data.Array(...value.map(expressionValue));
  return new data.Dictionary(
    ...Object.entries(value).map(([key, child]) => ({
      key,
      value: expressionValue(child),
    })),
  );
}

function evaluate(source, context) {
  const expression = source.replace(/^\$\{\{\s*|\s*\}\}$/g, "");
  const tokens = new Lexer(expression).lex();
  const tree = new Parser(tokens.tokens, Object.keys(context), []).parse();
  const pairs = Object.entries(context).map(([key, value]) => ({
    key,
    value: expressionValue(value),
  }));
  return new Evaluator(tree, new data.Dictionary(...pairs)).evaluate();
}

// The runner labels the job would ask for: a list for a self-hosted route, one label otherwise.
function runnerLabels(github) {
  const result = evaluate(job["runs-on"], { github });
  return result.values
    ? result.values().map((label) => label.coerceString())
    : [result.coerceString()];
}

function labelsOf(github) {
  const labels = runnerLabels(github);
  return labels.includes("self-hosted") ? "self-hosted" : labels.join(",");
}

const pullRequest = (headRepo) => ({
  event_name: "pull_request",
  ref: "refs/pull/1/merge",
  event: { pull_request: { head: { repo: { full_name: headRepo } } } },
});

test("a pull request from this repository routes to the owned pull-request runners", () => {
  const github = pullRequest("burin-labs/harn");
  assert.equal(labelsOf(github), "self-hosted");
  assert.deepEqual(runnerLabels(github), ["self-hosted", "harn-pr-macos"]);
});

test("a pull request from a fork stays on the hosted runner, whatever its fork is called", () => {
  for (const head of [
    "someone/harn",
    "burin-labs/harn-fork",
    "burin-labs/harn ",
    "",
  ]) {
    assert.equal(labelsOf(pullRequest(head)), "macos-latest", `head ${JSON.stringify(head)}`);
  }
});

// GitHub compares strings without regard to case, so burin-labs/Harn matches. That is the same
// repository: owner and repository names are unique case-insensitively, so no fork can spell it.
test("the comparison is case-insensitive, which only ever names this repository", () => {
  assert.equal(labelsOf(pullRequest("Burin-Labs/HARN")), "self-hosted");
});

test("a pull request whose head repository is missing from the payload stays hosted", () => {
  const github = { event_name: "pull_request", ref: "refs/pull/1/merge", event: {} };
  assert.equal(labelsOf(github), "macos-latest");
});

test("schedule, push and non-main dispatch stay hosted; only main dispatch takes the trusted group", () => {
  const base = { event: {} };
  assert.equal(labelsOf({ ...base, event_name: "schedule", ref: "refs/heads/main" }), "macos-latest");
  assert.equal(labelsOf({ ...base, event_name: "push", ref: "refs/heads/main" }), "macos-latest");
  assert.equal(labelsOf({ ...base, event_name: "workflow_dispatch", ref: "refs/heads/feature" }), "macos-latest");
  assert.deepEqual(
    runnerLabels({ ...base, event_name: "workflow_dispatch", ref: "refs/heads/main" }),
    ["self-hosted", "harn-trusted-macos"],
  );
});

test("control: the pull-request label is the only one a pull request can ever select", () => {
  for (const head of ["burin-labs/harn", "someone/harn"]) {
    const labels = runnerLabels(pullRequest(head));
    assert.ok(!labels.includes("harn-trusted-macos") && !labels.includes("release-signing"));
  }
});
