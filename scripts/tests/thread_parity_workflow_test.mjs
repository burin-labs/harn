import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import { Evaluator, Lexer, Parser, data } from "@actions/expressions";
import { load } from "js-yaml";

const workflow = load(fs.readFileSync(new URL("../../.github/workflows/thread-parity.yml", import.meta.url), "utf8"));
const matrix = workflow.jobs["thread-parity"].strategy.matrix.threads;

function selected(event, threads) {
  const context = new data.Dictionary(
    {key: "github", value: new data.Dictionary({key: "event_name", value: new data.StringData(event)})},
    {key: "inputs", value: new data.Dictionary({key: "threads", value: new data.StringData(threads)})},
  );
  const source = matrix.replace(/^\$\{\{\s*|\s*\}\}$/g, "");
  const tokens = new Lexer(source).lex();
  const tree = new Parser(tokens.tokens, ["github", "inputs"], []).parse();
  return new Evaluator(tree, context).evaluate().values().map(value => value.number());
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
