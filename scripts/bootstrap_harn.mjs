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

export async function fetchWithRetry(url, options = {}) {
  const fetchImpl = options.fetchImpl ?? globalThis.fetch;
  const attempts = options.attempts ?? 12;
  const delayMs = options.delayMs ?? 10_000;
  const delay =
    options.delay ??
    ((ms) => new Promise((resolve) => setTimeout(resolve, ms)));
  let lastError;
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      const response = await fetchImpl(url, {
        headers: { "user-agent": "burin-labs/harn-bootstrap" },
        redirect: "follow",
        signal: AbortSignal.timeout(30_000),
      });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      return Buffer.from(await response.arrayBuffer());
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

async function checksumManifest(options) {
  const { metadataPath, checksumUrl } = options;
  if (options.offline) {
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

  const downloaded = await fetchWithRetry(checksumUrl, options);
  parseChecksums(downloaded.toString("utf8"));
  if (fs.existsSync(metadataPath)) {
    const cached = fs.readFileSync(metadataPath);
    if (!cached.equals(downloaded)) {
      throw new Error(
        `published SHA256SUMS changed for an exact release: ${metadataPath}`,
      );
    }
    return downloaded;
  }
  publishFile(metadataPath, downloaded);
  const published = fs.readFileSync(metadataPath);
  if (!published.equals(downloaded)) {
    throw new Error(
      `concurrent checksum metadata publication disagreed for ${metadataPath}`,
    );
  }
  return published;
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
      manifest.binary_sha256 !== sha256(fs.readFileSync(binaryPath))
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
  const metadataPath = path.join(
    resolved.cache_dir,
    "metadata",
    resolved.version,
    "SHA256SUMS",
  );
  const manifest = await checksumManifest({
    metadataPath,
    checksumUrl,
    offline: options.offline,
    attempts: options.attempts,
    delayMs: options.delayMs,
    delay: options.delay,
    fetchImpl: options.fetchImpl,
  });
  const checksums = parseChecksums(manifest.toString("utf8"));
  const expectedChecksum = checksums.get(resolved.asset);
  if (!expectedChecksum) {
    throw new Error(`SHA256SUMS does not contain ${resolved.asset}`);
  }

  const archivePath = path.join(
    resolved.cache_dir,
    "downloads",
    resolved.version,
    resolved.asset,
  );
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

function writeActionOutputs(filePath, result) {
  appendOutput(filePath, "version", result.version);
  appendOutput(filePath, "target", result.target);
  if (result.asset) appendOutput(filePath, "asset", result.asset);
  if (result.cache_dir) appendOutput(filePath, "cache-dir", result.cache_dir);
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
