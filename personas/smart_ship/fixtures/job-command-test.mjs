import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  randomBytes,
  createPrivateKey,
  createPublicKey,
  verify,
} from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";

const [binary, command, binding, root] = process.argv.slice(2);
mkdirSync(root, { recursive: true });
const harn =
  binary.includes("/") || binary.includes("\\") ? resolve(binary) : binary;
const seed = randomBytes(32);
const seedText = seed.toString("hex");
const key = createPrivateKey({
  key: Buffer.concat([
    Buffer.from("302e020100300506032b657004220420", "hex"),
    seed,
  ]),
  format: "der",
  type: "pkcs8",
});
seed.fill(0);
const publicKey = createPublicKey(key);
const env = {
  PATH: process.env.PATH,
  HOME: process.env.HOME,
  SMART_SHIP_PARENT_ONLY: "must-not-enter-the-job",
};
if (process.env.SYSTEMROOT) env.SYSTEMROOT = process.env.SYSTEMROOT;
const run = (args, input) =>
  spawnSync(harn, ["run", "--write-root", root, ...args], {
    cwd: root,
    env,
    input,
    encoding: "utf8",
    timeout: 30000,
    maxBuffer: 1048576,
  });
const file = (name, value) => {
  const path = join(root, name);
  writeFileSync(path, JSON.stringify(value));
  return path;
};
const git = (...args) => {
  const result = spawnSync(
    "git",
    [
      "-c",
      "user.name=Smart Ship Fixture",
      "-c",
      "user.email=fixture@example.invalid",
      "-c",
      "commit.gpgsign=false",
      "-c",
      "core.hooksPath=.git/no-hooks",
      ...args,
    ],
    { cwd: root, env, encoding: "utf8" },
  );
  assert.equal(result.status, 0, "fixture Git failed");
};
const passed = (result) => {
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout);
};
writeFileSync(join(root, ".gitignore"), "*.json\n*.xml\n");
writeFileSync(join(root, "expectation.mjs"), "export const expected = true;\n");
writeFileSync(
  join(root, "test.mjs"),
  `import test from 'node:test';
import assert from 'node:assert/strict';
import { expected } from './expectation.mjs';
test('job has only its declared environment', () => {
  assert.equal(process.env.SMART_SHIP_PARENT_ONLY, undefined);
  assert.equal(process.env.SMART_SHIP_JOB_VALUE, 'declared');
  console.log(JSON.stringify(process.env));
});
test('actual project expectation', () => assert.equal(expected, true));
`,
);
git("init", "--quiet");
git("add", "--all");
git("commit", "--quiet", "-m", "ordinary stock command fixture");
const jobEnv = { PATH: process.env.PATH, SMART_SHIP_JOB_VALUE: "declared" };
if (process.env.SYSTEMROOT) jobEnv.SYSTEMROOT = process.env.SYSTEMROOT;
const job = {
  installation: "neutral",
  id: "tests",
  repository: "project",
  workspace: root,
  command: {
    kind: "tests",
    program: process.execPath,
    args: [
      "--test",
      "--test-reporter=junit",
      "--test-reporter-destination=.ship-report.xml",
      "test.mjs",
    ],
    version_args: ["--version"],
    timeout_ms: 10000,
    environment: jobEnv,
    test_report: ".ship-report.xml",
  },
  outputs: [],
  trust_domain: "local",
  run: "ordinary-ci-run",
  attempt: 2,
};
const prepare = (value = job) =>
  passed(
    run([
      command,
      "--",
      file("prepare.json", {
        action: "prepare",
        job: value,
      }),
    ]),
  );
const dispatch = (plan, mode = "snapshot") =>
  passed(run([binding, "--", mode, file("plan.json", plan)]));
const execute = (plan, frozen, secret = "fixture/executor") => {
  const value = Buffer.from(seedText);
  const frame = Buffer.from(
    JSON.stringify({
      version: 1,
      secrets: [
        {
          id: { namespace: "fixture", name: "executor" },
          value: Array.from(value),
        },
      ],
    }),
  );
  value.fill(0);
  const header = Buffer.alloc(4);
  header.writeUInt32BE(frame.length);
  const input = Buffer.concat([header, frame]);
  frame.fill(0);
  const result = run(
    [
      "--parent-secret-stdin",
      command,
      "--",
      file("execute.json", {
        action: "execute",
        plan,
        dispatch: frozen,
        signer: { id: "executor", secret },
      }),
    ],
    input,
  );
  input.fill(0);
  assert(
    !result.stdout.includes(seedText) && !result.stderr.includes(seedText),
    "executor key entered public output",
  );
  return result;
};
const plan = prepare();
assert.equal(plan.requirement.execution_request, "");
for (const kind of ["snapshot", "admission"]) {
  const frozen = dispatch(plan, kind);
  const report = passed(execute(plan, frozen));
  const original = report.proof.original;
  assert.equal(original.result, "passed");
  assert.equal(original.tests.total, 2);
  assert.equal(original.tests.passed, 2);
  assert.equal(original.attempt, 2);
  assert.equal(original.run, "ordinary-ci-run");
  assert.deepEqual(original.source, plan.source);
  assert.match(original.requirement.execution_request, /^sha256:[0-9a-f]{64}$/);
  assert(!report.stdout.includes("must-not-enter-the-job"));
  const bytes = run([
    binding,
    "--",
    "execution-bytes",
    file("execution.json", original),
  ]);
  assert.equal(bytes.status, 0, bytes.stderr);
  const message = Buffer.from(bytes.stdout.replace(/\r?\n$/, ""));
  assert(
    verify(
      null,
      message,
      publicKey,
      Buffer.from(report.proof.signature, "hex"),
    ),
  );
  assert(
    !verify(
      null,
      Buffer.concat([message, Buffer.from("altered")]),
      publicKey,
      Buffer.from(report.proof.signature, "hex"),
    ),
  );
}
const wrong = execute(plan, dispatch(plan, "wrong-source"), "fixture/absent");
assert.notEqual(wrong.status, 0);
assert.match(wrong.stderr, /prepared job differs/);
writeFileSync(
  join(root, "expectation.mjs"),
  "export const expected = false;\n",
);
const changed = execute(plan, dispatch(plan));
assert.notEqual(changed.status, 0);
assert.match(changed.stderr, /source|checkout/);
git("add", "expectation.mjs");
git("commit", "--quiet", "-m", "reach a real failing project test");
const failedPlan = prepare();
const failed = execute(failedPlan, dispatch(failedPlan));
assert.notEqual(failed.status, 0);
const failure = JSON.parse(failed.stdout);
assert.equal(failure.proof.original.result, "failed");
assert.equal(failure.proof.original.tests.total, 2);
assert.equal(failure.proof.original.tests.failed, 1);
assert.equal(failure.proof.original.tests.passed, 1);
assert.match(
  readFileSync(join(root, ".ship-report.xml"), "utf8"),
  /actual project expectation/,
);
const absentPlan = prepare({
  ...job,
  command: { ...job.command, test_report: ".absent.xml" },
});
const absent = execute(absentPlan, dispatch(absentPlan));
assert.notEqual(absent.status, 0);
assert.notEqual(JSON.parse(absent.stdout).proof.original.result, "passed");
assert.equal(JSON.parse(absent.stdout).proof.original.tests, null);
assert(!readFileSync(join(root, "execute.json"), "utf8").includes(seedText));
console.log(
  JSON.stringify({
    status: "passed",
    dispatchKinds: 2,
    observedPassingTests: 2,
    observedFailingTests: 1,
    modifiedSourceRefused: true,
    mismatchedDispatchRefused: true,
    absentReportRefused: true,
    signerOutsideChildAndPublicOutput: true,
    signatureAndTamperControlsPassed: true,
    liveGithubExecution: false,
  }),
);
