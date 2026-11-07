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
const ciWorkflow = load(fs.readFileSync(`${root}/.github/workflows/ci.yml`, "utf8"));
const ship = workflow.jobs.ship;

test("routine producers and merging have no runner-capacity dependency", () => {
  assert.equal(ciWorkflow.jobs["runner-availability"], undefined);
  assert.equal(ciWorkflow.jobs["rust-producer-capacity"], undefined);
  assert.deepEqual(Object.keys(workflow.jobs), ["ship"]);
  assert.equal(ship.needs, undefined);
  assert.equal(JSON.parse(ship.with["runs-on"]), "ubuntu-24.04");
  for (const name of ["rust-cli", "rust-check-inputs"]) {
    const producer = ciWorkflow.jobs[name];
    assert.deepEqual(producer.needs, ["changes"]);
    assert.equal(evaluate(producer["runs-on"], {}).coerceString(), "ubuntu-24.04");
  }
});

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
    return evaluate(ship.if, context).coerceString() === "true";
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
  stopSwitch = "on",
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
    vars: { HARN_SMART_SHIP: stopSwitch },
    inputs: { pr: "" },
  };
}

test("privileged dispatchers refuse fork heads before forwarding secrets", () => {
  for (const [file, jobName] of [
    ["merge-override-dispatch.yml", "override"],
    ["review-dispatch.yml", "dispatch"],
  ]) {
    const candidate = load(
      fs.readFileSync(`${root}/.github/workflows/${file}`, "utf8"),
    ).jobs[jobName];
    const context = pullRequestContext({ changedLabel: "force-merge" });
    context.needs = { eligibility: { outputs: { dispatch: "true" } } };
    assert.equal(evaluate(candidate.if, context).coerceString(), "true", file);
    context.github.event.pull_request.head.repo.full_name = "contributor/harn";
    assert.equal(
      evaluate(candidate.if, context).coerceString(),
      "false",
      `${file} must refuse a fork even with an eligible label or upstream result`,
    );
  }
});

test("the shipped caller admits every same-repository main PR labeled ship", () => {
  assert.equal(admits(pullRequestContext()), true);
  assert.equal(
    admits(pullRequestContext({ number: 43 })),
    true,
    "every ship PR wakes the merger on its own events",
  );
  for (const [name, change] of [
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

test("the shipped caller refuses every event unless the stop switch is on", () => {
  // GitHub compares strings case-insensitively, so "ON" also admits.
  for (const stopSwitch of ["", "off", "true", "42"]) {
    assert.equal(
      admits(pullRequestContext({ stopSwitch })),
      false,
      `stop switch ${JSON.stringify(stopSwitch)} must refuse`,
    );
  }
  const unset = pullRequestContext();
  unset.vars = {};
  assert.equal(admits(unset), false, "an unset stop switch must refuse");
});

test("the shipped caller dispatches one canonical PR number from main", () => {
  const context = pullRequestContext({ eventName: "workflow_dispatch" });
  context.github.ref = "refs/heads/main";
  context.inputs.pr = "42";
  assert.equal(admits(context), true);
  assert.equal(admits({ ...context, inputs: { pr: "43" } }), true);
  // A blank dispatch would reach the shared hosted sweep.
  for (const pr of ["", "x", "0", "-42", "42.5", "042", "null", "[]"]) {
    assert.equal(
      admits({ ...context, inputs: { pr } }),
      false,
      `dispatch PR ${JSON.stringify(pr)} must be refused`,
    );
  }
  assert.equal(
    admits({ ...context, vars: { HARN_SMART_SHIP: "off" } }),
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

test("the shipped caller admits the actual owned pull request workflow run", () => {
  const context = pullRequestContext({
    eventName: "workflow_run",
    number: 10885,
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
    true,
    "every pull request CI completion wakes the merger",
  );
  assert.equal(
    admits({ ...context, vars: { HARN_SMART_SHIP: "off" } }),
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

test("every pull request workflow's completion wakes the merger", () => {
  // A ship PR whose last pending check comes from an unlisted workflow is never
  // looked at again once that check passes; harn#10995 waited on macOS tests.
  const dir = `${root}/.github/workflows`;
  const producers = fs
    .readdirSync(dir)
    .filter((file) => /\.ya?ml$/.test(file))
    .map((file) => load(fs.readFileSync(`${dir}/${file}`, "utf8")))
    .filter((wf) => {
      const on = wf?.on;
      if (typeof on === "string") return on === "pull_request";
      if (Array.isArray(on)) return on.includes("pull_request");
      return on != null && Object.hasOwn(on, "pull_request");
    })
    .map((wf) => wf.name)
    .sort();
  assert.ok(producers.includes("macOS workspace tests"));
  assert.deepEqual(
    [...workflow.on.workflow_run.workflows].sort(),
    producers,
    "the wake list must name exactly the workflows with a pull_request trigger",
  );
});

test("the shipped workflow sweeps every 30 minutes and exposes a one-PR dispatch", () => {
  assert.deepEqual(workflow.on.schedule, [{ cron: "19,49 * * * *" }]);
  const sweep = pullRequestContext({ eventName: "schedule" });
  sweep.github.event = {};
  assert.equal(admits(sweep), true, "the scheduled sweep is admitted");
  assert.equal(
    admits({ ...sweep, vars: { HARN_SMART_SHIP: "off" } }),
    false,
    "the stop switch refuses the sweep",
  );
  assert.equal(
    admits({
      ...sweep,
      github: { ...sweep.github, repository: "burin-labs/other" },
    }),
    false,
    "another repository's sweep is refused",
  );
  assert.equal(workflow.on.workflow_dispatch.inputs.pr.required, true);
  assert.equal(workflow.on.workflow_dispatch.inputs.pr.default, undefined);
  assert.equal(ship.with.repair, false);
  assert.match(
    ship.uses,
    /^burin-labs\/\.github\/\.github\/workflows\/smart-ship\.yml@[0-9a-f]{40}$/,
  );
  assert.equal(JSON.parse(ship.with["runs-on"]), "ubuntu-24.04");
});
