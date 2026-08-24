import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

export const RECEIPT_SCHEMA = "harn-bootstrap-v1";

const INSTALL_MANIFEST_SCHEMA = "harn-bootstrap-install-v1";
const INSTALL_LOCK_SCHEMA = "harn-bootstrap-install-lock-v1";
const INSTALL_LOCK_POLL_MS = 10;
const INSTALL_LOCK_TIMEOUT_MS = 30_000;
const INSTALL_LOCK_TOTAL_TIMEOUT_MULTIPLIER = 4;
const INSTALL_LOCK_STALE_MS = 5 * 60_000;
const INSTALL_MUTATION_ATTEMPTS = 6;
const INSTALL_MUTATION_RETRY_MS = 25;
const WINDOWS_MULTICALL_ALIASES = ["harn-lsp.exe", "harn-dap.exe"];
const SEMVER = /^(?:v)?(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const REPOSITORY = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;
const CHECKSUM_LINE = /^([0-9a-fA-F]{64})[ \t]+\*?([^/\\\0]+)$/;
const TRANSIENT_NAMES = /^\.harn-bootstrap-/;
const SUPPORTED_TARGETS = new Set([
  "aarch64-apple-darwin",
  "aarch64-unknown-linux-gnu",
  "x86_64-apple-darwin",
  "x86_64-pc-windows-msvc",
  "x86_64-unknown-linux-gnu",
]);

class InstallLockTimeoutError extends Error {
  constructor(installRoot) {
    super(`timed out waiting for Harn installation at ${installRoot}`);
    this.name = "InstallLockTimeoutError";
    this.code = "HARN_INSTALL_LOCK_TIMEOUT";
  }
}

export function normalizeVersion(value) {
  const text = String(value ?? "").trim();
  const match = SEMVER.exec(text);
  if (!match) {
    throw new Error(
      `expected Harn version MAJOR.MINOR.PATCH, got ${JSON.stringify(text)}`,
    );
  }
  return `${match[1]}.${match[2]}.${match[3]}`;
}

export function resolveVersion(options = {}) {
  const explicit = String(options.version ?? "").trim();
  const requestedFile = String(options.versionFile ?? "").trim();
  if (explicit && requestedFile) {
    throw new Error("--version and --version-file are mutually exclusive");
  }
  if (explicit) return normalizeVersion(explicit);
  if (!requestedFile) {
    throw new Error("an exact --version or --version-file is required");
  }
  const versionFile = path.resolve(options.cwd ?? process.cwd(), requestedFile);
  let text;
  try {
    text = fs.readFileSync(versionFile, "utf8");
  } catch (error) {
    throw new Error(
      `could not read version file ${versionFile}: ${error.message}`,
    );
  }
  return normalizeVersion(text);
}

export function releaseTarget(runnerOs, runnerArch) {
  const platform = String(runnerOs).toLowerCase();
  const arch = String(runnerArch).toLowerCase();
  const normalizedArch =
    arch === "x64" || arch === "x86_64"
      ? "x86_64"
      : arch === "arm64" || arch === "aarch64"
        ? "aarch64"
        : null;
  if (!normalizedArch) {
    throw new Error(`unsupported runner architecture: ${runnerArch}`);
  }
  if (platform === "linux") return `${normalizedArch}-unknown-linux-gnu`;
  if (platform === "macos" || platform === "darwin")
    return `${normalizedArch}-apple-darwin`;
  if (
    (platform === "windows" || platform === "win32") &&
    normalizedArch === "x86_64"
  )
    return "x86_64-pc-windows-msvc";
  throw new Error(`unsupported Harn release target: ${runnerOs}/${runnerArch}`);
}

export function assetForTarget(target) {
  if (!SUPPORTED_TARGETS.has(target)) {
    throw new Error(`unsupported Harn release target: ${target}`);
  }
  return `harn-${target}.${target.endsWith("windows-msvc") ? "zip" : "tar.gz"}`;
}

export function parseChecksums(text) {
  const checksums = new Map();
  for (const [index, rawLine] of String(text).split(/\r?\n/).entries()) {
    const line = rawLine.trim();
    if (!line) continue;
    const match = CHECKSUM_LINE.exec(line);
    if (!match) {
      throw new Error(`malformed SHA256SUMS line ${index + 1}`);
    }
    const checksum = match[1].toLowerCase();
    const name = match[2];
    if (checksums.has(name)) {
      throw new Error(`SHA256SUMS contains duplicate entry for ${name}`);
    }
    checksums.set(name, checksum);
  }
  if (checksums.size === 0) throw new Error("SHA256SUMS is empty");
  return checksums;
}

export function sha256(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

export function archivePathForTar(directory, archivePath, pathApi = path) {
  return pathApi.relative(directory, archivePath).split(pathApi.sep).join("/");
}

export function extractionCommand(archivePath, directory, target) {
  if (target.endsWith("windows-msvc")) {
    return {
      command: "powershell.exe",
      args: [
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "Expand-Archive -LiteralPath $env:HARN_BOOTSTRAP_ARCHIVE_PATH -DestinationPath $env:HARN_BOOTSTRAP_EXTRACT_PATH -Force",
      ],
      env: {
        HARN_BOOTSTRAP_ARCHIVE_PATH: archivePath,
        HARN_BOOTSTRAP_EXTRACT_PATH: directory,
      },
    };
  }
  return {
    command: "tar",
    args: ["-xf", archivePathForTar(directory, archivePath)],
  };
}

/** One HTTP GET. Throws on a non-2xx status so callers can branch on it. */
async function fetchOnce(url, options = {}, extraHeaders = {}) {
  const fetchImpl = options.fetchImpl ?? globalThis.fetch;
  const response = await fetchImpl(url, {
    headers: { "user-agent": "burin-labs/harn-bootstrap", ...extraHeaders },
    redirect: "follow",
    signal: AbortSignal.timeout(30_000),
  });
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  return Buffer.from(await response.arrayBuffer());
}

function retrySchedule(options = {}) {
  return {
    attempts: options.attempts ?? 12,
    delayMs: options.delayMs ?? 10_000,
    delay:
      options.delay ??
      ((ms) => new Promise((resolve) => setTimeout(resolve, ms))),
  };
}

export async function fetchWithRetry(url, options = {}) {
  const { attempts, delayMs, delay } = retrySchedule(options);
  let lastError;
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      return await fetchOnce(url, options);
    } catch (error) {
      lastError = error;
      if (attempt < attempts) await delay(delayMs);
    }
  }
  throw new Error(
    `release asset unavailable after ${attempts} attempt(s): ${url}: ${lastError?.message ?? lastError}`,
  );
}

function transientPath(parent, label, suffix) {
  return path.join(
    parent,
    `.harn-bootstrap-${label}-${process.pid}-${crypto.randomUUID()}${suffix}`,
  );
}

function writeDurably(filePath, bytes) {
  const descriptor = fs.openSync(filePath, "wx", 0o600);
  try {
    fs.writeFileSync(descriptor, bytes);
    fs.fsyncSync(descriptor);
  } finally {
    fs.closeSync(descriptor);
  }
}

function publishFile(destination, bytes) {
  const parent = path.dirname(destination);
  fs.mkdirSync(parent, { recursive: true });
  const temporary = transientPath(parent, path.basename(destination), ".tmp");
  try {
    writeDurably(temporary, bytes);
    try {
      fs.linkSync(temporary, destination);
    } catch (error) {
      if (error.code !== "EEXIST" || !fs.existsSync(destination)) {
        throw error;
      }
      if (!fs.readFileSync(destination).equals(bytes)) {
        throw new Error(`concurrent publication disagreed for ${destination}`);
      }
    }
  } finally {
    fs.rmSync(temporary, { force: true });
  }
}

function discardInvalid(candidate) {
  if (!fs.existsSync(candidate)) return;
  const parent = path.dirname(candidate);
  const quarantine = transientPath(
    parent,
    path.basename(candidate),
    ".corrupt",
  );
  try {
    fs.renameSync(candidate, quarantine);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    return;
  }
  fs.rmSync(quarantine, { recursive: true, force: true });
}

export async function ensureVerifiedArchive(options) {
  const { archivePath, expectedChecksum, sourceUrl } = options;
  if (fs.existsSync(archivePath)) {
    const cached = fs.readFileSync(archivePath);
    if (sha256(cached) === expectedChecksum) {
      return { bytes: cached, cacheHit: true };
    }
    discardInvalid(archivePath);
  }
  if (options.offline) {
    throw new Error(
      `offline cache miss for ${path.basename(archivePath)} in ${path.dirname(archivePath)}`,
    );
  }
  const bytes = await fetchWithRetry(sourceUrl, options);
  const actual = sha256(bytes);
  if (actual !== expectedChecksum) {
    throw new Error(
      `checksum mismatch for ${path.basename(archivePath)}: expected ${expectedChecksum}, got ${actual}`,
    );
  }
  publishFile(archivePath, bytes);
  const published = fs.readFileSync(archivePath);
  if (sha256(published) !== expectedChecksum) {
    throw new Error(`concurrent cache publication corrupted ${archivePath}`);
  }
  return { bytes: published, cacheHit: false };
}

const ASSET_DIGEST = /^sha256:([0-9a-f]{64})$/;

/**
 * SHA-256 that GitHub publishes for one named release asset.
 *
 * The release API exposes a per-asset `digest` as soon as the asset finishes
 * uploading -- well before the release is finalized and `SHA256SUMS` exists.
 * It is computed by GitHub over the stored bytes, so it is an independent
 * check on the download rather than a restatement of it.
 */
export function parseAssetDigest(body, assetName) {
  let release;
  try {
    release = JSON.parse(body.toString("utf8"));
  } catch (error) {
    throw new Error(`release API response is not JSON: ${error.message}`);
  }
  const assets = Array.isArray(release?.assets) ? release.assets : [];
  const asset = assets.find((candidate) => candidate?.name === assetName);
  if (!asset) {
    throw new Error(`release API has no asset named ${assetName}`);
  }
  const match = ASSET_DIGEST.exec(String(asset.digest ?? ""));
  if (!match) {
    throw new Error(
      `release API asset ${assetName} has no well-formed sha256 digest`,
    );
  }
  return match[1];
}

/**
 * Where a digest-verified install records what it trusted.
 *
 * A digest install publishes no `SHA256SUMS`, so the immutability guard in
 * `checksumManifest` has nothing to compare against on a later run. This pin
 * gives it one: if `SHA256SUMS` later states a different checksum for the same
 * asset of the same tag, the release changed under an exact version and that
 * must fail rather than quietly reinstall.
 */
function digestPinPath(metadataPath, assetName) {
  return path.join(path.dirname(metadataPath), `${assetName}.sha256`);
}

function readDigestPin(metadataPath, assetName) {
  try {
    return fs
      .readFileSync(digestPinPath(metadataPath, assetName), "utf8")
      .trim();
  } catch (error) {
    if (error.code === "ENOENT") return undefined;
    throw error;
  }
}

/**
 * The checksum to verify the archive against, from whichever canonical source
 * exists first.
 *
 * `SHA256SUMS` stays preferred: it covers the whole release and carries the
 * immutability guarantee. But a downstream job that pins a just-published
 * version would otherwise burn its entire timeout waiting for a file that
 * appears only at finalization, while the exact asset it needs has been
 * uploaded and digested for minutes (#6036). Both sources are consulted on
 * every attempt, so the install starts as soon as either one is there, and the
 * attempt budget is the same one a `SHA256SUMS`-only wait would have spent.
 */
async function resolveExpectedChecksum(options) {
  const { metadataPath, checksumUrl, releaseApiUrl, assetName } = options;
  if (options.offline) {
    const manifest = await checksumManifest(options);
    const checksums = parseChecksums(manifest.toString("utf8"));
    const checksum = checksums.get(assetName);
    if (!checksum) throw new Error(`SHA256SUMS does not contain ${assetName}`);
    return { checksum, source: "SHA256SUMS" };
  }

  const { attempts, delayMs, delay } = retrySchedule(options);
  let checksumError;
  let digestError;
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      const downloaded = await fetchOnce(checksumUrl, options);
      const checksums = parseChecksums(downloaded.toString("utf8"));
      const checksum = checksums.get(assetName);
      if (!checksum) {
        throw new Error(`SHA256SUMS does not contain ${assetName}`);
      }
      const pinned = readDigestPin(metadataPath, assetName);
      if (pinned && pinned !== checksum) {
        throw new Error(
          `published SHA256SUMS disagrees with the verified asset digest for an exact release: ${assetName} was ${pinned}, SHA256SUMS says ${checksum}`,
        );
      }
      publishChecksumMetadata(metadataPath, downloaded);
      return { checksum, source: "SHA256SUMS" };
    } catch (error) {
      if (
        /disagrees with the verified asset digest|changed for an exact release/.test(
          error.message,
        )
      ) {
        throw error;
      }
      checksumError = error;
    }

    try {
      const body = await fetchOnce(releaseApiUrl, options, {
        accept: "application/vnd.github+json",
        // Unauthenticated api.github.com is 60 requests/hour per IP, which
        // GitHub-hosted runners share. A token is optional -- the release
        // download host needs none -- but without one this fallback is the
        // first thing to be throttled.
        ...(options.token ? { authorization: `Bearer ${options.token}` } : {}),
      });
      const checksum = parseAssetDigest(body, assetName);
      publishFile(
        digestPinPath(metadataPath, assetName),
        Buffer.from(`${checksum}\n`),
      );
      return { checksum, source: "release asset digest" };
    } catch (error) {
      digestError = error;
    }

    if (attempt < attempts) await delay(delayMs);
  }
  throw new Error(
    `no verification source for ${assetName} after ${attempts} attempt(s): ` +
      `${checksumUrl}: ${checksumError?.message ?? checksumError}; ` +
      `${releaseApiUrl}: ${digestError?.message ?? digestError}`,
  );
}

/** Publish `SHA256SUMS`, refusing a change to an already-cached release. */
function publishChecksumMetadata(metadataPath, downloaded) {
  if (fs.existsSync(metadataPath)) {
    const cached = fs.readFileSync(metadataPath);
    if (!cached.equals(downloaded)) {
      throw new Error(
        `published SHA256SUMS changed for an exact release: ${metadataPath}`,
      );
    }
    return;
  }
  publishFile(metadataPath, downloaded);
  const published = fs.readFileSync(metadataPath);
  if (!published.equals(downloaded)) {
    throw new Error(
      `concurrent checksum metadata publication disagreed for ${metadataPath}`,
    );
  }
}

/**
 * Cached `SHA256SUMS` for an offline install.
 *
 * Only the offline path reads this now: online, `resolveExpectedChecksum`
 * owns fetching and publishing so that the release-asset digest can be tried
 * in the same attempt.
 */
async function checksumManifest(options) {
  const { metadataPath } = options;
  let cached;
  try {
    cached = fs.readFileSync(metadataPath);
  } catch (error) {
    throw new Error(
      `offline checksum metadata is unavailable at ${metadataPath}: ${error.message}`,
    );
  }
  parseChecksums(cached.toString("utf8"));
  return cached;
}

function findBinary(root, binaryName) {
  const pending = [{ directory: root, depth: 0 }];
  while (pending.length) {
    const { directory, depth } = pending.shift();
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      if (TRANSIENT_NAMES.test(entry.name)) continue;
      const candidate = path.join(directory, entry.name);
      if (entry.isFile() && entry.name === binaryName) return candidate;
      if (entry.isDirectory() && depth < 2) {
        pending.push({ directory: candidate, depth: depth + 1 });
      }
    }
  }
  throw new Error(
    `${binaryName} was not present in the verified release archive`,
  );
}

function extractArchive(archivePath, directory, target) {
  const extraction = extractionCommand(archivePath, directory, target);
  const extracted = spawnSync(extraction.command, extraction.args, {
    cwd: directory,
    encoding: "utf8",
    env: { ...process.env, ...extraction.env },
  });
  if (extracted.error || extracted.status !== 0) {
    throw new Error(
      `archive extraction failed: ${
        extracted.error?.message || extracted.stderr || extracted.stdout
      }`,
    );
  }
  const binaryName = target.endsWith("windows-msvc") ? "harn.exe" : "harn";
  const found = findBinary(directory, binaryName);
  const destination = path.join(directory, binaryName);
  if (found !== destination) fs.renameSync(found, destination);
  if (binaryName === "harn") fs.chmodSync(destination, 0o755);
  return destination;
}

/**
 * Give Windows installs the same one-binary layout as Unix installs.
 *
 * Hard links preserve argv[0] dispatch without requiring elevation or
 * Developer Mode. The release archive therefore carries only harn.exe.
 */
export function installMulticallAliases(binaryPath, target) {
  if (!target.endsWith("windows-msvc")) return [];
  return WINDOWS_MULTICALL_ALIASES.map((name) => {
    const aliasPath = path.join(path.dirname(binaryPath), name);
    fs.rmSync(aliasPath, { force: true });
    fs.linkSync(binaryPath, aliasPath);
    return aliasPath;
  });
}

function hasMulticallAliases(binaryPath, target) {
  if (!target.endsWith("windows-msvc")) return true;
  const binary = fs.statSync(binaryPath);
  return WINDOWS_MULTICALL_ALIASES.every((name) => {
    const alias = fs.statSync(path.join(path.dirname(binaryPath), name));
    return alias.dev === binary.dev && alias.ino === binary.ino;
  });
}

function readValidInstall(installRoot, expected) {
  try {
    const manifestPath = path.join(installRoot, "install-manifest.json");
    const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
    const binaryName = expected.target.endsWith("windows-msvc")
      ? "harn.exe"
      : "harn";
    const binaryPath = path.join(installRoot, binaryName);
    if (
      manifest.schema_version !== INSTALL_MANIFEST_SCHEMA ||
      manifest.version !== expected.version ||
      manifest.target !== expected.target ||
      manifest.checksum !== expected.checksum ||
      manifest.source !== expected.source ||
      manifest.binary_path !== binaryPath ||
      manifest.binary_sha256 !== sha256(fs.readFileSync(binaryPath)) ||
      !hasMulticallAliases(binaryPath, expected.target)
    ) {
      return null;
    }
    return {
      binaryPath,
      binaryChecksum: manifest.binary_sha256,
    };
  } catch {
    return null;
  }
}

function processIsRunning(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error.code !== "ESRCH";
  }
}

function retryableInstallMutation(error) {
  return ["EACCES", "EBUSY", "EEXIST", "ENOTEMPTY", "ENOENT", "EPERM"].includes(
    error.code,
  );
}

function inspectInstallLock(lockPath) {
  let owner = null;
  try {
    const ownerPath = path.join(lockPath, "owner.json");
    owner = JSON.parse(fs.readFileSync(ownerPath, "utf8"));
  } catch {
    // A creator can be between mkdir and owner publication. The directory's
    // filesystem identity still lets waiters distinguish that owner from a
    // later one without treating an incomplete record as abandoned.
  }
  try {
    const stat = fs.statSync(lockPath);
    const token =
      owner?.schema_version === INSTALL_LOCK_SCHEMA &&
      typeof owner.token === "string" &&
      owner.token.length > 0
        ? owner.token
        : null;
    return {
      ageMs: Date.now() - stat.mtimeMs,
      identity: token ?? `${stat.dev}:${stat.ino}:${stat.mtimeMs}`,
      owner,
      token,
    };
  } catch {
    return null;
  }
}

async function discardInstallLock(lockPath, expectedIdentity) {
  for (let attempt = 1; attempt <= INSTALL_MUTATION_ATTEMPTS; attempt += 1) {
    const current = inspectInstallLock(lockPath);
    if (!current) return true;
    if (current.identity !== expectedIdentity) return false;
    try {
      discardInvalid(lockPath);
      return true;
    } catch (error) {
      if (
        !retryableInstallMutation(error) ||
        attempt === INSTALL_MUTATION_ATTEMPTS
      ) {
        throw error;
      }
      await new Promise((resolve) =>
        setTimeout(resolve, INSTALL_MUTATION_RETRY_MS * 2 ** (attempt - 1)),
      );
    }
  }
}

async function reclaimAbandonedInstallLock(lockPath, expectedIdentity = null) {
  const inspected = inspectInstallLock(lockPath);
  if (!inspected) return true;
  const { ageMs, identity, owner } = inspected;
  const localOwner =
    owner?.schema_version === INSTALL_LOCK_SCHEMA &&
    owner.hostname === os.hostname() &&
    Number.isSafeInteger(owner.pid) &&
    owner.pid > 0;
  // Hostname equality is useful evidence only for ordinary host processes;
  // the age ceiling remains authoritative for shared container mounts where
  // PID namespaces can differ despite equal configured hostnames.
  const abandoned =
    ageMs >= INSTALL_LOCK_STALE_MS ||
    (localOwner && !processIsRunning(owner.pid));
  if (!abandoned && identity !== expectedIdentity) return false;
  return discardInstallLock(lockPath, identity);
}

// Lock invariants: identity changes reset only the per-owner eviction deadline;
// the total wait remains bounded; token checks fence every destructive install
// mutation; and a waiter accepts a peer publication only after full manifest
// and binary validation.
async function withInstallLock(
  installRoot,
  callback,
  timeoutMs = INSTALL_LOCK_TIMEOUT_MS,
) {
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
    throw new Error("install lock timeout must be a positive number");
  }
  const lockPath = `${installRoot}.lock`;
  const absoluteDeadline =
    Date.now() + timeoutMs * INSTALL_LOCK_TOTAL_TIMEOUT_MULTIPLIER;
  let deadline = Date.now() + timeoutMs;
  let observedIdentity = null;
  const token = crypto.randomUUID();
  for (;;) {
    try {
      fs.mkdirSync(lockPath);
      try {
        fs.writeFileSync(
          path.join(lockPath, "owner.json"),
          `${JSON.stringify({
            schema_version: INSTALL_LOCK_SCHEMA,
            hostname: os.hostname(),
            pid: process.pid,
            token,
          })}\n`,
        );
      } catch (error) {
        fs.rmSync(lockPath, { recursive: true, force: true });
        throw error;
      }
      break;
    } catch (error) {
      if (error.code !== "EEXIST") throw error;
      if (Date.now() >= absoluteDeadline) {
        throw new InstallLockTimeoutError(installRoot);
      }
      const inspected = inspectInstallLock(lockPath);
      if (inspected) {
        if (inspected.identity !== observedIdentity) {
          observedIdentity = inspected.identity;
          deadline = Date.now() + timeoutMs;
        }
        if (Date.now() >= deadline) {
          // A critical section should finish in milliseconds. At the owner
          // deadline, evict only the exact identity observed for that wait.
          // Rotation gets a fresh owner deadline but not a fresh total bound.
          await reclaimAbandonedInstallLock(lockPath, observedIdentity);
          continue;
        }
        if (await reclaimAbandonedInstallLock(lockPath)) continue;
      }
      await new Promise((resolve) => setTimeout(resolve, INSTALL_LOCK_POLL_MS));
    }
  }
  const assertOwned = () => {
    if (inspectInstallLock(lockPath)?.token !== token) {
      throw new Error(`lost Harn installation lock at ${installRoot}`);
    }
  };
  try {
    return await callback(assertOwned);
  } finally {
    try {
      const owner = JSON.parse(
        fs.readFileSync(path.join(lockPath, "owner.json"), "utf8"),
      );
      if (owner.token === token) {
        fs.rmSync(lockPath, { recursive: true, force: true });
      }
    } catch {
      // A stale-lock recovery may already have replaced this ownership token.
    }
  }
}

async function commitInstall(temporary, installRoot, expected, lockTimeoutMs) {
  try {
    return await withInstallLock(
      installRoot,
      async (assertOwned) => {
        const current = readValidInstall(installRoot, expected);
        if (current) return current;
        for (
          let attempt = 1;
          attempt <= INSTALL_MUTATION_ATTEMPTS;
          attempt += 1
        ) {
          try {
            assertOwned();
            discardInvalid(installRoot);
            assertOwned();
            fs.renameSync(temporary, installRoot);
            return readValidInstall(installRoot, expected);
          } catch (error) {
            if (
              !retryableInstallMutation(error) ||
              attempt === INSTALL_MUTATION_ATTEMPTS
            ) {
              throw error;
            }
            await new Promise((resolve) =>
              setTimeout(
                resolve,
                INSTALL_MUTATION_RETRY_MS * 2 ** (attempt - 1),
              ),
            );
          }
        }
        return null;
      },
      lockTimeoutMs,
    );
  } catch (error) {
    if (error?.code !== "HARN_INSTALL_LOCK_TIMEOUT") throw error;
    const winner = readValidInstall(installRoot, expected);
    if (winner) return winner;
    throw error;
  }
}

async function installVerifiedArchive(options) {
  const expected = {
    version: options.version,
    target: options.target,
    checksum: options.checksum,
    source: options.source,
  };
  const existing = readValidInstall(options.installRoot, expected);
  if (existing) return existing;

  const parent = path.dirname(options.installRoot);
  fs.mkdirSync(parent, { recursive: true });
  const temporary = fs.mkdtempSync(
    path.join(parent, `.harn-bootstrap-install-${process.pid}-`),
  );
  try {
    const temporaryBinary = extractArchive(
      options.archivePath,
      temporary,
      options.target,
    );
    installMulticallAliases(temporaryBinary, options.target);
    const binaryName = path.basename(temporaryBinary);
    const binaryPath = path.join(options.installRoot, binaryName);
    const binaryChecksum = sha256(fs.readFileSync(temporaryBinary));
    const manifestPath = path.join(
      options.installRoot,
      "install-manifest.json",
    );
    const installManifest = {
      schema_version: INSTALL_MANIFEST_SCHEMA,
      version: options.version,
      target: options.target,
      binary_path: binaryPath,
      source: options.source,
      checksum: options.checksum,
      binary_sha256: binaryChecksum,
    };
    fs.writeFileSync(
      path.join(temporary, "install-manifest.json"),
      `${JSON.stringify(installManifest, null, 2)}\n`,
    );
    const committed = await commitInstall(
      temporary,
      options.installRoot,
      expected,
      options.lockTimeoutMs,
    );
    if (!committed) {
      throw new Error(
        `installed manifest failed validation at ${manifestPath}`,
      );
    }
    return committed;
  } finally {
    fs.rmSync(temporary, { recursive: true, force: true });
  }
}

function defaultCacheDir(environment = process.env) {
  if (environment.RUNNER_TOOL_CACHE) {
    return path.join(environment.RUNNER_TOOL_CACHE, "harn-bootstrap");
  }
  if (environment.XDG_CACHE_HOME) {
    return path.join(environment.XDG_CACHE_HOME, "harn", "bootstrap");
  }
  if (process.platform === "win32" && environment.LOCALAPPDATA) {
    return path.join(environment.LOCALAPPDATA, "harn", "bootstrap");
  }
  return path.join(os.homedir(), ".cache", "harn", "bootstrap");
}

export function resolveBootstrap(options = {}) {
  const version = resolveVersion(options);
  const target =
    options.target ??
    releaseTarget(options.platform ?? os.platform(), options.arch ?? os.arch());
  const asset = assetForTarget(target);
  const environment = options.env ?? process.env;
  const cacheDir = path.resolve(
    options.cacheDir ?? defaultCacheDir(environment),
  );
  const installDir = path.resolve(
    options.installDir ??
      (environment.RUNNER_TOOL_CACHE
        ? path.join(environment.RUNNER_TOOL_CACHE, "harn", version, target)
        : path.join(cacheDir, "installs", version, target)),
  );
  return {
    version,
    target,
    asset,
    cache_dir: cacheDir,
    install_dir: installDir,
    metadata_path: path.join(cacheDir, "metadata", version, "SHA256SUMS"),
    archive_path: path.join(cacheDir, "downloads", version, asset),
  };
}

export async function bootstrap(options = {}) {
  const resolved = resolveBootstrap(options);
  const repository = options.repository ?? "burin-labs/harn";
  if (!REPOSITORY.test(repository)) {
    throw new Error(`invalid repository: ${repository}`);
  }
  const releaseRoot = `https://github.com/${repository}/releases/download/v${resolved.version}`;
  const checksumUrl = `${releaseRoot}/SHA256SUMS`;
  const sourceUrl = `${releaseRoot}/${resolved.asset}`;
  const releaseApiUrl = `https://api.github.com/repos/${repository}/releases/tags/v${resolved.version}`;
  const metadataPath = resolved.metadata_path;
  const { checksum: expectedChecksum, source: checksumSource } =
    await resolveExpectedChecksum({
      metadataPath,
      checksumUrl,
      releaseApiUrl,
      assetName: resolved.asset,
      token: options.token,
      offline: options.offline,
      attempts: options.attempts,
      delayMs: options.delayMs,
      delay: options.delay,
      fetchImpl: options.fetchImpl,
    });

  const archivePath = resolved.archive_path;
  const verified = await ensureVerifiedArchive({
    archivePath,
    expectedChecksum,
    sourceUrl,
    offline: options.offline,
    attempts: options.attempts,
    delayMs: options.delayMs,
    delay: options.delay,
    fetchImpl: options.fetchImpl,
  });
  const installed = await installVerifiedArchive({
    archivePath,
    installRoot: resolved.install_dir,
    version: resolved.version,
    target: resolved.target,
    checksum: expectedChecksum,
    source: sourceUrl,
    lockTimeoutMs: options.installLockTimeoutMs,
  });
  return {
    schema_version: RECEIPT_SCHEMA,
    version: resolved.version,
    target: resolved.target,
    binary_path: installed.binaryPath,
    source: sourceUrl,
    checksum: expectedChecksum,
    cache_hit: verified.cacheHit,
    binary_sha256: installed.binaryChecksum,
    checksum_source: checksumSource,
  };
}

function usage() {
  return `Usage:
  node scripts/bootstrap_harn.mjs [install] (--version X.Y.Z | --version-file PATH) [options]
  node scripts/bootstrap_harn.mjs resolve (--version X.Y.Z | --version-file PATH) [options]

Options:
  --cache-dir PATH             Archive and metadata cache root
  --install-dir PATH           Directory that will contain harn[.exe]
  --target TRIPLE              Override detected release target
  --repository OWNER/REPO      Release repository (default: burin-labs/harn)
  --offline                    Require checksum metadata and archive in cache
  --max-attempts N             Bounded download attempts (default: 12)
  --retry-delay-seconds N      Delay between attempts (default: 10)
  --github-output PATH         Also write GitHub Actions step outputs
  --github-path PATH           Append the installed binary directory
  --github-summary PATH        Append an installation summary
  --help                       Show this help
`;
}

function parseInteger(value, name, { allowZero = false } = {}) {
  const parsed = Number.parseInt(String(value), 10);
  const minimum = allowZero ? 0 : 1;
  if (
    !Number.isSafeInteger(parsed) ||
    parsed < minimum ||
    String(parsed) !== String(value)
  ) {
    throw new Error(`${name} must be an integer >= ${minimum}`);
  }
  return parsed;
}

export function parseCliArgs(argv) {
  const args = [...argv];
  let mode = "install";
  if (args[0] && !args[0].startsWith("-")) {
    mode = args.shift();
  }
  if (!["install", "resolve"].includes(mode)) {
    throw new Error(`unknown bootstrap command: ${mode}`);
  }
  const options = { mode, offline: false, help: false };
  const keys = new Map([
    ["--version", "version"],
    ["--version-file", "versionFile"],
    ["--cache-dir", "cacheDir"],
    ["--install-dir", "installDir"],
    ["--target", "target"],
    ["--repository", "repository"],
    ["--max-attempts", "attempts"],
    ["--retry-delay-seconds", "retryDelaySeconds"],
    ["--github-output", "githubOutput"],
    ["--github-path", "githubPath"],
    ["--github-summary", "githubSummary"],
  ]);
  while (args.length) {
    const argument = args.shift();
    if (argument === "--offline") {
      options.offline = true;
    } else if (argument === "--help" || argument === "-h") {
      options.help = true;
    } else if (keys.has(argument)) {
      if (!args.length) throw new Error(`${argument} requires a value`);
      options[keys.get(argument)] = args.shift();
    } else {
      throw new Error(`unknown bootstrap option: ${argument}`);
    }
  }
  return options;
}

function appendOutput(filePath, name, value) {
  const text = String(value);
  if (/[\r\n]/.test(text)) {
    throw new Error(`GitHub output ${name} contains a newline`);
  }
  fs.appendFileSync(filePath, `${name}=${text}${os.EOL}`);
}

const OUTPUT_DELIMITER = "harn-bootstrap-output";

// Write a multi-line GitHub Actions output using the heredoc form.
// `actions/cache` takes a newline-separated `path`, which the single-line
// `name=value` form cannot carry.
function appendMultilineOutput(filePath, name, lines) {
  for (const line of lines) {
    if (line.includes(OUTPUT_DELIMITER)) {
      throw new Error(`GitHub output ${name} contains the heredoc delimiter`);
    }
    if (/[\r\n]/.test(line)) {
      throw new Error(`GitHub output ${name} entry contains a newline`);
    }
  }
  const body = lines.join(os.EOL);
  fs.appendFileSync(
    filePath,
    `${name}<<${OUTPUT_DELIMITER}${os.EOL}${body}${os.EOL}${OUTPUT_DELIMITER}${os.EOL}`,
  );
}

function writeActionOutputs(filePath, result) {
  appendOutput(filePath, "version", result.version);
  appendOutput(filePath, "target", result.target);
  if (result.asset) appendOutput(filePath, "asset", result.asset);
  if (result.cache_dir) appendOutput(filePath, "cache-dir", result.cache_dir);
  if (result.metadata_path && result.archive_path) {
    // Exactly the immutable files this version/asset addresses. Caching
    // `cache_dir` instead keys a version-specific entry to a directory that
    // accumulates every version and target a runner has ever bootstrapped.
    appendMultilineOutput(filePath, "cache-path", [
      result.metadata_path,
      result.archive_path,
    ]);
  }
  if (result.install_dir)
    appendOutput(filePath, "install-dir", result.install_dir);
  if (result.binary_path) appendOutput(filePath, "path", result.binary_path);
  if (result.checksum) appendOutput(filePath, "checksum", result.checksum);
  if (result.source) appendOutput(filePath, "source-url", result.source);
  if (typeof result.cache_hit === "boolean") {
    appendOutput(filePath, "cache-hit", result.cache_hit);
  }
  if (result.schema_version) {
    appendOutput(filePath, "receipt", JSON.stringify(result));
  }
}

export async function main(
  argv = process.argv.slice(2),
  environment = process.env,
) {
  const cli = parseCliArgs(argv);
  if (cli.help) {
    process.stdout.write(usage());
    return null;
  }
  const environmentVersion = environment.HARN_BOOTSTRAP_VERSION?.trim();
  const environmentVersionFile =
    environment.HARN_BOOTSTRAP_VERSION_FILE?.trim();
  const versionOptions = {
    version: cli.version ?? environmentVersion,
    versionFile:
      cli.versionFile ??
      (cli.version || environmentVersion
        ? undefined
        : environmentVersionFile || ".harn-version"),
  };
  const options = {
    ...versionOptions,
    target:
      cli.target ?? (environment.HARN_BOOTSTRAP_TARGET?.trim() || undefined),
    repository:
      cli.repository ??
      (environment.HARN_BOOTSTRAP_REPOSITORY?.trim() || undefined),
    token: environment.HARN_BOOTSTRAP_TOKEN?.trim() || undefined,
    cacheDir:
      cli.cacheDir ??
      (environment.HARN_BOOTSTRAP_CACHE_DIR?.trim() || undefined),
    installDir:
      cli.installDir ??
      (environment.HARN_BOOTSTRAP_INSTALL_DIR?.trim() || undefined),
    offline:
      cli.offline ||
      ["1", "true"].includes(
        String(environment.HARN_BOOTSTRAP_OFFLINE ?? "").toLowerCase(),
      ),
    attempts:
      cli.attempts === undefined &&
      !environment.HARN_BOOTSTRAP_MAX_ATTEMPTS?.trim()
        ? 12
        : parseInteger(
            cli.attempts ?? environment.HARN_BOOTSTRAP_MAX_ATTEMPTS,
            "--max-attempts",
          ),
    delayMs:
      (cli.retryDelaySeconds === undefined &&
      !environment.HARN_BOOTSTRAP_RETRY_DELAY_SECONDS?.trim()
        ? 10
        : parseInteger(
            cli.retryDelaySeconds ??
              environment.HARN_BOOTSTRAP_RETRY_DELAY_SECONDS,
            "--retry-delay-seconds",
            { allowZero: true },
          )) * 1000,
    env: environment,
  };
  const result =
    cli.mode === "resolve"
      ? resolveBootstrap(options)
      : await bootstrap(options);
  process.stdout.write(`${JSON.stringify(result)}${os.EOL}`);
  if (cli.githubOutput) writeActionOutputs(cli.githubOutput, result);
  if (cli.mode === "install" && cli.githubPath) {
    fs.appendFileSync(
      cli.githubPath,
      `${path.dirname(result.binary_path)}${os.EOL}`,
    );
  }
  if (cli.mode === "install" && cli.githubSummary) {
    fs.appendFileSync(
      cli.githubSummary,
      `Installed Harn ${result.version} for ${result.target} from a checksum-verified release archive.${os.EOL}`,
    );
  }
  return result;
}

if (
  process.argv[1] &&
  fileURLToPath(import.meta.url) === path.resolve(process.argv[1])
) {
  main().catch((error) => {
    process.stderr.write(`harn-bootstrap: ${error.message}${os.EOL}`);
    process.exitCode = 1;
  });
}
