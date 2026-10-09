// CacheConfig remains the pinned action's owner. This projection exports only
// its resolver; it never invokes that action's restore or save entrypoint.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const crypto = require("node:crypto");
const { execFileSync } = require("node:child_process");
const pin = require("../../.github/actions/rust-cache/pinned-config.json");
const families = ["workspace-tests", "harn-ci-cli", "package-audit"];
// These files own the producer's toolchain, action inputs and post lifecycle.
// Native source equality is delegated to reuse_workspace_crates.sh below.
const producerConfigurationPaths = [
  ".github/workflows/rust-cache-refresh.yml",
  ".github/actions",
  "rust-toolchain.toml",
  ".cargo/config.toml",
  // Producer scripts invoke audit planning, shell libraries and nested CI
  // helpers. Preserve this whole owning recipe subtree rather than maintain
  // an incomplete transitive file list. Unrelated script edits conservatively
  // require the next producer; catalog-only descendants can still qualify.
  "scripts",
];
const inputNames = [
  "shared-key",
  "cache-on-failure",
  "cache-bin",
  "cache-workspace-crates",
  "cache-targets",
  "cache-directories",
  "workspaces",
  "save-if",
];
function requireValue(condition, message) {
  if (!condition) throw Error(message);
}
function verifiedProjection(bundle) {
  requireValue(
    bundle.length === pin.bundle_size,
    "Pinned cache bundle size changed",
  );
  requireValue(
    crypto.createHash("sha256").update(bundle).digest("hex") ===
      pin.bundle_sha256,
    "Pinned cache bundle digest changed",
  );
  requireValue(
    crypto
      .createHash("sha1")
      .update(`blob ${bundle.length}\0`)
      .update(bundle)
      .digest("hex") === pin.bundle_git_blob,
    "Pinned cache bundle identity changed",
  );
  const text = bundle.toString("utf8");
  const epilogue = "\nrun();\n\n})();";
  requireValue(
    text.split(epilogue).length === 2,
    "Pinned configuration epilogue is ambiguous",
  );
  return text.replace(
    epilogue,
    "\n__webpack_exports__.resolveConfig = () => CacheConfig.new();\n\n})();",
  );
}
function cacheCensus(pages) {
  requireValue(
    Array.isArray(pages) && pages.length > 0,
    "Cache inventory is unmeasured",
  );
  const total = pages[0].total_count;
  requireValue(
    Number.isSafeInteger(total) && total >= 0,
    "Cache inventory count is unreported",
  );
  requireValue(
    pages.every(
      (p) => p && p.total_count === total && Array.isArray(p.actions_caches),
    ),
    "Cache inventory pages are incomplete",
  );
  const caches = pages.flatMap((p) => p.actions_caches);
  requireValue(caches.length === total, "Cache inventory is partial");
  const ids = new Set();
  for (const cache of caches) {
    requireValue(
      Number.isSafeInteger(cache.id) && cache.id > 0 && !ids.has(cache.id),
      "Cache inventory IDs are invalid or duplicated",
    );
    ids.add(cache.id);
    requireValue(
      typeof cache.key === "string" &&
        cache.key.length > 0 &&
        typeof cache.ref === "string" &&
        cache.ref.length > 0,
      "Cache inventory identity is invalid",
    );
    requireValue(
      Number.isSafeInteger(cache.size_in_bytes) && cache.size_in_bytes >= 0,
      "Cache inventory size is unmeasured",
    );
  }
  return caches;
}
function exactCache(caches, key) {
  const matches = caches.filter(
    (c) => c.ref === "refs/heads/main" && c.key === key,
  );
  requireValue(matches.length <= 1, "Exact cache generation is ambiguous");
  if (matches.length === 1)
    requireValue(
      matches[0].size_in_bytes > 0,
      "Exact cache generation is empty",
    );
  return matches[0] ?? null;
}
function qualify(context, observation, sourceCurrent, caches) {
  const current = exactCache(caches, context.cache_key);
  requireValue(current !== null, "Exact cache upload is unmeasured");
  if (context.initial_cache_id !== null) {
    requireValue(
      observation.cache_hit === true && observation.source_current === true,
      "Existing immutable cache lacks a measured current-source restore",
    );
    requireValue(
      current.id === context.initial_cache_id,
      "Restored cache identity changed",
    );
  } else {
    requireValue(
      sourceCurrent === true,
      "Uploaded cache lacks current-source verification",
    );
    requireValue(
      current.id !== context.initial_cache_id,
      "Cache upload did not commit a new generation",
    );
  }
  return {
    ...context,
    schema: "harn.qualified_rust_cache_generation.v1",
    cache_id: current.id,
    cache_bytes: current.size_in_bytes,
  };
}
function retirementPlan(identity, caches) {
  requireValue(
    identity.schema === "harn.qualified_rust_cache_generation.v1" &&
      families.includes(identity.family),
    "Cache generation qualification is invalid",
  );
  requireValue(
    identity.repository === "burin-labs/harn" &&
      identity.ref === "refs/heads/main" &&
      /^[0-9a-f]{40}$/.test(identity.source_sha),
    "Cache producer identity is invalid",
  );
  const prefix = `v0-rust-${identity.family}-workspace-crates-v3-Linux-x64-`;
  requireValue(
    new RegExp(`^${prefix}[0-9a-f]{8}$`).test(identity.restore_key),
    "Cache environment identity is invalid",
  );
  requireValue(
    new RegExp(`^${identity.restore_key}-[0-9a-f]{8}$`).test(
      identity.cache_key,
    ),
    "Cache source identity is invalid",
  );
  requireValue(
    identity.pinned_action === pin.commit,
    "Cache configuration authority changed",
  );
  const current = exactCache(caches, identity.cache_key);
  requireValue(
    current &&
      current.id === identity.cache_id &&
      current.size_in_bytes === identity.cache_bytes,
    "Qualified cache readback changed",
  );
  requireValue(
    typeof current.created_at === "string" &&
      Number.isFinite(Date.parse(current.created_at)),
    "Qualified cache creation is unmeasured",
  );
  const compatible = new RegExp(`^${identity.restore_key}-[0-9a-f]{8}$`);
  const deleted = caches.filter(
    (c) =>
      c.ref === identity.ref && c.id !== current.id && compatible.test(c.key),
  );
  for (const cache of deleted)
    requireValue(
      Number.isFinite(Date.parse(cache.created_at)) &&
        Date.parse(cache.created_at) < Date.parse(current.created_at),
      "Superseded generation order is unmeasured",
    );
  return {
    schema: "harn.qualified_cache_retirement.v1",
    qualified: identity,
    observed: caches.length,
    pending: 0,
    bad: 0,
    deleted,
  };
}
function apiCensus() {
  const pages = JSON.parse(
    execFileSync(
      "gh",
      [
        "api",
        "--paginate",
        "--slurp",
        "repos/burin-labs/harn/actions/caches?per_page=100",
      ],
      { encoding: "utf8", timeout: 30000, maxBuffer: 8 * 1024 * 1024 },
    ),
  );
  return cacheCensus(pages);
}
function output(file, name, value) {
  requireValue(
    typeof file === "string" && file.length > 0,
    "Action command file is unavailable",
  );
  fs.appendFileSync(file, `${name}=${value}\n`);
}
function cleanupScratch(scratch) {
  requireValue(
    path.basename(scratch).startsWith("rust-cache-generation-") &&
      path.isAbsolute(scratch),
    "Generation scratch cleanup path is invalid",
  );
  fs.rmSync(scratch, { recursive: true, force: true });
}
async function start() {
  requireValue(
    process.env.GITHUB_REPOSITORY === "burin-labs/harn" &&
      process.env.GITHUB_REF === "refs/heads/main",
    "Generation writer must run on owning main",
  );
  const inputs = { ...pin.defaults };
  for (const name of inputNames) {
    const value = process.env[`INPUT_${name.toUpperCase()}`];
    if (value !== undefined && value !== "") inputs[name] = value;
  }
  const family = inputs["shared-key"]?.replace(/-workspace-crates-v3$/, "");
  requireValue(
    families.includes(family) &&
      inputs["shared-key"] === `${family}-workspace-crates-v3`,
    "Generation family is not owned",
  );
  requireValue(
    inputs["save-if"] === "true" && inputs["cache-workspace-crates"] === "true",
    "Generation writer is not authorized",
  );
  const scratch = fs.mkdtempSync(
    path.join(process.env.RUNNER_TEMP || os.tmpdir(), "rust-cache-generation-"),
  );
  let registered = false;
  try {
    const response = await fetch(
      `https://raw.githubusercontent.com/Swatinem/rust-cache/${pin.commit}/dist/restore/index.js`,
    );
    requireValue(response.ok, "Pinned cache configuration could not be read");
    const bundle = Buffer.from(await response.arrayBuffer());
    const projection = path.join(scratch, "config.cjs");
    fs.writeFileSync(projection, verifiedProjection(bundle), { mode: 0o600 });
    for (const [name, value] of Object.entries(inputs))
      process.env[`INPUT_${name.toUpperCase()}`] = value;
    const config = await require(projection).resolveConfig();
    const caches = apiCensus();
    const initial = exactCache(caches, config.cacheKey);
    const source = execFileSync("git", ["rev-parse", "--verify", "HEAD"], {
      encoding: "utf8",
    }).trim();
    requireValue(
      source === process.env.GITHUB_SHA && /^[0-9a-f]{40}$/.test(source),
      "Generation source differs from the workflow",
    );
    const context = {
      family,
      repository: process.env.GITHUB_REPOSITORY,
      ref: process.env.GITHUB_REF,
      source_sha: source,
      pinned_action: pin.commit,
      cache_key: config.cacheKey,
      restore_key: config.restoreKey,
      initial_cache_id: initial?.id ?? null,
      scratch,
    };
    const observationFile = path.join(scratch, "restore-observation.json");
    fs.writeFileSync(
      observationFile,
      JSON.stringify({ cache_hit: false, source_current: false }),
      { mode: 0o600 },
    );
    output(process.env.GITHUB_STATE, "generation", JSON.stringify(context));
    output(process.env.GITHUB_OUTPUT, "observation-file", observationFile);
    output(process.env.GITHUB_OUTPUT, "cache-key", config.cacheKey);
    console.log(
      JSON.stringify({
        schema: "harn.rust_cache_identity.v1",
        family,
        source_sha: source,
        cache_key: config.cacheKey,
        restore_key: config.restoreKey,
        initial_cache_id: context.initial_cache_id,
      }),
    );
    registered = true;
  } finally {
    if (!registered) cleanupScratch(scratch);
  }
}
function observe() {
  requireValue(
    ["true", "false"].includes(process.env.CACHE_HIT) &&
      ["true", "false"].includes(process.env.SOURCE_CURRENT),
    "Restore observation is unreported",
  );
  const file = process.env.CACHE_OBSERVATION_FILE;
  requireValue(
    typeof file === "string" &&
      path.basename(file) === "restore-observation.json",
    "Restore observation path is invalid",
  );
  fs.writeFileSync(
    file,
    JSON.stringify({
      cache_hit: process.env.CACHE_HIT === "true",
      source_current: process.env.SOURCE_CURRENT === "true",
    }),
    { mode: 0o600 },
  );
}
async function finish() {
  const context = JSON.parse(process.env.STATE_generation ?? "null");
  requireValue(
    context &&
      context.source_sha === process.env.GITHUB_SHA &&
      context.ref === process.env.GITHUB_REF,
    "Generation action state is invalid",
  );
  try {
    requireValue(
      execFileSync("git", ["rev-parse", "--verify", "HEAD"], {
        encoding: "utf8",
      }).trim() === context.source_sha,
      "Generation checkout changed after admission",
    );
    const main = execFileSync(
      "git",
      ["ls-remote", "--exit-code", "origin", "refs/heads/main"],
      { encoding: "utf8", timeout: 30000 },
    )
      .trim()
      .split(/\s+/);
    requireValue(
      main.length === 2 && main[1] === "refs/heads/main",
      "Owning main source is unmeasured",
    );
    requireValue(/^[0-9a-f]{40}$/.test(main[0]), "Owning main source is invalid");
    if (main[0] !== context.source_sha) {
      execFileSync("git", ["fetch", "--quiet", "--no-tags", "--no-write-fetch-head", "origin", main[0]], {
        timeout: 30000,
        stdio: "pipe",
      });
      execFileSync("git", ["merge-base", "--is-ancestor", context.source_sha, main[0]], {
        stdio: "pipe",
      });
      execFileSync("git", ["diff", "--quiet", context.source_sha, main[0], "--", ...producerConfigurationPaths], {
        stdio: "pipe",
      });
      execFileSync("bash", ["scripts/ci/reuse_workspace_crates.sh", "current", main[0], context.source_sha], {
        stdio: "pipe",
      });
    }
    const observation = JSON.parse(
      fs.readFileSync(
        path.join(context.scratch, "restore-observation.json"),
        "utf8",
      ),
    );
    let sourceCurrent = false;
    if (context.initial_cache_id === null) {
      try {
        execFileSync(
          "bash",
          ["scripts/ci/reuse_workspace_crates.sh", "current"],
          { stdio: "pipe" },
        );
        sourceCurrent = true;
      } catch {
        sourceCurrent = false;
      }
    }
    const caches = apiCensus();
    const identity = qualify(context, observation, sourceCurrent, caches);
    const receipt = path.join(context.scratch, "qualified-generation.json");
    fs.writeFileSync(receipt, JSON.stringify(identity), { mode: 0o600 });
    const plan = execFileSync(
      "bash",
      [
        "scripts/prune_ci_cache_generations.sh",
        "--retain-qualified-generation",
        receipt,
      ],
      { encoding: "utf8", timeout: 60000, maxBuffer: 8 * 1024 * 1024 },
    );
    console.log(plan.trim());
  } finally {
    cleanupScratch(context.scratch);
  }
}
module.exports = {
  verifiedProjection,
  cacheCensus,
  exactCache,
  qualify,
  retirementPlan,
  start,
  observe,
  finish,
  pin,
};
if (require.main === module) {
  if (process.argv[2] === "observe" && process.argv.length === 3) observe();
  else if (process.argv[2] === "validate-census" && process.argv.length === 3) {
    const pages = JSON.parse(fs.readFileSync(0, "utf8"));
    cacheCensus(pages);
    console.log(JSON.stringify(pages));
  } else if (
    process.argv[2] === "plan-qualified-generation" &&
    process.argv.length === 4
  ) {
    const identity = JSON.parse(fs.readFileSync(process.argv[3], "utf8"));
    const caches = cacheCensus(JSON.parse(fs.readFileSync(0, "utf8")));
    console.log(JSON.stringify(retirementPlan(identity, caches)));
  } else throw Error("Unsupported cache generation command");
}
