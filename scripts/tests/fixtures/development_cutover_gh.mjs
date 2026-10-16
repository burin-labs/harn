import { readFileSync, writeFileSync } from "node:fs";

const file = process.env.REPAIR_TEST_STATE;
const state = JSON.parse(readFileSync(file, "utf8"));
const policy = JSON.parse(readFileSync(process.env.REPAIR_TEST_POLICY, "utf8"));
const args = process.argv.slice(2);
const save = () => writeFileSync(file, JSON.stringify(state));
if (args[0] === "release" && args[1] === "view") {
  console.log(JSON.stringify(state.publication));
} else if (args[0] === "api") {
  const endpoint = args[1];
  const method = args[args.indexOf("--method") + 1];
  const body =
    method === "POST" ? JSON.parse(readFileSync(0, "utf8")) : undefined;
  state.requests.push({ method, endpoint, body });
  save();
  const prefix = `repos/${policy.repository}`;
  if (endpoint === `${prefix}/commits/${policy.anchor}`) {
    if (state.anchorMissing) process.exit(17);
    console.log(JSON.stringify({ sha: policy.anchor }));
  } else if (
    endpoint.startsWith(`${prefix}/commits/${policy.anchor}/status?`)
  ) {
    const page = Number(
      new URL(`https://api.github.com/${endpoint}`).searchParams.get("page"),
    );
    const rows = state.seedMissing
      ? state.statuses.filter((row) => row.id !== policy.seed.id)
      : state.statuses;
    console.log(
      JSON.stringify({
        sha: policy.anchor,
        repository: { id: policy.repository_id, full_name: policy.repository },
        total_count: rows.length + (state.partial ? 1 : 0),
        statuses: rows.slice((page - 1) * 100, page * 100),
      }),
    );
  } else if (endpoint.startsWith(`${prefix}/pulls?`)) {
    console.log(JSON.stringify(state.pullRequests));
  } else if (
    endpoint === `${prefix}/statuses/${policy.anchor}` &&
    method === "POST"
  ) {
    const row = { id: state.nextId++, ...body };
    state.statuses.push(row);
    save();
    if (state.crashAfterReservation) process.exit(19);
    console.log(JSON.stringify(row));
  } else if (
    endpoint === `${prefix}/actions/workflows/${policy.workflow}/dispatches` &&
    method === "POST"
  ) {
    state.dispatches += 1;
    save();
    console.log(
      JSON.stringify(
        state.dispatchUnknownAck
          ? {}
          : {
              workflow_run_id: 70042,
              html_url: `https://github.com/${policy.repository}/actions/runs/70042`,
            },
      ),
    );
  } else {
    throw new Error(`Unexpected fixture request: ${method} ${endpoint}`);
  }
} else {
  throw new Error("Unexpected fixture gh command");
}
