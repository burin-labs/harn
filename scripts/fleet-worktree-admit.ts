#!/usr/bin/env -S node --experimental-strip-types --no-warnings

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, isAbsolute } from "node:path";
import { fileURLToPath } from "node:url";

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const args = process.argv.slice(2);
if (args.some((arg) => arg === "--repository-root" || arg.startsWith("--repository-root="))) {
  console.error(
    "This entry owns --repository-root; run it from the Harn checkout being admitted.",
  );
  process.exit(1);
}
const configured = spawnSync(
  "git",
  ["-C", repositoryRoot, "config", "--local", "--path", "--get", "harn.fleetAdmissionOwner"],
  { encoding: "utf8" },
);
if (configured.status !== 0) {
  console.error(
    configured.stderr || "Configure harn.fleetAdmissionOwner in this repository's local Git config before admission.",
  );
  process.exit(configured.status ?? 1);
}
const owner = configured.stdout.replace(/\r?\n$/, "");
if (!isAbsolute(owner) || !existsSync(owner)) {
  console.error(
    `Canonical fleet admission owner is missing or not absolute at ${owner}. Set harn.fleetAdmissionOwner to the existing owner file's absolute path.`,
  );
  process.exit(1);
}
const result = spawnSync(
  process.execPath,
  ["--experimental-strip-types", "--no-warnings", owner, "--repository-root", repositoryRoot, ...args],
  { stdio: "inherit" },
);
if (result.error) console.error(result.error.message);
process.exit(result.status ?? 1);
