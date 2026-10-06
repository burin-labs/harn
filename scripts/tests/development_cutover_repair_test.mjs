import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const root = dirname(dirname(dirname(fileURLToPath(import.meta.url))));
const policy = JSON.parse(
  readFileSync(join(root, "scripts/development_cutover_repair.json"), "utf8"),
);
const workflow = readFileSync(
  join(root, ".github/workflows/repository-state-reconciliation.yml"),
  "utf8",
);
const command = workflow.match(
  /name: Reserve and dispatch one owed development repair[\s\S]*?\n\s+run: (.+)/,
)?.[1];
assert.equal(command, "node scripts/repair_development_cutover.mjs");

function fixture(t, overrides = {}) {
  const directory = mkdtempSync(join(tmpdir(), "harn-repair-test-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const workspace = join(directory, "workspace");
  const bin = join(directory, "bin");
  mkdirSync(workspace);
  mkdirSync(bin);
  writeFileSync(
    join(workspace, "Cargo.toml"),
    '[workspace.package]\nversion = "1.2.3"\n',
  );
  const git = (...args) =>
    execFileSync("git", args, { cwd: workspace, stdio: "pipe" });
  git("init", "-b", "main");
  git("config", "user.name", "Cutover fixture");
  git("config", "user.email", "cutover@example.invalid");
  git("config", "commit.gpgsign", "false");
  git("config", "tag.gpgSign", "false");
  git("add", "Cargo.toml");
  git("commit", "-m", "Release fixture");
  git("tag", "v1.2.3");
  writeFileSync(
    join(bin, "gh"),
    '#!/bin/sh\nexec "$REPAIR_TEST_NODE" "$REPAIR_TEST_GH_FIXTURE" "$@"\n',
    { mode: 0o700 },
  );
  const stateFile = join(directory, "state.json");
  const initial = {
    publication: {
      tagName: "v1.2.3",
      isDraft: false,
      isPrerelease: false,
      publishedAt: "2026-10-03T12:00:00Z",
    },
    pullRequests: [],
    dispatches: 0,
    requests: [],
    nextId: policy.seed.id + 1,
    statuses: [{ ...policy.seed, state: "success" }],
    ...overrides,
  };
  writeFileSync(stateFile, JSON.stringify(initial));
  const read = () => JSON.parse(readFileSync(stateFile, "utf8"));
  const change = (update) =>
    writeFileSync(stateFile, JSON.stringify({ ...read(), ...update }));
  const run = () =>
    spawnSync("bash", ["-c", command], {
      cwd: root,
      encoding: "utf8",
      env: {
        ...process.env,
        PATH: `${bin}:${process.env.PATH}`,
        GH_TOKEN: "fixture",
        GH_REPO: policy.repository,
        GITHUB_RUN_ID: "70001",
        MEASURED_REPAIR_OWED: "true",
        MEASURED_DEVELOPMENT_VERSION: "1.2.4-dev",
        MEASURED_PUBLISHED_TAG: "v1.2.3",
        HARN_RELEASE_ROOT: workspace,
        REPAIR_TEST_STATE: stateFile,
        REPAIR_TEST_NODE: process.execPath,
        REPAIR_TEST_GH_FIXTURE: join(
          root,
          "scripts/tests/fixtures/development_cutover_gh.mjs",
        ),
        REPAIR_TEST_POLICY: join(
          root,
          "scripts/development_cutover_repair.json",
        ),
      },
      timeout: 30_000,
    });
  return { run, read, change, git, workspace };
}

test("owning workflow command dispatches once and preserves the fixed fence after tag movement", (t) => {
  const f = fixture(t);
  const first = f.run();
  assert.equal(first.status, 0, first.stderr);
  assert.equal(f.read().dispatches, 1);
  assert.equal(JSON.parse(first.stdout).state, "dispatched");
  writeFileSync(join(f.workspace, "README"), "A later source commit\n");
  f.git("add", "README");
  f.git("commit", "-m", "Move release fixture");
  f.git("tag", "-f", "v1.2.3");
  const second = f.run();
  assert.equal(second.status, 0, second.stderr);
  assert.equal(JSON.parse(second.stdout).state, "reserved");
  assert.equal(f.read().dispatches, 1);
  assert.equal(f.read().requests.filter((r) => r.method === "POST").length, 2);
  assert(
    f
      .read()
      .requests.filter((r) => r.endpoint.includes("/commits/"))
      .every((r) => r.endpoint.includes(policy.anchor)),
  );
});

test("crash after durable reservation cannot dispatch on retry", (t) => {
  const f = fixture(t, { crashAfterReservation: true });
  assert.equal(f.run().status, 1);
  f.change({ crashAfterReservation: false });
  const retry = f.run();
  assert.equal(retry.status, 0, retry.stderr);
  assert.equal(JSON.parse(retry.stdout).state, "reserved");
  assert.equal(f.read().dispatches, 0);
  assert.equal(f.read().requests.filter((r) => r.method === "POST").length, 1);
});

test("ambiguous dispatch acknowledgment consumes the attempt", (t) => {
  const f = fixture(t, { dispatchUnknownAck: true });
  assert.equal(f.run().status, 1);
  assert.equal(f.read().dispatches, 1);
  assert.equal(f.run().status, 0);
  assert.equal(f.read().dispatches, 1);
});

for (const control of ["partial", "seedMissing", "anchorMissing"]) {
  test(`${control} refuses before any status or workflow mutation`, (t) => {
    const f = fixture(t, { [control]: true });
    assert.equal(f.run().status, 1);
    assert.equal(
      f.read().requests.filter((r) => r.method === "POST").length,
      0,
    );
    assert.equal(f.read().dispatches, 0);
  });
}

test("existing red development PR suppresses automatic repair", (t) => {
  const f = fixture(t, {
    pullRequests: [
      {
        number: 42,
        state: "open",
        head: { ref: "automation/development-1.2.4-dev" },
        checks: "failure",
      },
    ],
  });
  const result = f.run();
  assert.equal(result.status, 0, result.stderr);
  assert.equal(JSON.parse(result.stdout).state, "existing_pr");
  assert.equal(f.read().requests.filter((r) => r.method === "POST").length, 0);
});

test("reservation on a later status page suppresses dispatch", (t) => {
  const statuses = [
    { ...policy.seed, state: "success" },
    ...Array.from({ length: 99 }, (_, i) => ({
      id: i + 1,
      context: `fixture/${i}`,
      state: "success",
    })),
    {
      id: policy.seed.id + 2,
      context: `${policy.context_prefix}1.2.4-dev`,
      state: "failure",
    },
  ];
  const f = fixture(t, { statuses });
  const result = f.run();
  assert.equal(result.status, 0, result.stderr);
  assert.equal(JSON.parse(result.stdout).state, "reserved");
  assert.equal(JSON.parse(result.stdout).measuredContexts, 101);
  assert.equal(f.read().requests.filter((r) => r.method === "POST").length, 0);
  assert(f.read().requests.some((r) => r.endpoint.endsWith("page=2")));
});

test("missing publication proof refuses before reservation", (t) => {
  const f = fixture(t, { publication: { tagName: "v1.2.3", isDraft: false } });
  assert.equal(f.run().status, 1);
  assert.equal(f.read().requests.filter((r) => r.method === "POST").length, 0);
});

test("controller serializes schedule and manual repair while preserving merge-group cancellation", () => {
  assert(
    workflow.includes(
      "github.event_name == 'merge_group' && 'merge_group' || 'cutover'",
    ),
  );
  assert(
    workflow.includes(
      "cancel-in-progress: ${{ github.event_name == 'merge_group' }}",
    ),
  );
  assert(workflow.includes("if: steps.cutover.outputs.repair_owed == 'true'"));
});
