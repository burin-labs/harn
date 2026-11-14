import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { Evaluator, Lexer, Parser, data } from "@actions/expressions";
import { load } from "js-yaml";

const workflow = load(fs.readFileSync(new URL("../../.github/workflows/thread-parity.yml", import.meta.url), "utf8"));
const matrix = workflow.jobs["thread-parity"].strategy.matrix.threads;

function evaluate(source, context, names) {
  const expression = source.replace(/^\$\{\{\s*|\s*\}\}$/g, "");
  const tokens = new Lexer(expression).lex();
  const tree = new Parser(tokens.tokens, names, []).parse();
  return new Evaluator(tree, context).evaluate();
}

function selected(event, threads) {
  const context = new data.Dictionary(
    {key: "github", value: new data.Dictionary({key: "event_name", value: new data.StringData(event)})},
    {key: "inputs", value: new data.Dictionary({key: "threads", value: new data.StringData(threads)})},
  );
  return evaluate(matrix, context, ["github", "inputs"]).values().map(value => value.number());
}

test("scheduled and manual defaults preserve every thread count", () => {
  assert.deepEqual(selected("schedule", "1"), [1, 8, 16]);
  assert.deepEqual(selected("workflow_dispatch", ""), [1, 8, 16]);
  assert.deepEqual(selected("workflow_dispatch", "all"), [1, 8, 16]);
  for (const threads of ["1", "8", "16"]) {
    assert.deepEqual(selected("workflow_dispatch", threads), [Number(threads)]);
  }
});

test("untrusted dispatch values select a refused nonempty job", () => {
  for (const threads of ["0", "1,8", "null", "[1]", "1]; echo injected"]) {
    assert.deepEqual(selected("workflow_dispatch", threads), [0]);
  }
});

test("the actual run step passes matrix values as one literal argument", () => {
  const step = workflow.jobs["thread-parity"].steps.find(step => step.id === "parity");
  assert.doesNotMatch(step.run, /\$\{\{/);
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "parity-workflow-"));
  try {
    fs.mkdirSync(path.join(scratch, "scripts/ci"), {recursive: true});
    fs.writeFileSync(path.join(scratch, "scripts/ci/run_thread_parity.sh"),
      '#!/usr/bin/env node\nrequire("node:fs").writeFileSync(process.env.ARGUMENTS_FILE, JSON.stringify(process.argv.slice(2)));\n',
      {mode: 0o755});
    for (const value of ["1", '1; touch injected; $(touch injected) " `touch injected`']) {
      const context = new data.Dictionary({key: "matrix", value: new data.Dictionary({key: "threads", value: new data.StringData(value)})});
      const env = {...process.env, ARGUMENTS_FILE: path.join(scratch, "arguments.json")};
      for (const [name, source] of Object.entries(step.env)) {
        env[name] = evaluate(source, context, ["matrix"]).coerceString();
      }
      const result = spawnSync("bash", ["-e", "-c", step.run], {cwd: scratch, env, encoding: "utf8"});
      assert.equal(result.error, undefined);
      assert.equal(result.status, 0, result.stderr);
      assert.deepEqual(JSON.parse(fs.readFileSync(env.ARGUMENTS_FILE, "utf8")), [value]);
      assert.equal(fs.existsSync(path.join(scratch, "injected")), false);
    }
  } finally {
    fs.rmSync(scratch, {recursive: true, force: true});
  }
});
