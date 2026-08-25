import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";
import test from "node:test";
import {
  RECEIPT_SCHEMA,
  archivePathForTar,
  assetForTarget,
  bootstrap,
  ensureVerifiedArchive,
  extractionCommand,
  fetchWithRetry,
  installMulticallAliases,
  main,
  normalizeVersion,
  parseChecksums,
  parseCliArgs,
  releaseTarget,
  resolveBootstrap,
  resolveVersion,
  sha256,
} from "../bootstrap_harn.mjs";

const TEST_VERSION = "1.2.3";
const TEST_TARGET = "x86_64-unknown-linux-gnu";
const TEST_ASSET = assetForTarget(TEST_TARGET);

function temporaryRoot(context) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "harn-bootstrap-test-"));
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  return root;
}

function makeArchive(root, contents = "fake harn binary\n") {
  const source = path.join(root, `archive-source-${crypto.randomUUID()}`);
  fs.mkdirSync(source);
  fs.writeFileSync(path.join(source, "harn"), contents);
  const archive = path.join(root, `harn-${crypto.randomUUID()}.tar.gz`);
  const created = spawnSync("tar", ["-czf", archive, "-C", source, "harn"], {
    encoding: "utf8",
  });
  assert.equal(created.status, 0, created.stderr || created.stdout);
  return fs.readFileSync(archive);
}

function fakeRelease(archiveBytes, options = {}) {
  const checksum = options.checksum ?? sha256(archiveBytes);
  const manifest = options.manifest ?? `${checksum}  ${TEST_ASSET}\n`;
  const calls = { metadata: 0, archive: 0 };
  return {
    calls,
    fetchImpl: async (url) => {
      if (url.endsWith("/SHA256SUMS")) {
        calls.metadata += 1;
        return new Response(manifest);
      }
      if (url.endsWith(`/${TEST_ASSET}`)) {
        calls.archive += 1;
        return new Response(archiveBytes);
      }
      return new Response("missing", { status: 404 });
    },
  };
}

function bootstrapOptions(root, fetchImpl) {
  return {
    version: TEST_VERSION,
    target: TEST_TARGET,
    cacheDir: path.join(root, "cache"),
    installDir: path.join(root, "install"),
    attempts: 1,
    delayMs: 0,
    fetchImpl,
  };
}

function writeInstallLock(root, owner) {
  const lockRoot = path.join(root, "install.lock");
  fs.mkdirSync(lockRoot);
  fs.writeFileSync(
    path.join(lockRoot, "owner.json"),
    `${JSON.stringify({
      schema_version: "harn-bootstrap-install-lock-v1",
      token: crypto.randomUUID(),
      ...owner,
    })}\n`,
  );
  return lockRoot;
}

function replaceInstallLockOwner(lockRoot, owner) {
  const ownerPath = path.join(lockRoot, "owner.json");
  const temporary = path.join(lockRoot, `owner-${crypto.randomUUID()}.tmp`);
  fs.writeFileSync(
    temporary,
    `${JSON.stringify({
      schema_version: "harn-bootstrap-install-lock-v1",
      token: crypto.randomUUID(),
      ...owner,
    })}\n`,
  );
  fs.renameSync(temporary, ownerPath);
}

function publishValidInstall(root, archive) {
  const installRoot = path.join(root, "install");
  const binaryPath = path.join(installRoot, "harn");
  const binary = Buffer.from("fake harn binary\n");
  fs.mkdirSync(installRoot);
  fs.writeFileSync(binaryPath, binary);
  fs.writeFileSync(
    path.join(installRoot, "install-manifest.json"),
    `${JSON.stringify({
      schema_version: "harn-bootstrap-install-v1",
      version: TEST_VERSION,
      target: TEST_TARGET,
      binary_path: binaryPath,
      source: `https://github.com/burin-labs/harn/releases/download/v${TEST_VERSION}/${TEST_ASSET}`,
      checksum: sha256(archive),
      binary_sha256: sha256(binary),
    })}\n`,
  );
}

function runChild(script, arguments_) {
  return new Promise((resolve, reject) => {
    const child = spawn(
      process.execPath,
      ["--input-type=module", "-e", script, ...arguments_],
      { stdio: ["ignore", "pipe", "pipe"] },
    );
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.on("error", reject);
    child.on("close", (code) => {
      if (code === 0) resolve(stdout);
      else reject(new Error(`child exited ${code}: ${stderr}`));
    });
  });
}

test("archive extraction never passes drive-qualified paths to tar", () => {
  assert.equal(
    archivePathForTar(
      String.raw`C:\hostedtoolcache\windows\harn\0.10.28\.install-123`,
      String.raw`C:\hostedtoolcache\windows\harn-setup\downloads\harn.zip`,
      path.win32,
    ),
    "../../../harn-setup/downloads/harn.zip",
  );
});

test("archive extraction follows the published archive format", () => {
  const windows = extractionCommand(
    String.raw`C:\cache\harn.zip`,
    String.raw`C:\cache\install`,
    "x86_64-pc-windows-msvc",
  );
  assert.equal(windows.command, "powershell.exe");
  assert.deepEqual(windows.env, {
    HARN_BOOTSTRAP_ARCHIVE_PATH: String.raw`C:\cache\harn.zip`,
    HARN_BOOTSTRAP_EXTRACT_PATH: String.raw`C:\cache\install`,
  });

  const linux = extractionCommand(
    "/cache/downloads/harn.tar.gz",
    "/cache/install",
    TEST_TARGET,
  );
  assert.deepEqual(linux, {
    command: "tar",
    args: ["-xf", "../downloads/harn.tar.gz"],
  });
});

test("Windows command aliases share one installed binary", (context) => {
  const root = temporaryRoot(context);
  const binaryPath = path.join(root, "harn.exe");
  const aliases = ["harn-lsp.exe", "harn-dap.exe"].map((name) =>
    path.join(root, name),
  );
  fs.writeFileSync(binaryPath, "new binary\n");
  for (const alias of aliases) fs.writeFileSync(alias, "old archive copy\n");

  assert.deepEqual(
    installMulticallAliases(binaryPath, "x86_64-pc-windows-msvc"),
    aliases,
  );

  const binary = fs.statSync(binaryPath);
  for (const alias of aliases) {
    const linked = fs.statSync(alias);
    assert.equal(linked.dev, binary.dev);
    assert.equal(linked.ino, binary.ino);
    assert.equal(fs.readFileSync(alias, "utf8"), "new binary\n");
  }
  assert.equal(binary.nlink, 3);
});

test("versions are exact, canonical, and file-derived", (context) => {
  const root = temporaryRoot(context);
  const versionFile = path.join(root, ".harn-version");
  fs.writeFileSync(versionFile, "v1.2.3\n");
  assert.equal(normalizeVersion(" v1.2.3\n"), TEST_VERSION);
  assert.equal(resolveVersion({ versionFile }), TEST_VERSION);
  assert.equal(resolveVersion({ version: TEST_VERSION }), TEST_VERSION);
  assert.throws(
    () => resolveVersion({ version: TEST_VERSION, versionFile }),
    /mutually exclusive/,
  );
  assert.throws(() => resolveVersion({}), /exact --version or --version-file/);
  for (const invalid of [
    "latest",
    "1.2",
    "1.2.3/../../x",
    "01.2.3",
    "1.2.3-beta",
  ]) {
    assert.throws(() => normalizeVersion(invalid));
  }
});

test("host platforms map only to published assets", () => {
  assert.equal(
    assetForTarget(releaseTarget("Linux", "X64")),
    "harn-x86_64-unknown-linux-gnu.tar.gz",
  );
  assert.equal(
    assetForTarget(releaseTarget("Linux", "ARM64")),
    "harn-aarch64-unknown-linux-gnu.tar.gz",
  );
  assert.equal(
    assetForTarget(releaseTarget("macOS", "ARM64")),
    "harn-aarch64-apple-darwin.tar.gz",
  );
  assert.equal(
    assetForTarget(releaseTarget("Windows", "X64")),
    "harn-x86_64-pc-windows-msvc.zip",
  );
  assert.equal(
    assetForTarget(releaseTarget("win32", "x64")),
    "harn-x86_64-pc-windows-msvc.zip",
  );
  assert.throws(() => releaseTarget("Windows", "ARM64"));
  assert.throws(() => assetForTarget("../../unexpected"), /unsupported/);
});

test("checksum manifests fail closed on malformed or duplicate metadata", () => {
  const a = "a".repeat(64);
  const b = "b".repeat(64);
  const parsed = parseChecksums(
    `${a}  harn-linux.tar.gz\n${b} *harn.exe.zip\n`,
  );
  assert.equal(parsed.get("harn-linux.tar.gz"), a);
  assert.equal(parsed.get("harn.exe.zip"), b);
  assert.throws(() => parseChecksums("noise"), /malformed/);
  assert.throws(
    () => parseChecksums(`${a}  harn.zip\n${b}  harn.zip\n`),
    /duplicate/,
  );
});

test("bounded release retries stop without wall-clock delays", async () => {
  let calls = 0;
  const bytes = await fetchWithRetry("https://example.invalid/asset", {
    attempts: 3,
    delayMs: 0,
    delay: async () => {},
    fetchImpl: async () => {
      calls += 1;
      if (calls < 3) return new Response("missing", { status: 404 });
      return new Response("ready", { status: 200 });
    },
  });
  assert.equal(bytes.toString(), "ready");
  assert.equal(calls, 3);
});

test("unpublished assets end in one closed bounded failure", async () => {
  let calls = 0;
  await assert.rejects(
    fetchWithRetry("https://example.invalid/missing", {
      attempts: 4,
      delayMs: 0,
      delay: async () => {},
      fetchImpl: async () => {
        calls += 1;
        return new Response("missing", { status: 404 });
      },
    }),
    /unavailable after 4 attempt\(s\).*HTTP 404/,
  );
  assert.equal(calls, 4);
});

test("verified cached archives are reused and corrupt candidates are replaced", async (context) => {
  const root = temporaryRoot(context);
  const archivePath = path.join(root, "asset.tar.gz");
  const expected = Buffer.from("expected archive");
  fs.writeFileSync(archivePath, expected);
  let downloads = 0;
  const cached = await ensureVerifiedArchive({
    archivePath,
    expectedChecksum: sha256(expected),
    sourceUrl: "https://example.invalid/asset",
    fetchImpl: async () => {
      downloads += 1;
      return new Response(expected);
    },
  });
  assert.equal(cached.cacheHit, true);
  assert.equal(downloads, 0);

  fs.writeFileSync(archivePath, "tampered");
  const repaired = await ensureVerifiedArchive({
    archivePath,
    expectedChecksum: sha256(expected),
    sourceUrl: "https://example.invalid/asset",
    fetchImpl: async () => {
      downloads += 1;
      return new Response(expected);
    },
  });
  assert.equal(repaired.cacheHit, false);
  assert.equal(downloads, 1);
  assert.deepEqual(fs.readFileSync(archivePath), expected);
});

test("checksum mismatch fails closed and is never cached", async (context) => {
  const root = temporaryRoot(context);
  const archivePath = path.join(root, "asset.tar.gz");
  await assert.rejects(
    ensureVerifiedArchive({
      archivePath,
      expectedChecksum: sha256(Buffer.from("expected")),
      sourceUrl: "https://example.invalid/asset",
      fetchImpl: async () => new Response("different"),
    }),
    /checksum mismatch/,
  );
  assert.equal(fs.existsSync(archivePath), false);
});

test("release metadata without the selected asset fails closed", async (context) => {
  const root = temporaryRoot(context);
  const archive = makeArchive(root);
  const release = fakeRelease(archive, {
    manifest: `${sha256(archive)}  another-asset.tar.gz\n`,
  });
  await assert.rejects(
    bootstrap(bootstrapOptions(root, release.fetchImpl)),
    new RegExp(
      `SHA256SUMS does not contain ${TEST_ASSET.replaceAll(".", "\\.")}`,
    ),
  );
  assert.equal(release.calls.archive, 0);
});

test("bootstrap emits a stable receipt and reuses only verified bytes", async (context) => {
  const root = temporaryRoot(context);
  const archive = makeArchive(root);
  const release = fakeRelease(archive);
  const options = bootstrapOptions(root, release.fetchImpl);

  const first = await bootstrap(options);
  assert.deepEqual(
    {
      schema_version: first.schema_version,
      version: first.version,
      target: first.target,
      binary_path: first.binary_path,
      source: first.source,
      checksum: first.checksum,
      cache_hit: first.cache_hit,
    },
    {
      schema_version: RECEIPT_SCHEMA,
      version: TEST_VERSION,
      target: TEST_TARGET,
      binary_path: path.join(root, "install", "harn"),
      source: `https://github.com/burin-labs/harn/releases/download/v${TEST_VERSION}/${TEST_ASSET}`,
      checksum: sha256(archive),
      cache_hit: false,
    },
  );
  assert.equal(
    fs.readFileSync(first.binary_path, "utf8"),
    "fake harn binary\n",
  );

  const second = await bootstrap(options);
  assert.equal(second.cache_hit, true);
  assert.equal(release.calls.archive, 1);
  assert.equal(release.calls.metadata, 2);
});

test("corrupt archive and install bytes are repaired without trusting either cache", async (context) => {
  const root = temporaryRoot(context);
  const archive = makeArchive(root);
  const release = fakeRelease(archive);
  const options = bootstrapOptions(root, release.fetchImpl);
  const first = await bootstrap(options);
  fs.writeFileSync(
    path.join(root, "cache", "downloads", TEST_VERSION, TEST_ASSET),
    "corrupt archive",
  );
  fs.writeFileSync(first.binary_path, "corrupt binary");

  const repaired = await bootstrap(options);
  assert.equal(repaired.cache_hit, false);
  assert.equal(
    fs.readFileSync(repaired.binary_path, "utf8"),
    "fake harn binary\n",
  );
  assert.equal(release.calls.archive, 2);
});

test("exact-release checksum metadata cannot change underneath a cache", async (context) => {
  const root = temporaryRoot(context);
  const archive = makeArchive(root);
  const initial = fakeRelease(archive);
  const options = bootstrapOptions(root, initial.fetchImpl);
  await bootstrap(options);

  const changed = fakeRelease(archive, {
    manifest: `${"f".repeat(64)}  ${TEST_ASSET}\n`,
  });
  await assert.rejects(
    bootstrap({ ...options, fetchImpl: changed.fetchImpl }),
    /published SHA256SUMS changed/,
  );
});

test("offline mode requires and re-verifies cached metadata and archive", async (context) => {
  const root = temporaryRoot(context);
  const archive = makeArchive(root);
  const release = fakeRelease(archive);
  const options = bootstrapOptions(root, release.fetchImpl);
  await bootstrap(options);

  const offline = await bootstrap({
    ...options,
    offline: true,
    fetchImpl: async () => {
      throw new Error("offline mode performed a fetch");
    },
  });
  assert.equal(offline.cache_hit, true);

  fs.rmSync(path.join(root, "cache", "metadata"), {
    recursive: true,
    force: true,
  });
  await assert.rejects(
    bootstrap({ ...options, offline: true }),
    /offline checksum metadata is unavailable/,
  );
});

test("interrupted temporary files never become cache or install state", async (context) => {
  const root = temporaryRoot(context);
  const archive = makeArchive(root);
  const release = fakeRelease(archive);
  const cache = path.join(root, "cache");
  const installParent = path.dirname(path.join(root, "install"));
  fs.mkdirSync(path.join(cache, "downloads", TEST_VERSION), {
    recursive: true,
  });
  fs.writeFileSync(
    path.join(
      cache,
      "downloads",
      TEST_VERSION,
      ".harn-bootstrap-interrupted.tmp",
    ),
    "partial archive",
  );
  fs.mkdirSync(path.join(installParent, ".harn-bootstrap-install-interrupted"));

  const result = await bootstrap(bootstrapOptions(root, release.fetchImpl));
  assert.equal(
    fs.readFileSync(result.binary_path, "utf8"),
    "fake harn binary\n",
  );
  assert.equal(result.checksum, sha256(archive));
});

test("an abandoned install lock is reclaimed before publication", async (context) => {
  const root = temporaryRoot(context);
  const archive = makeArchive(root);
  const release = fakeRelease(archive);
  const lockRoot = writeInstallLock(root, {
    hostname: os.hostname(),
    pid: 2_147_483_647,
  });

  const result = await bootstrap(bootstrapOptions(root, release.fetchImpl));
  assert.equal(fs.existsSync(lockRoot), false);
  assert.equal(
    fs.readFileSync(result.binary_path, "utf8"),
    "fake harn binary\n",
  );
});

test("a live install owner holds the publication boundary", async (context) => {
  const root = temporaryRoot(context);
  const archive = makeArchive(root);
  const release = fakeRelease(archive);
  const lockRoot = writeInstallLock(root, {
    hostname: os.hostname(),
    pid: process.pid,
  });
  const startedAt = Date.now();
  const pending = bootstrap(bootstrapOptions(root, release.fetchImpl));
  setTimeout(() => fs.rmSync(lockRoot, { recursive: true, force: true }), 150);

  const result = await pending;
  assert.ok(Date.now() - startedAt >= 100);
  assert.equal(
    fs.readFileSync(result.binary_path, "utf8"),
    "fake harn binary\n",
  );
});

test("the wait deadline recovers a foreign abandoned lock", async (context) => {
  const root = temporaryRoot(context);
  const archive = makeArchive(root);
  const release = fakeRelease(archive);
  const lockRoot = writeInstallLock(root, {
    hostname: "foreign-bootstrap-host",
    pid: 1,
  });
  const startedAt = Date.now();
  const result = await bootstrap({
    ...bootstrapOptions(root, release.fetchImpl),
    installLockTimeoutMs: 50,
  });

  assert.ok(Date.now() - startedAt >= 40);
  assert.equal(fs.existsSync(lockRoot), false);
  assert.equal(
    fs.readFileSync(result.binary_path, "utf8"),
    "fake harn binary\n",
  );
});

test("a wait deadline does not evict a newly rotated owner", async (context) => {
  const root = temporaryRoot(context);
  const archive = makeArchive(root);
  const release = fakeRelease(archive);
  const lockRoot = writeInstallLock(root, {
    hostname: "foreign-bootstrap-host",
    pid: 1,
    token: "foreign-owner",
  });
  let rotatedOwnerSurvived = false;
  let rotationPrecededInstall = false;
  const startedAt = Date.now();
  const pending = bootstrap({
    ...bootstrapOptions(root, release.fetchImpl),
    installLockTimeoutMs: 50,
  });
  setTimeout(() => {
    rotationPrecededInstall = !fs.existsSync(path.join(root, "install"));
    fs.rmSync(lockRoot, { recursive: true, force: true });
    writeInstallLock(root, {
      hostname: os.hostname(),
      pid: process.pid,
      token: "rotated-live-owner",
    });
  }, 35);
  setTimeout(() => {
    rotatedOwnerSurvived = fs.existsSync(lockRoot);
    fs.rmSync(lockRoot, { recursive: true, force: true });
  }, 70);

  const result = await pending;
  assert.equal(rotationPrecededInstall, true);
  assert.equal(rotatedOwnerSurvived, true);
  assert.ok(Date.now() - startedAt >= 60);
  assert.equal(
    fs.readFileSync(result.binary_path, "utf8"),
    "fake harn binary\n",
  );
});

test("rapid owner rotation still has an overall wait bound", async (context) => {
  const root = temporaryRoot(context);
  const archive = makeArchive(root);
  const release = fakeRelease(archive);
  const lockRoot = writeInstallLock(root, {
    hostname: "foreign-bootstrap-host",
    pid: 1,
  });
  const rotation = setInterval(() => {
    replaceInstallLockOwner(lockRoot, {
      hostname: "foreign-bootstrap-host",
      pid: 1,
    });
  }, 8);
  context.after(() => clearInterval(rotation));

  await assert.rejects(
    bootstrap({
      ...bootstrapOptions(root, release.fetchImpl),
      installLockTimeoutMs: 25,
    }),
    /timed out waiting for Harn installation/,
  );
  clearInterval(rotation);
});

test("a peer install published at the total deadline wins", async (context) => {
  const root = temporaryRoot(context);
  const archive = makeArchive(root);
  const release = fakeRelease(archive);
  const lockRoot = writeInstallLock(root, {
    hostname: os.hostname(),
    pid: process.pid,
  });
  const rotation = setInterval(() => {
    replaceInstallLockOwner(lockRoot, {
      hostname: os.hostname(),
      pid: process.pid,
    });
  }, 8);
  context.after(() => clearInterval(rotation));
  setTimeout(() => publishValidInstall(root, archive), 50);

  const startedAt = Date.now();
  const result = await bootstrap({
    ...bootstrapOptions(root, release.fetchImpl),
    installLockTimeoutMs: 50,
  });
  clearInterval(rotation);

  assert.ok(Date.now() - startedAt >= 150);
  assert.equal(
    fs.existsSync(lockRoot),
    true,
    "the waiter must accept the peer install without evicting or acquiring its lock",
  );
  assert.equal(
    fs.readFileSync(result.binary_path, "utf8"),
    "fake harn binary\n",
  );
});

test("independent processes safely converge on one atomic install", async (context) => {
  const root = temporaryRoot(context);
  const archive = makeArchive(root);
  const archivePath = path.join(root, "fixture.tar.gz");
  fs.writeFileSync(archivePath, archive);
  const moduleUrl = pathToFileURL(
    path.resolve("scripts/bootstrap_harn.mjs"),
  ).href;
  const childScript = `
    import fs from "node:fs";
    const [{ bootstrap, assetForTarget, sha256 }] = await Promise.all([
      import(process.argv[1]),
    ]);
    const archive = fs.readFileSync(process.argv[2]);
    const target = ${JSON.stringify(TEST_TARGET)};
    const asset = assetForTarget(target);
    const fetchImpl = async (url) => new Response(
      url.endsWith("/SHA256SUMS")
        ? sha256(archive) + "  " + asset + "\\n"
        : archive
    );
    const receipt = await bootstrap({
      version: ${JSON.stringify(TEST_VERSION)},
      target,
      cacheDir: process.argv[3],
      installDir: process.argv[4],
      attempts: 1,
      delayMs: 0,
      fetchImpl,
    });
    process.stdout.write(JSON.stringify(receipt));
  `;
  const childArguments = [
    moduleUrl,
    archivePath,
    path.join(root, "cache"),
    path.join(root, "install"),
  ];
  const installRoot = path.join(root, "install");
  fs.mkdirSync(installRoot);
  fs.writeFileSync(
    path.join(installRoot, "install-manifest.json"),
    "corrupt\n",
  );
  const outputs = await Promise.all(
    Array.from({ length: 8 }, () => runChild(childScript, childArguments)),
  );
  const receipts = outputs.map((output) => JSON.parse(output));
  for (const receipt of receipts.slice(1)) {
    assert.equal(receipt.binary_path, receipts[0].binary_path);
  }
  assert.equal(
    fs.readFileSync(receipts[0].binary_path, "utf8"),
    "fake harn binary\n",
  );
  const stored = JSON.parse(
    fs.readFileSync(path.join(root, "install", "install-manifest.json")),
  );
  assert.equal(stored.binary_sha256, sha256(Buffer.from("fake harn binary\n")));
});

test("CLI options preserve explicit directories and exact pin semantics", (context) => {
  const root = temporaryRoot(context);
  const parsed = parseCliArgs([
    "resolve",
    "--version",
    TEST_VERSION,
    "--cache-dir",
    path.join(root, "cache"),
    "--install-dir",
    path.join(root, "install"),
    "--offline",
  ]);
  assert.equal(parsed.mode, "resolve");
  assert.equal(parsed.version, TEST_VERSION);
  assert.equal(parsed.offline, true);
  const resolved = resolveBootstrap({
    version: parsed.version,
    cacheDir: parsed.cacheDir,
    installDir: parsed.installDir,
    target: TEST_TARGET,
  });
  assert.equal(resolved.cache_dir, path.join(root, "cache"));
  assert.equal(resolved.install_dir, path.join(root, "install"));
  assert.throws(() => parseCliArgs(["--version", TEST_VERSION, "latest"]));
});

/** Read a heredoc-form GitHub Actions output back into its lines. */
function readMultilineOutput(filePath, name) {
  const text = fs.readFileSync(filePath, "utf8");
  const lines = text.split(/\r?\n/);
  const opener = lines.findIndex((line) => line.startsWith(`${name}<<`));
  assert.notEqual(opener, -1, `no ${name} output was written`);
  const delimiter = lines[opener].slice(`${name}<<`.length);
  const closer = lines.indexOf(delimiter, opener + 1);
  assert.notEqual(closer, -1, `${name} output was never terminated`);
  return lines.slice(opener + 1, closer);
}

// The cache key names one version and asset. The bootstrap cache *root*
// accumulates every version and target a runner has ever bootstrapped, so
// caching the root archives all of them under each narrow key -- 441 MB for
// one entry on Harn Cloud, whose restore timed out after transferring 80 MB
// while verifying and installing the exact archive directly took 7.2s.
// Regression for #6065.
test("the Actions cache path names one version and asset, not the bootstrap root", async (context) => {
  const root = temporaryRoot(context);
  const cacheDir = path.join(root, "cache");
  const outputFile = path.join(root, "github-output");
  fs.writeFileSync(outputFile, "");

  // What a runner that has bootstrapped other versions and targets holds.
  const otherAsset = assetForTarget("aarch64-apple-darwin");
  const unrelated = [
    path.join(cacheDir, "metadata", "9.9.9", "SHA256SUMS"),
    path.join(cacheDir, "downloads", "9.9.9", otherAsset),
    path.join(cacheDir, "downloads", TEST_VERSION, otherAsset),
    path.join(cacheDir, "installs", "9.9.9", "aarch64-apple-darwin", "harn"),
  ];
  for (const stale of unrelated) {
    fs.mkdirSync(path.dirname(stale), { recursive: true });
    fs.writeFileSync(stale, "stale");
  }

  await main(
    [
      "resolve",
      "--version",
      TEST_VERSION,
      "--target",
      TEST_TARGET,
      "--cache-dir",
      cacheDir,
      "--github-output",
      outputFile,
    ],
    {},
  );

  const cachePath = readMultilineOutput(outputFile, "cache-path");
  assert.deepEqual(cachePath, [
    path.join(cacheDir, "metadata", TEST_VERSION, "SHA256SUMS"),
    path.join(cacheDir, "downloads", TEST_VERSION, TEST_ASSET),
  ]);

  // `actions/cache` archives each listed path and everything under it, so
  // "the entry contains X" is "X is at or under a listed path".
  const contains = (candidate) =>
    cachePath.some(
      (entry) => candidate === entry || candidate.startsWith(entry + path.sep),
    );
  for (const stale of unrelated) {
    assert.equal(
      contains(stale),
      false,
      `a ${TEST_VERSION}/${TEST_TARGET} cache entry would archive ${stale}`,
    );
  }
  assert.equal(contains(cacheDir), false, "the cache root is still archived");
});

// The paths the action caches have to be the paths bootstrap reads and
// writes; two derivations of one layout drift into caching the wrong files.
test("the cached paths are the ones bootstrap uses", (context) => {
  const root = temporaryRoot(context);
  const resolved = resolveBootstrap({
    version: TEST_VERSION,
    target: TEST_TARGET,
    cacheDir: path.join(root, "cache"),
  });
  assert.equal(
    resolved.metadata_path,
    path.join(root, "cache", "metadata", TEST_VERSION, "SHA256SUMS"),
  );
  assert.equal(
    resolved.archive_path,
    path.join(root, "cache", "downloads", TEST_VERSION, TEST_ASSET),
  );
});

// --- release-asset digest fallback (#6036) ----------------------------------
//
// A downstream repo pinning a just-published version could burn its whole CI
// timeout waiting for SHA256SUMS, which appears only when the release is
// finalized -- while the exact asset it needs had been uploaded, and digested
// by GitHub, for minutes. Both sources are now consulted on every attempt.

/**
 * A release whose asset is uploaded and digested but whose SHA256SUMS has not
 * been published yet.
 */
function pendingRelease(archiveBytes, options = {}) {
  const digest =
    options.digest === undefined
      ? `sha256:${sha256(archiveBytes)}`
      : options.digest;
  const calls = { metadata: 0, api: 0, archive: 0 };
  const body = {
    assets: [
      {
        name: "harn-some-other-target.tar.gz",
        digest: `sha256:${"0".repeat(64)}`,
      },
      ...(options.omitAsset ? [] : [{ name: TEST_ASSET, digest }]),
    ],
  };
  return {
    calls,
    fetchImpl: async (url) => {
      if (url.endsWith("/SHA256SUMS")) {
        calls.metadata += 1;
        if (options.manifest) return new Response(options.manifest);
        return new Response("not found", { status: 404 });
      }
      if (url.includes("/releases/tags/")) {
        calls.api += 1;
        if (options.apiStatus) {
          return new Response("nope", { status: options.apiStatus });
        }
        return new Response(JSON.stringify(body));
      }
      if (url.endsWith(`/${TEST_ASSET}`)) {
        calls.archive += 1;
        return new Response(archiveBytes);
      }
      return new Response("missing", { status: 404 });
    },
  };
}

test("an asset digest installs before SHA256SUMS is published", async (t) => {
  const root = temporaryRoot(t);
  const archive = makeArchive(root);
  const release = pendingRelease(archive);

  const result = await bootstrap(bootstrapOptions(root, release.fetchImpl));

  assert.equal(result.checksum_source, "release asset digest");
  assert.equal(result.checksum, sha256(archive));
  assert.ok(fs.existsSync(result.binary_path));
  // SHA256SUMS was still preferred and tried first on the attempt.
  assert.equal(release.calls.metadata, 1);
  assert.equal(release.calls.api, 1);
});

test("a digest that does not match the bytes is a hard failure", async (t) => {
  const root = temporaryRoot(t);
  const archive = makeArchive(root);
  const release = pendingRelease(archive, {
    digest: `sha256:${"1".repeat(64)}`,
  });

  await assert.rejects(
    bootstrap(bootstrapOptions(root, release.fetchImpl)),
    /checksum mismatch/,
  );
});

test("a malformed or absent digest exhausts the bounded budget and explains both sources", async (t) => {
  const root = temporaryRoot(t);
  const archive = makeArchive(root);
  const malformed = pendingRelease(archive, { digest: "not-a-digest" });

  await assert.rejects(
    bootstrap({ ...bootstrapOptions(root, malformed.fetchImpl), attempts: 2 }),
    (error) =>
      /no verification source for/.test(error.message) &&
      /SHA256SUMS/.test(error.message) &&
      /well-formed sha256 digest/.test(error.message),
  );
  // Bounded: two attempts, each consulting both sources, plus the single
  // terminal release-state query (#7221). No unbounded wait.
  assert.equal(malformed.calls.metadata, 2);
  assert.equal(malformed.calls.api, 3);

  const missing = pendingRelease(archive, { omitAsset: true });
  await assert.rejects(
    bootstrap({ ...bootstrapOptions(root, missing.fetchImpl), attempts: 1 }),
    /has no asset named/,
  );
});

test("SHA256SUMS disagreeing with a verified digest fails an exact release", async (t) => {
  const root = temporaryRoot(t);
  const archive = makeArchive(root);
  const options = bootstrapOptions(root, pendingRelease(archive).fetchImpl);

  const first = await bootstrap(options);
  assert.equal(first.checksum_source, "release asset digest");

  // The release is later finalized, but SHA256SUMS states a different
  // checksum for the same asset of the same tag. That is a mutated release.
  const contradicting = pendingRelease(archive, {
    manifest: `${"2".repeat(64)}  ${TEST_ASSET}\n`,
  });
  await assert.rejects(
    bootstrap({ ...options, fetchImpl: contradicting.fetchImpl }),
    /disagrees with the verified asset digest for an exact release/,
  );

  // An agreeing SHA256SUMS finalizes normally and becomes the source.
  const agreeing = pendingRelease(archive, {
    manifest: `${sha256(archive)}  ${TEST_ASSET}\n`,
  });
  const finalized = await bootstrap({
    ...options,
    fetchImpl: agreeing.fetchImpl,
  });
  assert.equal(finalized.checksum_source, "SHA256SUMS");
  assert.equal(finalized.cache_hit, true);
});

// --- terminal release-state diagnosis (#7221) -------------------------------
//
// A release that does not exist and a release whose asset upload stalled both
// answer `HTTP 404` on every source, so the exhausted-budget failure could not
// tell them apart. A half-published v0.10.115 therefore blocked every pinned
// consumer for ~70 minutes while reading as a flaky download. Each state now
// has to name itself, which is why these three assert distinct text rather
// than that a failure occurred: an "it errors" assertion passed before the fix.

test("an absent release is named as absent, not as a transport 404", async (t) => {
  const root = temporaryRoot(t);
  const absent = pendingRelease(makeArchive(root), { apiStatus: 404 });

  await assert.rejects(
    bootstrap({ ...bootstrapOptions(root, absent.fetchImpl), attempts: 2 }),
    (error) =>
      /observed release state: release v1\.2\.3 does not exist:/.test(
        error.message,
      ) &&
      // The transport errors are added to, never replaced.
      /SHA256SUMS: HTTP 404/.test(error.message) &&
      !/does not publish|mid-finalization/.test(error.message),
  );
  // Exactly one query beyond the two attempts: the diagnostic never enters the
  // retry loop, where it would spend the shared 60/hour unauthenticated budget.
  assert.equal(absent.calls.api, 3);
});

test("a published release missing this asset says so and lists what it has", async (t) => {
  const root = temporaryRoot(t);
  const partial = pendingRelease(makeArchive(root), { omitAsset: true });

  await assert.rejects(
    bootstrap({ ...bootstrapOptions(root, partial.fetchImpl), attempts: 2 }),
    (error) =>
      new RegExp(
        `observed release state: release v1\\.2\\.3 exists but does not publish ${TEST_ASSET.replace(/\./g, "\\.")}: ` +
          `it has 1 asset\\(s\\) \\[harn-some-other-target\\.tar\\.gz\\]\\. The release is incomplete`,
      ).test(error.message) &&
      /SHA256SUMS: HTTP 404/.test(error.message) &&
      !/does not exist|mid-finalization/.test(error.message),
  );
  assert.equal(partial.calls.api, 3);
});

test("a released asset without SHA256SUMS reads as mid-finalization", async (t) => {
  const root = temporaryRoot(t);
  // The asset is published, so only the finalization artifacts are missing.
  const finalizing = pendingRelease(makeArchive(root), {
    digest: "not-a-digest",
  });

  await assert.rejects(
    bootstrap({ ...bootstrapOptions(root, finalizing.fetchImpl), attempts: 2 }),
    (error) =>
      new RegExp(
        `observed release state: release v1\\.2\\.3 exists and publishes ${TEST_ASSET.replace(/\./g, "\\.")}, ` +
          `but neither SHA256SUMS nor a usable asset digest was available: the release is mid-finalization`,
      ).test(error.message) &&
      !/does not exist|does not publish/.test(error.message),
  );
});

test("a throttled state query degrades to the transport errors alone", async (t) => {
  const root = temporaryRoot(t);
  // 403 is what a spent unauthenticated rate limit looks like. The state is
  // then unknown, and an unknown state must not be reported as a known one.
  const throttled = pendingRelease(makeArchive(root), { apiStatus: 403 });

  await assert.rejects(
    bootstrap({ ...bootstrapOptions(root, throttled.fetchImpl), attempts: 2 }),
    (error) =>
      /no verification source for/.test(error.message) &&
      /releases\/tags\/v1\.2\.3: HTTP 403/.test(error.message) &&
      !/observed release state/.test(error.message),
  );
});

test("a state query that answers nonsense does not invent a state", async (t) => {
  const root = temporaryRoot(t);
  const archive = makeArchive(root);
  let apiCalls = 0;
  const fetchImpl = async (url) => {
    if (url.endsWith("/SHA256SUMS"))
      return new Response("nope", { status: 404 });
    if (url.includes("/releases/tags/")) {
      apiCalls += 1;
      // Reachable, 200, and not JSON: neither absent nor describable.
      return new Response("<html>proxy interstitial</html>");
    }
    return new Response(archive);
  };

  await assert.rejects(
    bootstrap({ ...bootstrapOptions(root, fetchImpl), attempts: 1 }),
    (error) =>
      /no verification source for/.test(error.message) &&
      !/observed release state/.test(error.message),
  );
  assert.equal(apiCalls, 2);
});
