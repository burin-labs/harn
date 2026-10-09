const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const assert = require("node:assert/strict");
const { execFileSync, spawnSync } = require("node:child_process");
const root = path.resolve(__dirname, "../..");
const owner = require("../ci/rust_cache_generation.cjs");
const scratch = fs.mkdtempSync(
  path.join(os.tmpdir(), "cache-generation-retirement-"),
);
const repository = path.join(scratch, "repository");
const origin = path.join(scratch, "origin.git");
const key =
  "v0-rust-harn-ci-cli-workspace-crates-v3-Linux-x64-2af81dbb-ac61bc82";
const prefix = key.slice(0, -9);
const current = {
  id: 2,
  key,
  ref: "refs/heads/main",
  size_in_bytes: 1773390835,
  created_at: "2026-10-09T12:53:35Z",
};
const old = {
  id: 1,
  key: `${prefix}-6d94302f`,
  ref: "refs/heads/main",
  size_in_bytes: 1773508120,
  created_at: "2026-10-09T12:24:34Z",
};
const otherEnvironment = {
  ...old,
  id: 3,
  key: "v0-rust-harn-ci-cli-workspace-crates-v3-Linux-x64-ffffffff-6d94302f",
};
const gitConfig = path.join(scratch, "empty-gitconfig");
fs.writeFileSync(gitConfig, "");
const gitEnv = {
  ...process.env,
  GIT_CONFIG_GLOBAL: gitConfig,
  GIT_CONFIG_NOSYSTEM: "1",
};
for (const name of [
  "GIT_AUTHOR_NAME",
  "GIT_AUTHOR_EMAIL",
  "GIT_COMMITTER_NAME",
  "GIT_COMMITTER_EMAIL",
])
  delete gitEnv[name];
const runGit = (args) =>
  execFileSync("git", ["-c", "user.useConfigOnly=true", ...args], {
    cwd: repository,
    env: gitEnv,
    encoding: "utf8",
    stdio: ["pipe", "pipe", "pipe"],
  }).trim();
try {
  const startupRoot = fs.mkdtempSync(path.join(scratch, "startup-"));
  const startupMarker = path.join(scratch, "startup-fetch-fired");
  const startup = spawnSync(
    process.execPath,
    [
      "-e",
      `
    const fs=require('node:fs');
    global.fetch=async url=>{
      if(url!==${JSON.stringify(`https://raw.githubusercontent.com/Swatinem/rust-cache/${owner.pin.commit}/dist/restore/index.js`)}) throw Error('unexpected fetch');
      fs.writeFileSync(${JSON.stringify(startupMarker)}, 'expected pinned fetch reached');
      throw Error('probe pinned download unavailable');
    };
    require(${JSON.stringify(path.join(root, "scripts/ci/rust_cache_generation.cjs"))}).start().catch(e=>{console.error(e.message);process.exitCode=1});
  `,
    ],
    {
      encoding: "utf8",
      env: {
        ...process.env,
        GITHUB_REPOSITORY: "burin-labs/harn",
        GITHUB_REF: "refs/heads/main",
        RUNNER_TEMP: startupRoot,
        "INPUT_SHARED-KEY": "harn-ci-cli-workspace-crates-v3",
        "INPUT_SAVE-IF": "true",
        "INPUT_CACHE-WORKSPACE-CRATES": "true",
      },
    },
  );
  assert.equal(startup.status, 1);
  assert.match(startup.stderr, /probe pinned download unavailable/);
  assert.equal(
    fs.readFileSync(startupMarker, "utf8"),
    "expected pinned fetch reached",
  );
  assert.deepEqual(
    fs.readdirSync(startupRoot),
    [],
    "failed startup must remove its allocated scratch before post registration",
  );
  console.log(
    "PASS actual startup: failed pinned fetch reached and scratch removed",
  );
  fs.mkdirSync(repository);
  execFileSync("git", ["init", "--bare", origin], {
    stdio: "pipe",
    env: gitEnv,
  });
  runGit(["init", "-b", "main"]);
  runGit(["config", "maintenance.auto", "false"]);
  fs.mkdirSync(path.join(repository, "scripts/ci"), { recursive: true });
  fs.mkdirSync(path.join(repository, ".github/actions/rust-cache"), {
    recursive: true,
  });
  for (const file of [
    "scripts/ci/reuse_workspace_crates.sh",
    "scripts/ci/rust_cache_generation.cjs",
    "scripts/prune_ci_cache_generations.sh",
    ".github/actions/rust-cache/pinned-config.json",
  ])
    fs.copyFileSync(path.join(root, file), path.join(repository, file));
  fs.writeFileSync(path.join(repository, "Cargo.toml"), "[workspace]\n");
  fs.writeFileSync(
    path.join(repository, ".gitignore"),
    ".harn-workspace-source/\n",
  );
  runGit(["add", "."]);
  runGit([
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "commit.gpgsign=false",
    "-c",
    "user.name=Cache lifecycle fixture",
    "-c",
    "user.email=cache-fixture@example.invalid",
    "commit",
    "-m",
    "Fixture source",
  ]);
  runGit(["remote", "add", "origin", origin]);
  runGit(["push", "origin", "main"]);
  const sha = runGit(["rev-parse", "HEAD"]);
  const bin = path.join(scratch, "bin");
  fs.mkdirSync(bin);
  fs.writeFileSync(
    path.join(bin, "gh"),
    `#!/usr/bin/env node
const fs=require('node:fs');
const args=process.argv.slice(2);
fs.appendFileSync(process.env.PROBE_CALLS,JSON.stringify(args)+'\\n');
if(args[0]==='api' && args.length===4 && args.includes('--paginate') && args.includes('--slurp') && args.includes('repos/burin-labs/harn/actions/caches?per_page=100')) {
  if(process.env.PROBE_HTTP_FAILURE==='true'){console.error('probe inventory unavailable');process.exit(23);}
  process.stdout.write(fs.readFileSync(process.env.PROBE_PAGES));
} else if(JSON.stringify(args)===JSON.stringify(['cache','delete','1','--repo','burin-labs/harn'])) {} else {console.error('unexpected gh call');process.exit(64);}
`,
    { mode: 0o755 },
  );
  const pages = (caches) => [
    { total_count: caches.length, actions_caches: caches },
  ];
  let passed = 0;
  function scenario(
    name,
    {
      initial = null,
      observation = { cache_hit: false, source_current: false },
      record = true,
      census = pages([old, current, otherEnvironment]),
      httpFailure = false,
      success = false,
      deleted = false,
      expectedError = null,
    } = {},
  ) {
    const stateRoot = fs.mkdtempSync(
      path.join(os.tmpdir(), "rust-cache-generation-"),
    );
    const calls = path.join(scratch, `${name}.calls`);
    const payload = path.join(scratch, `${name}.json`);
    fs.writeFileSync(payload, JSON.stringify(census));
    fs.writeFileSync(
      path.join(stateRoot, "restore-observation.json"),
      JSON.stringify(observation),
    );
    const sourceRoot = path.join(repository, ".harn-workspace-source");
    fs.rmSync(sourceRoot, { recursive: true, force: true });
    if (record) {
      fs.mkdirSync(sourceRoot);
      fs.writeFileSync(path.join(sourceRoot, "commit"), sha);
    }
    const context = {
      family: "harn-ci-cli",
      repository: "burin-labs/harn",
      ref: "refs/heads/main",
      source_sha: sha,
      pinned_action: owner.pin.commit,
      cache_key: key,
      restore_key: prefix,
      initial_cache_id: initial,
      scratch: stateRoot,
    };
    const child = spawnSync(
      process.execPath,
      [
        "-e",
        `require(${JSON.stringify(path.join(root, "scripts/ci/rust_cache_generation.cjs"))}).finish().catch(e=>{console.error(e.message);process.exitCode=1})`,
      ],
      {
        cwd: repository,
        encoding: "utf8",
        env: {
          ...process.env,
          PATH: `${bin}:${process.env.PATH}`,
          GITHUB_REPOSITORY: "burin-labs/harn",
          GITHUB_REF: "refs/heads/main",
          GITHUB_SHA: sha,
          STATE_generation: JSON.stringify(context),
          PROBE_CALLS: calls,
          PROBE_PAGES: payload,
          PROBE_HTTP_FAILURE: String(httpFailure),
        },
      },
    );
    assert.equal(child.status === 0, success, `${name}: ${child.stderr}`);
    if (expectedError !== null) assert.match(child.stderr, expectedError, name);
    const observations = fs.existsSync(calls)
      ? fs
          .readFileSync(calls, "utf8")
          .trim()
          .split("\n")
          .filter(Boolean)
          .map(JSON.parse)
      : [];
    const writes = observations.filter((a) => a[0] === "cache");
    assert.deepEqual(
      writes,
      deleted ? [["cache", "delete", "1", "--repo", "burin-labs/harn"]] : [],
      name,
    );
    if (success) {
      const result = JSON.parse(child.stdout.trim());
      assert.equal(result.qualified.cache_id, 2);
      assert.equal(result.observed, census[0].total_count);
    }
    assert.equal(
      fs.existsSync(stateRoot),
      false,
      `${name}: temporary action state not retired`,
    );
    console.log(`PASS actual post lifecycle: ${name}`);
    passed++;
  }
  scenario("new-upload", { success: true, deleted: true });
  scenario("measured-exact-restore", {
    initial: 2,
    observation: { cache_hit: true, source_current: true },
    record: false,
    success: true,
    deleted: true,
  });
  scenario("sole-qualified-generation", {
    census: pages([current]),
    success: true,
  });
  scenario("upload-absent", { census: pages([old]) });
  scenario("partial-upload-zero-size", {
    census: pages([old, { ...current, size_in_bytes: 0 }]),
  });
  scenario("source-record-absent", { record: false });
  scenario("immutable-old-entry-cannot-be-relabeled", { initial: 2 });
  scenario("exact-restore-source-unmeasured", {
    initial: 2,
    observation: { cache_hit: true, source_current: false },
  });
  scenario("prefix-restore-is-not-exact", {
    initial: 2,
    observation: { cache_hit: false, source_current: true },
  });
  scenario("duplicate-exact-entry", {
    census: pages([old, current, { ...current, id: 4 }]),
  });
  scenario("partial-census", {
    census: [{ total_count: 4, actions_caches: [old, current] }],
  });
  scenario("empty-unreported-census", { census: [] });
  scenario("malformed-row", {
    census: pages([old, { ...current, size_in_bytes: "unknown" }]),
  });
  scenario("http-failure", { httpFailure: true });
  scenario("newer-generation-not-superseded", {
    census: pages([{ ...old, created_at: "2026-10-09T13:53:35Z" }, current]),
  });
  fs.writeFileSync(path.join(repository, "new-main"), "advance owning main\n");
  runGit(["add", "new-main"]);
  runGit([
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "commit.gpgsign=false",
    "-c",
    "user.name=Cache lifecycle fixture",
    "-c",
    "user.email=cache-fixture@example.invalid",
    "commit",
    "-m",
    "Advance main",
  ]);
  runGit(["push", "origin", "main"]);
  scenario("stale-source-after-main-advanced", {
    initial: current.id,
    observation: { cache_hit: true, source_current: true },
    expectedError: /Owning main advanced; cache qualification is unmeasured/,
  });
  const qualified = {
    schema: "harn.qualified_rust_cache_generation.v1",
    family: "harn-ci-cli",
    repository: "burin-labs/harn",
    ref: "refs/heads/main",
    source_sha: sha,
    pinned_action: owner.pin.commit,
    cache_key: key,
    restore_key: prefix,
    cache_id: 2,
    cache_bytes: current.size_in_bytes,
  };
  assert.throws(() =>
    owner.retirementPlan(
      { ...qualified, restore_key: prefix.replace("2af81dbb", "ffffffff") },
      [old, current],
    ),
  );
  assert.throws(() =>
    owner.retirementPlan({ ...qualified, pinned_action: "0".repeat(40) }, [
      old,
      current,
    ]),
  );
  assert.throws(() =>
    owner.cacheCensus([{ total_count: 2, actions_caches: [current, current] }]),
  );
  const bundleFile = process.env.HARN_TEST_PINNED_CACHE_BUNDLE;
  if (bundleFile) {
    const bundle = fs.readFileSync(bundleFile);
    assert.equal(
      owner
        .verifiedProjection(bundle)
        .includes("__webpack_exports__.resolveConfig"),
      true,
    );
    const bad = Buffer.from(bundle);
    bad[0] ^= 1;
    assert.throws(() => owner.verifiedProjection(bad));
    console.log("PASS actual pinned bundle and tampered-byte refusal");
  }
  console.log(
    `Cache generation lifecycle: ${passed} actual subprocess cases passed`,
  );
} finally {
  fs.rmSync(scratch, { recursive: true, force: true });
}
