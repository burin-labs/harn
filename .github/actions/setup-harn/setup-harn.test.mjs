import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import {
  archivePathForTar,
  assetForTarget,
  ensureVerifiedArchive,
  extractionCommand,
  fetchWithRetry,
  normalizeVersion,
  parseChecksums,
  releaseTarget,
  sha256,
} from "./setup-harn.mjs";

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
    HARN_SETUP_ARCHIVE_PATH: String.raw`C:\cache\harn.zip`,
    HARN_SETUP_EXTRACT_PATH: String.raw`C:\cache\install`,
  });

  const linux = extractionCommand(
    "/cache/downloads/harn.tar.gz",
    "/cache/install",
    "x86_64-unknown-linux-gnu",
  );
  assert.deepEqual(linux, {
    command: "tar",
    args: ["-xf", "../downloads/harn.tar.gz"],
  });
});

test("versions are canonical and traversal-safe", () => {
  assert.equal(normalizeVersion(" v1.2.3\n"), "1.2.3");
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

test("hosted runner targets map to published assets", () => {
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
  assert.throws(() => releaseTarget("Windows", "ARM64"));
});

test("checksum manifests accept standard and binary markers", () => {
  const a = "a".repeat(64);
  const b = "b".repeat(64);
  const parsed = parseChecksums(
    `${a}  harn-linux.tar.gz\n${b} *harn.exe.zip\nnoise`,
  );
  assert.equal(parsed.get("harn-linux.tar.gz"), a);
  assert.equal(parsed.get("harn.exe.zip"), b);
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

test("verified cached archives are reused and tampered archives are replaced", async (context) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "setup-harn-test-"));
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
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
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "setup-harn-test-"));
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
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
