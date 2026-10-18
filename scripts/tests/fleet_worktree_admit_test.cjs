const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const { cpSync, mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } = require("node:fs");
const { tmpdir } = require("node:os");
const { join, resolve } = require("node:path");
const { test } = require("node:test");

const entry = resolve(__dirname, "../fleet-worktree-admit.ts");
function fixture(t) {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), "harn admission spaces ")));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const primary = join(dir, "harn primary");
  const owner = join(dir, "host repo", "scripts");
  mkdirSync(join(primary, "scripts"), { recursive: true });
  mkdirSync(owner, { recursive: true });
  cpSync(entry, join(primary, "scripts/fleet-worktree-admit.ts"));
  const git = (...args) => {
    const result = spawnSync("git", ["-C", primary, ...args], { encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
  };
  git("init", "--initial-branch=main");
  git("config", "--local", "harn.fleetAdmissionOwner", join(owner, "fleet-worktree-admit.ts"));
  return { dir, primary, owner: join(owner, "fleet-worktree-admit.ts"), git };
}
function invoke(root, args = []) {
  return spawnSync(process.execPath, ["--experimental-strip-types", "--no-warnings", join(root, "scripts/fleet-worktree-admit.ts"), ...args], { encoding: "utf8" });
}

test("forwards exact arguments and the managed checkout using common local configuration", (t) => {
  const f = fixture(t);
  f.git("add", "scripts");
  f.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", "commit", "-m", "fixture");
  const candidate = join(f.dir, "managed candidate spaces");
  f.git("worktree", "add", "-b", "candidate", candidate);
  writeFileSync(f.owner, "console.log(JSON.stringify(process.argv.slice(2))); console.error('owner diagnostic'); process.exit(7);\n");
  const args = ["--issue-url", "https://github.com/acme/harn/issues/1", "--worktree-path", join(f.dir, "next lane spaces")];
  const result = invoke(candidate, args);
  assert.equal(result.status, 7);
  assert.deepEqual(JSON.parse(result.stdout), ["--repository-root", candidate, ...args]);
  assert.equal(result.stderr, "owner diagnostic\n");
});

test("refuses a missing canonical owner", (t) => {
  const f = fixture(t);
  const result = invoke(f.primary);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Canonical fleet admission owner is missing/);
  assert.ok(result.stderr.includes(f.owner));
  assert.equal(result.stdout, "");
});

test("refuses unconfigured and relative owner paths", (t) => {
  const f = fixture(t);
  f.git("config", "--local", "--unset", "harn.fleetAdmissionOwner");
  const unconfigured = invoke(f.primary);
  assert.equal(unconfigured.status, 1);
  assert.match(unconfigured.stderr, /Configure harn.fleetAdmissionOwner/);
  assert.equal(unconfigured.stdout, "");
  f.git("config", "--local", "harn.fleetAdmissionOwner", "relative/owner.ts");
  const relative = invoke(f.primary);
  assert.equal(relative.status, 1);
  assert.match(relative.stderr, /not absolute/);
  assert.equal(relative.stdout, "");
});

test("refuses every caller repository-root override before invoking the owner", (t) => {
  const f = fixture(t);
  writeFileSync(f.owner, "throw new Error('must not invoke owner');\n");
  for (const args of [["--repository-root", f.primary], ["--repository-root=/different"]]) {
    const result = invoke(f.primary, args);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /This entry owns --repository-root/);
    assert.doesNotMatch(result.stderr, /must not invoke owner/);
    assert.equal(result.stdout, "");
  }
});
