import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { IntlProvider } from "react-intl"
import { MemoryRouter } from "react-router-dom"
import { afterEach, describe, expect, it, vi } from "vitest"

import { App } from "./App"

const runsPayload = {
  stats: {
    total_runs: 2,
    completed_runs: 1,
    active_runs: 0,
    failed_runs: 1,
    avg_duration_ms: 1200,
  },
  filtered_count: 2,
  pagination: {
    page: 1,
    page_size: 25,
    total_pages: 1,
    total_runs: 2,
    has_previous: false,
    has_next: false,
  },
  runs: [
    {
      path: "failed.json",
      id: "run-failed",
      workflow_name: "demo",
      status: "failed",
      last_stage_node_id: "verify",
      failure_summary: "verify failed: assertion failed",
      started_at: "2026-04-04T10:00:00Z",
      finished_at: "2026-04-04T10:00:05Z",
      duration_ms: 5000,
      stage_count: 2,
      child_run_count: 0,
      call_count: 3,
      input_tokens: 100,
      output_tokens: 40,
      models: ["gpt-5"],
      updated_at_ms: 1,
      skills: [],
    },
    {
      path: "ok.json",
      id: "run-ok",
      workflow_name: "demo",
      status: "completed",
      last_stage_node_id: "finalize",
      failure_summary: null,
      started_at: "2026-04-04T09:00:00Z",
      finished_at: "2026-04-04T09:00:02Z",
      duration_ms: 2000,
      stage_count: 1,
      child_run_count: 0,
      call_count: 1,
      input_tokens: 20,
      output_tokens: 10,
      models: ["gpt-5"],
      updated_at_ms: 2,
      skills: [],
    },
  ],
}

const detailPayload = {
  summary: runsPayload.runs[0],
  task: "Fix issue",
  workflow_id: "wf",
  parent_run_id: null,
  root_run_id: null,
  policy_summary: {
    tools: [],
    capabilities: [],
    workspace_roots: [],
    side_effect_level: null,
    recursion_limit: null,
    tool_arg_constraints: [],
    validation_valid: true,
    validation_errors: [],
    validation_warnings: [],
    reachable_nodes: ["verify"],
  },
  replay_summary: {
    fixture_id: "fixture-1",
    source_run_id: "run-failed",
    created_at: "2026-04-04T10:01:00Z",
    expected_status: "failed",
    stage_assertions: [],
  },
  execution: null,
  insights: [],
  stages: [],
  spans: [],
  activities: [],
  transitions: [],
  checkpoints: [],
  artifacts: [],
  execution_summary: null,
  transcript_steps: [],
  story: [],
  child_runs: [],
  observability: {
    schema_version: 4,
    planner_rounds: [],
    research_fact_count: 0,
    action_graph_nodes: [],
    action_graph_edges: [],
    worker_lineage: [],
    verification_outcomes: [],
    transcript_pointers: [],
    daemon_events: [
      {
        daemon_id: "daemon-1",
        name: "reviewer",
        kind: "spawned",
        timestamp: "1710000000.100",
        persist_path: "/tmp/reviewer",
        payload_summary: "always-on reviewer",
      },
    ],
  },
  skill_timeline: [],
  skill_match_events: [],
  tool_load_events: [],
  active_skills: [],
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe("App", () => {
  it("shows a paginated runs page and navigates into run detail", async () => {
    const fetchMock = vi.fn(async (input: string) => {
      if (input.startsWith("/api/runs")) {
        return { ok: true, json: async () => runsPayload }
      }
      if (input === "/api/meta") {
        return {
          ok: true,
          json: async () => ({ workspace_root: "/workspace/harn", run_dir: ".harn-runs/portal-demo" }),
        }
      }
      if (input === "/api/llm/options") {
        return {
          ok: true,
          json: async () => ({
            preferred_provider: "local",
            preferred_model: "gpt-4o",
            providers: [
              {
                name: "local",
                base_url: "http://localhost:8000",
                base_url_env: "LOCAL_LLM_BASE_URL",
                auth_style: "none",
                auth_envs: [],
                auth_configured: true,
                viable: true,
                local: true,
                models: ["gpt-4o"],
                aliases: [],
                default_model: "gpt-4o",
              },
            ],
          }),
        }
      }
      if (input.startsWith("/api/run?path=failed.json")) {
        return { ok: true, json: async () => detailPayload }
      }
      if (input === "/api/launch/targets") {
        return { ok: true, json: async () => ({ targets: [] }) }
      }
      if (input === "/api/launch/jobs") {
        return { ok: true, json: async () => ({ jobs: [] }) }
      }
      if (input.startsWith("/api/compare?")) {
        return {
          ok: true,
          json: async () => ({
            identical: false,
            left_status: "completed",
            right_status: "failed",
            stage_diffs: [],
            tool_diffs: [],
            observability_diffs: [],
            transition_count_delta: 1,
            artifact_count_delta: 0,
            checkpoint_count_delta: 0,
          }),
        }
      }
      throw new Error(`unexpected fetch ${input}`)
    })
    vi.stubGlobal("fetch", fetchMock)

    render(
      <MemoryRouter initialEntries={["/runs"]}>
        <IntlProvider locale="en">
          <App />
        </IntlProvider>
      </MemoryRouter>,
    )

    expect(await screen.findByText("Persisted runs")).toBeInTheDocument()
    expect(await screen.findByText("failed.json")).toBeInTheDocument()

    await userEvent.click(screen.getAllByRole("button", { name: "Inspect" })[0])

    await waitFor(() => {
      expect(screen.getByText("Inspect persisted run")).toBeInTheDocument()
    })
    expect(screen.getByText("harn replay .harn-runs/failed.json")).toBeInTheDocument()
    expect(screen.getByText(/reviewer • Spawned • 1710000000.100/)).toBeInTheDocument()
  })
})
