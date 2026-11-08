import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import { Evaluator, Lexer, Parser, data } from "@actions/expressions";
import { load } from "js-yaml";

const workflow = load(
  fs.readFileSync(
    new URL("../../.github/workflows/install-smoke.yml", import.meta.url),
    "utf8",
  ),
);

function expressionValue(value) {
  if (value == null) return new data.Null();
  if (typeof value === "string") return new data.StringData(value);
  if (typeof value === "boolean") return new data.BooleanData(value);
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
  return new Evaluator(tree, expressionValue(context))
    .evaluate()
    .coerceString();
}

function context(eventName, inputs = {}) {
  return {
    github: {
      event_name: eventName,
      ref: "refs/heads/main",
      event:
        eventName === "release" ? { release: { tag_name: "v0.10.192" } } : {},
    },
    inputs,
  };
}

test("publication exercises all three hosted release installers", () => {
  assert.deepEqual(workflow.on.release.types, ["published"]);
  const installer = workflow.jobs["release-installer"];
  assert.equal(installer.if, undefined);
  assert.deepEqual(
    installer.strategy.matrix.include.map(({ platform, runner }) => [
      platform,
      runner,
    ]),
    [
      ["linux", "ubuntu-24.04"],
      ["macos", "macos-latest"],
      ["windows", "windows-latest"],
    ],
  );
  const examples = installer.steps.filter(
    (step) => step.run === "node scripts/smoke_first_hour.mjs",
  );
  assert.equal(examples.length, 1);
  assert.equal(examples[0].if, undefined);
});

test("cold source installs run only weekly or when explicitly requested", () => {
  const condition = workflow.jobs["cargo-install"].if;
  for (const [event, inputs, expected] of [
    ["release", {}, "false"],
    ["pull_request", {}, "false"],
    ["push", {}, "false"],
    ["schedule", {}, "true"],
    ["workflow_dispatch", {}, "true"],
    ["workflow_dispatch", { release_only: true }, "false"],
  ])
    assert.equal(evaluate(condition, context(event, inputs)), expected, event);
});

test("both installers select the publication tag and retain manual/latest modes", () => {
  const installers = workflow.jobs["release-installer"].steps.filter((step) =>
    ["Run install.sh", "Run install.ps1"].includes(step.name),
  );
  assert.equal(installers.length, 2);
  for (const installer of installers) {
    assert.equal(
      evaluate(installer.env.VERSION_INPUT, context("release")),
      "v0.10.192",
    );
    assert.equal(
      evaluate(
        installer.env.VERSION_INPUT,
        context("workflow_dispatch", { version: "v0.10.191" }),
      ),
      "v0.10.191",
    );
    assert.equal(
      evaluate(installer.env.VERSION_INPUT, context("schedule")),
      "",
    );
  }
  const group = workflow.concurrency.group.match(/\$\{\{.*\}\}/)[0];
  assert.equal(evaluate(group, context("release")), "v0.10.192");
  assert.equal(
    evaluate(group, context("workflow_dispatch", { version: "v0.10.192" })),
    "v0.10.192",
  );
});
