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
  const [left, right] = await Promise.all([
    runChild(childScript, childArguments),
    runChild(childScript, childArguments),
  ]);
  const receipts = [JSON.parse(left), JSON.parse(right)];
  assert.equal(receipts[0].binary_path, receipts[1].binary_path);
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
