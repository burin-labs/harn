import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { Evaluator, Lexer, Parser, data } from "@actions/expressions";
import { load } from "js-yaml";

const root = fileURLToPath(new URL("../../", import.meta.url));
const repository = "burin-labs/harn";
const workflow = load(
  fs.readFileSync(`${root}/.github/workflows/smart-ship.yml`, "utf8"),
);
const availability = workflow.jobs.availability;
const ship = workflow.jobs.ship;

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

function admits(context) {
  try {
    return evaluate(availability.if, context).coerceString() === "true";
  } catch {
    // GitHub refuses a job when evaluating malformed fromJSON input fails.
    return false;
  }
}

function pullRequestContext({
  eventName = "pull_request_target",
  action = "labeled",
  number = 42,
  headRepository = repository,
  baseRepository = repository,
  baseRef = "main",
  labels = [{ name: "ship" }],
  changedLabel = "ship",
  selected = "42",
} = {}) {
  return {
    github: {
      repository,
      repository_id: "1192706077",
      ref: "refs/heads/topic",
      event_name: eventName,
      event: {
        action,
        label: { name: changedLabel },
        pull_request: {
          number,
          head: { repo: { full_name: headRepository } },
          base: { repo: { full_name: baseRepository }, ref: baseRef },
          labels,
        },
      },
    },
    vars: { HARN_SMART_SHIP_PR: selected },
    inputs: { pr: "" },
  };
}

test("the shipped caller admits only the selected same-repository main PR", () => {
  assert.equal(admits(pullRequestContext()), true);
  for (const [name, change] of [
    ["a different PR", { number: 43 }],
    ["a fork head", { headRepository: "contributor/harn" }],
    ["a foreign base repository", { baseRepository: "contributor/harn" }],
    ["a non-main base", { baseRef: "release" }],
    ["a missing ship label", { labels: [] }],
    [
      "an unrelated label removal",
      { action: "unlabeled", changedLabel: "docs", labels: [] },
    ],
  ]) {
    assert.equal(
      admits(pullRequestContext(change)),
      false,
      `${name} must be refused`,
    );
  }
  const otherRepository = pullRequestContext();
  otherRepository.github.repository = "burin-labs/other";
  assert.equal(
    admits(otherRepository),
    false,
    "another repository must be refused",
  );
});

test("the shipped caller admits only a deliberate stop event when ship is removed", () => {
  assert.equal(
    admits(pullRequestContext({ action: "unlabeled", labels: [] })),
    true,
  );
});

test("the shipped caller refuses empty, invalid, nonpositive, and noncanonical selected PR values", () => {
  for (const selected of ["", "x", "0", "-42", "42.5", "042", "null", "[]"]) {
    assert.equal(
      admits(pullRequestContext({ selected })),
      false,
      `selected PR ${JSON.stringify(selected)} must be refused`,
    );
  }
});

test("the shipped caller dispatches only the selected PR from main", () => {
  const context = pullRequestContext({ eventName: "workflow_dispatch" });
  context.github.ref = "refs/heads/main";
  context.inputs.pr = "42";
  assert.equal(admits(context), true);
  assert.equal(admits({ ...context, inputs: { pr: "43" } }), false);
  assert.equal(admits({ ...context, inputs: { pr: "" } }), false);
  assert.equal(
    admits({
      ...context,
      inputs: { pr: "42.5" },
      vars: { HARN_SMART_SHIP_PR: "42.5" },
    }),
    false,
  );
  assert.equal(
    admits({
      ...context,
      github: { ...context.github, ref: "refs/heads/topic" },
    }),
    false,
  );
});

test("the shipped caller admits the actual selected owned pull request workflow run", () => {
  const context = pullRequestContext({
    eventName: "workflow_run",
    number: 10885,
    selected: "10885",
  });
  context.github.event = {
    workflow_run: {
      event: "pull_request",
      head_repository: { full_name: repository },
      pull_requests: [
        {
          number: 10885,
          base: {
            repo: {
              id: 1192706077,
              name: "harn",
              url: "https://api.github.com/repos/burin-labs/harn",
            },
            ref: "main",
          },
        },
      ],
    },
  };
  assert.equal(
    context.github.event.workflow_run.pull_requests[0].base.repo.full_name,
    undefined,
    "the actual workflow_run base repository shape has no full_name",
  );
  assert.equal(
    evaluate(
      "github.event.workflow_run.pull_requests[0].base.repo.full_name == github.repository",
      context,
    ).coerceString(),
    "false",
    "the previous admission check rejects the actual workflow_run payload",
  );
  assert.equal(admits(context), true);
  assert.equal(
    admits({
      ...context,
      github: {
        ...context.github,
        event: {
          workflow_run: {
            ...context.github.event.workflow_run,
            event: "push",
          },
        },
      },
    }),
    false,
  );
  assert.equal(
    admits({
      ...context,
      github: {
        ...context.github,
        event: {
          workflow_run: {
            ...context.github.event.workflow_run,
            head_repository: { full_name: "contributor/harn" },
          },
        },
      },
    }),
    false,
  );
  assert.equal(
    admits({
      ...context,
      github: {
        ...context.github,
        event: {
          workflow_run: {
            ...context.github.event.workflow_run,
            pull_requests: [
              {
                ...context.github.event.workflow_run.pull_requests[0],
                base: {
                  ...context.github.event.workflow_run.pull_requests[0].base,
                  repo: { id: 1, name: "harn", url: "https://example.test" },
                },
              },
            ],
          },
        },
      },
    }),
    false,
    "a foreign base repository id must be refused",
  );
  assert.equal(
    admits({
      ...context,
      github: {
        ...context.github,
        event: {
          workflow_run: {
            ...context.github.event.workflow_run,
            pull_requests: [
              {
                ...context.github.event.workflow_run.pull_requests[0],
                number: 10886,
              },
            ],
          },
        },
      },
    }),
    false,
  );
  assert.equal(
    admits({
      ...context,
      github: {
        ...context.github,
        event: {
          workflow_run: {
            ...context.github.event.workflow_run,
            pull_requests: [
              {
                ...context.github.event.workflow_run.pull_requests[0],
                base: {
                  ...context.github.event.workflow_run.pull_requests[0].base,
                  ref: "release",
                },
              },
            ],
          },
        },
      },
    }),
    false,
  );
  assert.equal(
    admits({
      ...context,
      github: {
        ...context.github,
        event: {
          workflow_run: {
            ...context.github.event.workflow_run,
            pull_requests: [],
          },
        },
      },
    }),
    false,
    "a missing associated pull request must be refused",
  );
});

test("the shipped workflow exposes one selected PR and no scheduled sweep", () => {
  assert.equal(workflow.on.schedule, undefined);
  assert.deepEqual(workflow.on.workflow_run.workflows, ["CI"]);
  assert.equal(workflow.on.workflow_dispatch.inputs.pr.required, true);
  assert.equal(workflow.on.workflow_dispatch.inputs.pr.default, undefined);
  assert.match(
    availability.uses,
    /^burin-labs\/\.github\/\.github\/workflows\/runner-availability\.yml@3d3aba7995a5f4749a1a131204445f6cb438b00a$/,
  );
  assert.equal(availability.with.owned_only, true);
  assert.equal(availability.with.require_measured, true);
  assert.equal(availability.with.linux_probe_runner_tag, "harn-ci-big");
  assert.deepEqual(JSON.parse(availability.with.minimum_online_by_pool), {
    linux_big: 1,
  });
  assert.deepEqual(ship.needs, ["availability"]);
  assert.equal(ship.with.repair, false);
  assert.match(
    ship.uses,
    /^burin-labs\/\.github\/\.github\/workflows\/smart-ship\.yml@6958845300760fb5923b7da8d3e57f9df655558f$/,
  );
  assert.deepEqual(
    JSON.parse(evaluate(ship.with["runs-on"], {}).coerceString()),
    ["self-hosted", "Linux", "X64", "harn-ci-big"],
  );
});

test("the shipped merger predicate requires successful measured eligible big capacity", () => {
  const context = {
    needs: {
      availability: {
        result: "success",
        outputs: {
          capacity: JSON.stringify({
            linux_big: { measured: true, eligible: true },
          }),
        },
      },
    },
  };
  assert.equal(evaluate(ship.if, context).coerceString(), "true");
  for (const availability of [
    { result: "failure", outputs: context.needs.availability.outputs },
    {
      result: "success",
      outputs: {
        capacity: JSON.stringify({
          linux_big: { measured: false, eligible: true },
        }),
      },
    },
    {
      result: "success",
      outputs: {
        capacity: JSON.stringify({
          linux_big: { measured: true, eligible: false },
        }),
      },
    },
  ]) {
    assert.equal(
      evaluate(ship.if, { needs: { availability } }).coerceString(),
      "false",
    );
  }
});
