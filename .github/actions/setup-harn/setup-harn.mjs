import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const SEMVER = /^(?:v)?(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

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
  if (platform === "windows" && normalizedArch === "x86_64")
    return "x86_64-pc-windows-msvc";
  throw new Error(`unsupported Harn release target: ${runnerOs}/${runnerArch}`);
}

export function assetForTarget(target) {
  return `harn-${target}.${target.endsWith("windows-msvc") ? "zip" : "tar.gz"}`;
}

export function parseChecksums(text) {
  const checksums = new Map();
  for (const line of String(text).split(/\r?\n/)) {
    const match = /^([0-9a-fA-F]{64})\s+\*?(.+)$/.exec(line.trim());
    if (match) checksums.set(match[2], match[1].toLowerCase());
  }
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
        "Expand-Archive -LiteralPath $env:HARN_SETUP_ARCHIVE_PATH -DestinationPath $env:HARN_SETUP_EXTRACT_PATH -Force",
      ],
      env: {
        HARN_SETUP_ARCHIVE_PATH: archivePath,
        HARN_SETUP_EXTRACT_PATH: directory,
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
        headers: { "user-agent": "burin-labs/setup-harn" },
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

export async function ensureVerifiedArchive(options) {
  const { archivePath, expectedChecksum, sourceUrl } = options;
  if (fs.existsSync(archivePath)) {
    const cached = fs.readFileSync(archivePath);
    if (sha256(cached) === expectedChecksum)
      return { bytes: cached, cacheHit: true };
    fs.rmSync(archivePath, { force: true });
  }
  const bytes = await fetchWithRetry(sourceUrl, options);
  const actual = sha256(bytes);
  if (actual !== expectedChecksum) {
    throw new Error(
      `checksum mismatch for ${path.basename(archivePath)}: expected ${expectedChecksum}, got ${actual}`,
    );
  }
  fs.mkdirSync(path.dirname(archivePath), { recursive: true });
  const temporary = `${archivePath}.${process.pid}.tmp`;
  fs.writeFileSync(temporary, bytes, { flag: "wx" });
  fs.renameSync(temporary, archivePath);
  return { bytes, cacheHit: false };
}

function output(name, value) {
  const target = process.env.GITHUB_OUTPUT;
  if (target) fs.appendFileSync(target, `${name}=${value}${os.EOL}`);
  else process.stdout.write(`${name}=${value}${os.EOL}`);
}

function positiveInteger(value, name) {
  const parsed = Number.parseInt(String(value), 10);
  if (!Number.isSafeInteger(parsed) || parsed < 1)
    throw new Error(`${name} must be a positive integer`);
  return parsed;
}

function resolveVersion() {
  const explicit = process.env.HARN_SETUP_VERSION?.trim();
  if (explicit) return normalizeVersion(explicit);
  const workspace = process.env.GITHUB_WORKSPACE || process.cwd();
  const requested = process.env.HARN_SETUP_VERSION_FILE || ".harn-version";
  const versionFile = path.resolve(workspace, requested);
  const relative = path.relative(workspace, versionFile);
  if (relative.startsWith("..") || path.isAbsolute(relative)) {
    throw new Error(
      `version-file must stay inside GITHUB_WORKSPACE: ${requested}`,
    );
  }
  return normalizeVersion(fs.readFileSync(versionFile, "utf8"));
}

function findBinary(root, binaryName) {
  const pending = [{ directory: root, depth: 0 }];
  while (pending.length) {
    const { directory, depth } = pending.shift();
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const candidate = path.join(directory, entry.name);
      if (entry.isFile() && entry.name === binaryName) return candidate;
      if (entry.isDirectory() && depth < 2)
        pending.push({ directory: candidate, depth: depth + 1 });
    }
  }
  throw new Error(
    `${binaryName} was not present in the verified release archive`,
  );
}

function extractVerifiedArchive(archivePath, installRoot, target) {
  fs.mkdirSync(path.dirname(installRoot), { recursive: true });
  const temporary = fs.mkdtempSync(
    path.join(path.dirname(installRoot), ".install-"),
  );
  try {
    const extraction = extractionCommand(archivePath, temporary, target);
    const extracted = spawnSync(extraction.command, extraction.args, {
      cwd: temporary,
      encoding: "utf8",
      env: { ...process.env, ...extraction.env },
    });
    if (extracted.status !== 0)
      throw new Error(
        `archive extraction failed: ${extracted.stderr || extracted.stdout}`,
      );
    const binaryName = target.endsWith("windows-msvc") ? "harn.exe" : "harn";
    const found = findBinary(temporary, binaryName);
    const destination = path.join(temporary, binaryName);
    if (found !== destination) fs.renameSync(found, destination);
    if (binaryName === "harn") fs.chmodSync(destination, 0o755);
    fs.rmSync(installRoot, { recursive: true, force: true });
    fs.renameSync(temporary, installRoot);
    return path.join(installRoot, binaryName);
  } catch (error) {
    fs.rmSync(temporary, { recursive: true, force: true });
    throw error;
  }
}

async function resolve() {
  const version = resolveVersion();
  const target = releaseTarget(
    process.env.RUNNER_OS || os.platform(),
    process.env.RUNNER_ARCH || os.arch(),
  );
  output("version", version);
  output("target", target);
  output("asset", assetForTarget(target));
}

async function install() {
  const version = normalizeVersion(process.env.HARN_SETUP_VERSION);
  const repository = process.env.HARN_SETUP_REPOSITORY || "burin-labs/harn";
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository))
    throw new Error(`invalid repository: ${repository}`);
  const target = process.env.HARN_SETUP_TARGET;
  const asset = process.env.HARN_SETUP_ASSET;
  if (asset !== assetForTarget(target))
    throw new Error(`asset does not match target ${target}: ${asset}`);
  const attempts = positiveInteger(
    process.env.HARN_SETUP_MAX_ATTEMPTS || "12",
    "max-attempts",
  );
  const delaySeconds = positiveInteger(
    process.env.HARN_SETUP_RETRY_DELAY_SECONDS || "10",
    "retry-delay-seconds",
  );
  const releaseRoot = `https://github.com/${repository}/releases/download/v${version}`;
  const checksumUrl = `${releaseRoot}/SHA256SUMS`;
  const sourceUrl = `${releaseRoot}/${asset}`;
  const checksums = parseChecksums(
    (
      await fetchWithRetry(checksumUrl, {
        attempts,
        delayMs: delaySeconds * 1000,
      })
    ).toString("utf8"),
  );
  const expectedChecksum = checksums.get(asset);
  if (!expectedChecksum)
    throw new Error(`SHA256SUMS does not contain ${asset}`);

  const toolCache =
    process.env.RUNNER_TOOL_CACHE || path.join(os.tmpdir(), "harn-tool-cache");
  const archivePath = path.join(
    toolCache,
    "harn-setup",
    "downloads",
    version,
    asset,
  );
  const verified = await ensureVerifiedArchive({
    archivePath,
    expectedChecksum,
    sourceUrl,
    attempts,
    delayMs: delaySeconds * 1000,
  });
  const installRoot = path.join(toolCache, "harn", version, target);
  const binaryPath = extractVerifiedArchive(archivePath, installRoot, target);
  const receipt = {
    version,
    target,
    asset,
    archive_sha256: expectedChecksum,
    binary_sha256: sha256(fs.readFileSync(binaryPath)),
    source_url: sourceUrl,
  };
  fs.writeFileSync(
    path.join(installRoot, "setup-receipt.json"),
    `${JSON.stringify(receipt, null, 2)}\n`,
  );
  if (process.env.GITHUB_PATH)
    fs.appendFileSync(process.env.GITHUB_PATH, `${installRoot}${os.EOL}`);
  if (process.env.GITHUB_STEP_SUMMARY) {
    fs.appendFileSync(
      process.env.GITHUB_STEP_SUMMARY,
      `Installed Harn ${version} for ${target} from a checksum-verified release archive.\n`,
    );
  }
  output("version", version);
  output("path", binaryPath);
  output("checksum", expectedChecksum);
  output("source-url", sourceUrl);
  output("cache-hit", String(verified.cacheHit));
}

async function main() {
  const mode = process.env.HARN_SETUP_MODE;
  if (mode === "resolve") await resolve();
  else if (mode === "install") await install();
  else throw new Error(`unknown setup-harn mode: ${mode}`);
}

if (
  process.argv[1] &&
  fileURLToPath(import.meta.url) === path.resolve(process.argv[1])
) {
  main().catch((error) => {
    process.stderr.write(`setup-harn: ${error.message}\n`);
    process.exitCode = 1;
  });
}
