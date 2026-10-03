/** @typedef {{schema_version: string, repository: string, repository_id: number,
 * anchor: string, seed: {id: number, context: string, target_url: string},
 * context_prefix: string, workflow: string}} RepairPolicy */

function requireCondition(condition, message) {
  if (!condition) throw new Error(message);
}

/** Validate the owning contract once, before any remote effect. */
export function repairPolicy(value) {
  requireCondition(
    value?.schema_version === "harn.development_cutover_repair.v1",
    "Unknown development repair contract",
  );
  requireCondition(
    typeof value.repository === "string" &&
      /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(value.repository) &&
      Number.isSafeInteger(value.repository_id) &&
      value.repository_id > 0,
    "Missing repair repository identity",
  );
  requireCondition(
    typeof value.anchor === "string" && /^[a-f0-9]{40}$/.test(value.anchor),
    "Repair requires a fixed commit anchor",
  );
  requireCondition(
    Number.isSafeInteger(value.seed?.id) &&
      value.seed.id > 0 &&
      typeof value.seed.context === "string" &&
      value.seed.context.length > 0 &&
      typeof value.seed.target_url === "string" &&
      value.seed.target_url.startsWith(
        `https://github.com/${value.repository}/actions/runs/`,
      ),
    "Missing nonempty repair census control",
  );
  requireCondition(
    typeof value.context_prefix === "string" &&
      value.context_prefix.length > 0 &&
      typeof value.workflow === "string" &&
      /^[A-Za-z0-9_-]+\.yml$/.test(value.workflow),
    "Missing owning repair context or workflow",
  );
  return value;
}

/**
 * Read the complete latest-per-context census on the fixed anchor. A context
 * remains present when a later status supersedes it, so any reservation state
 * consumes the automatic attempt. Overall combined success is not consulted.
 * @param {RepairPolicy} policy
 * @param {(method: string, endpoint: string, body?: object) => any} api
 */
export function readRepairCensus(policy, api) {
  const commit = api(
    "GET",
    `repos/${policy.repository}/commits/${policy.anchor}`,
  );
  requireCondition(
    commit?.sha === policy.anchor,
    "Repair anchor is unreadable or changed",
  );
  const rows = new Map();
  const identities = new Set();
  let total;
  for (let page = 1; page <= 100; page += 1) {
    const response = api(
      "GET",
      `repos/${policy.repository}/commits/${policy.anchor}/status?per_page=100&page=${page}`,
    );
    requireCondition(
      response?.sha === policy.anchor &&
        response.repository?.id === policy.repository_id &&
        response.repository?.full_name === policy.repository &&
        Number.isSafeInteger(response.total_count) &&
        response.total_count > 0 &&
        Array.isArray(response.statuses),
      "Incomplete repair status census",
    );
    total ??= response.total_count;
    requireCondition(
      total === response.total_count,
      "Repair status census changed during pagination",
    );
    requireCondition(
      response.statuses.length > 0,
      "Repair status census ended before its measured total",
    );
    for (const row of response.statuses) {
      requireCondition(
        Number.isSafeInteger(row?.id) &&
          row.id > 0 &&
          typeof row.context === "string" &&
          row.context.length > 0 &&
          ["pending", "success", "failure", "error"].includes(row.state),
        "Malformed repair status record",
      );
      requireCondition(
        !rows.has(row.context.toLowerCase()) && !identities.has(row.id),
        "Duplicate repair status page or context",
      );
      rows.set(row.context.toLowerCase(), row);
      identities.add(row.id);
    }
    requireCondition(
      rows.size <= total,
      "Repair status census exceeded its measured total",
    );
    if (rows.size === total) {
      const seed = rows.get(policy.seed.context.toLowerCase());
      requireCondition(
        seed?.id === policy.seed.id &&
          seed.context === policy.seed.context &&
          seed.target_url === policy.seed.target_url,
        "Repair census control is missing or changed",
      );
      return { total, rows };
    }
  }
  throw new Error(
    "Repair status census exceeded its bounded pagination budget",
  );
}

/**
 * Reserve before dispatch. An ambiguous acknowledgment stops here; the next
 * controller observes the reservation and cannot dispatch again. The version
 * was normalized by the existing release planner, not inferred from prose.
 * All callers must share the controller's non-cancelling concurrency group.
 * @param {RepairPolicy} policy
 */
export function reserveAndDispatchRepair(policy, plan, controllerRunId, api) {
  requireCondition(
    plan.required === true &&
      typeof plan.version === "string" &&
      plan.version.length > 0 &&
      typeof plan.published_tag === "string" &&
      plan.published_tag.length > 0,
    "Repair requires a proved publication plan",
  );
  requireCondition(
    Number.isSafeInteger(controllerRunId) && controllerRunId > 0,
    "Repair requires the actual controller run identity",
  );
  const context = policy.context_prefix + plan.version;
  const before = readRepairCensus(policy, api);
  const prior = before.rows.get(context.toLowerCase());
  if (prior) {
    return {
      state: "reserved",
      version: plan.version,
      measuredContexts: before.total,
      reservationId: prior.id,
      dispatched: false,
    };
  }
  const targetUrl = `https://github.com/${policy.repository}/actions/runs/${controllerRunId}`;
  const reservation = api(
    "POST",
    `repos/${policy.repository}/statuses/${policy.anchor}`,
    {
      state: "pending",
      context,
      target_url: targetUrl,
      description:
        "Automatic development repair attempt reserved before dispatch",
    },
  );
  requireCondition(
    Number.isSafeInteger(reservation?.id) &&
      reservation.id > 0 &&
      reservation.context === context &&
      reservation.state === "pending" &&
      reservation.target_url === targetUrl,
    "Repair reservation acknowledgment is ambiguous",
  );
  const after = readRepairCensus(policy, api);
  const observed = after.rows.get(context.toLowerCase());
  requireCondition(
    observed?.id === reservation.id &&
      observed.context === context &&
      observed.state === "pending" &&
      observed.target_url === targetUrl,
    "Repair reservation did not read back with its exact identity",
  );
  const dispatch = api(
    "POST",
    `repos/${policy.repository}/actions/workflows/${policy.workflow}/dispatches`,
    {
      ref: "main",
      inputs: { published_tag: plan.published_tag },
      return_run_details: true,
    },
  );
  requireCondition(
    Number.isSafeInteger(dispatch?.workflow_run_id) &&
      dispatch.workflow_run_id > 0 &&
      dispatch.html_url ===
        `https://github.com/${policy.repository}/actions/runs/${dispatch.workflow_run_id}`,
    "Repair dispatch acknowledgment is ambiguous; reservation remains consumed",
  );
  return {
    state: "dispatched",
    version: plan.version,
    measuredContexts: after.total,
    reservationId: reservation.id,
    dispatched: true,
    runId: dispatch.workflow_run_id,
    runUrl: dispatch.html_url,
  };
}
