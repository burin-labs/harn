import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  repairPolicy,
  reserveAndDispatchRepair,
} from "./lib/development_cutover_repair.mjs";

const root = dirname(dirname(fileURLToPath(import.meta.url)));

function github(method, endpoint, body) {
  const args = [
    "api",
    endpoint,
    "--method",
    method,
    "-H",
    "X-GitHub-Api-Version: 2026-03-10",
  ];
  if (body) args.push("--input", "-");
  try {
    return JSON.parse(
      execFileSync("gh", args, {
        input: body ? JSON.stringify(body) : undefined,
        encoding: "utf8",
        timeout: 30_000,
        stdio: ["pipe", "pipe", "pipe"],
      }),
    );
  } catch {
    throw new Error(
      `Could not ${method} ${endpoint}; automatic repair state is unknown`,
    );
  }
}

function publicationPlan(policy) {
  const temporary = mkdtempSync(join(tmpdir(), "harn-cutover-repair-"));
  try {
    const output = join(temporary, "plan");
    execFileSync("bash", [join(root, "scripts/plan_development_bump.sh")], {
      env: {
        ...process.env,
        GITHUB_OUTPUT: output,
        GITHUB_REPOSITORY: policy.repository,
        PUBLISHED_TAG: process.env.MEASURED_PUBLISHED_TAG,
      },
      stdio: ["ignore", "pipe", "pipe"],
      timeout: 45_000,
    });
    const fields = {};
    for (const line of readFileSync(output, "utf8").trim().split("\n")) {
      const separator = line.indexOf("=");
      const key = line.slice(0, separator);
      if (separator < 1 || Object.hasOwn(fields, key))
        throw new Error("Malformed publication plan");
      fields[key] = line.slice(separator + 1);
    }
    if (
      Object.keys(fields).sort().join(",") !==
        "published_tag,reason,required,version" ||
      !["true", "false"].includes(fields.required) ||
      !fields.reason ||
      fields.published_tag !== process.env.MEASURED_PUBLISHED_TAG
    ) {
      throw new Error("Incomplete publication plan");
    }
    return { ...fields, required: fields.required === "true" };
  } finally {
    // This exact OS temporary root belongs to this invocation and contains
    // only its nonsecret bootstrap projection, never user-authored state.
    rmSync(temporary, { recursive: true, force: true });
  }
}

function main() {
  const policy = repairPolicy(
    JSON.parse(
      readFileSync(
        join(root, "scripts/development_cutover_repair.json"),
        "utf8",
      ),
    ),
  );
  if (
    process.env.GH_REPO !== policy.repository ||
    process.env.MEASURED_REPAIR_OWED !== "true" ||
    !process.env.MEASURED_DEVELOPMENT_VERSION ||
    !process.env.MEASURED_PUBLISHED_TAG
  ) {
    throw new Error(
      "Automatic repair requires the owning controller's measured debt",
    );
  }
  const plan = publicationPlan(policy);
  if (!plan.required) {
    console.log(
      JSON.stringify({
        state: "not_owed",
        reason: plan.reason,
        dispatched: false,
      }),
    );
    return;
  }
  if (plan.version !== process.env.MEASURED_DEVELOPMENT_VERSION) {
    throw new Error("Publication plan and measured development debt disagree");
  }
  // Re-read the exact branch immediately before reserving. An existing red PR
  // remains its owner's work, never a reason to retry its CI or replace it.
  const branch = `automation/development-${plan.version}`;
  const response = github(
    "GET",
    `repos/${policy.repository}/pulls?state=open&head=${policy.repository.split("/")[0]}:${branch}&per_page=100`,
  );
  if (
    !Array.isArray(response) ||
    response.some(
      (pr) =>
        pr?.state !== "open" ||
        pr.head?.ref !== branch ||
        !Number.isSafeInteger(pr.number) ||
        pr.number < 1,
    )
  ) {
    throw new Error("Existing development PR census is incomplete");
  }
  if (response.length > 0) {
    console.log(
      JSON.stringify({
        state: "existing_pr",
        openCount: response.length,
        pullRequests: response.map((pr) => pr.number),
        dispatched: false,
      }),
    );
    return;
  }
  console.log(
    JSON.stringify(
      reserveAndDispatchRepair(
        policy,
        plan,
        Number(process.env.GITHUB_RUN_ID),
        github,
      ),
    ),
  );
}

try {
  main();
} catch (error) {
  console.error(`Development repair refused: ${error.message}`);
  process.exitCode = 1;
}
