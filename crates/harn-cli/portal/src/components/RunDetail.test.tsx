import { render, screen, within } from "@testing-library/react"
import { IntlProvider } from "react-intl"
import { describe, expect, it } from "vitest"

import type { PortalActivity, PortalRunDetail, RunSummary } from "../types"
import { RunDetail } from "./RunDetail"

const baseSummary: RunSummary = {
  path: "run.json",
  id: "run-1",
  workflow_name: "demo",
  status: "completed",
  last_stage_node_id: "finalize",
  failure_summary: null,
  started_at: "2026-05-16T10:00:00Z",
  finished_at: "2026-05-16T10:00:05Z",
  duration_ms: 5000,
  stage_count: 1,
  child_run_count: 0,
  call_count: 2,
  input_tokens: 10,
  output_tokens: 5,
  models: ["gpt-5"],
  updated_at_ms: 1,
  skills: [],
}

const auditedActivity: PortalActivity = {
  label: "search_files",
  kind: "tool_call",
  started_offset_ms: 0,
  duration_ms: 12,
  stage_node_id: "plan",
  call_id: "call-with-audit",
  summary: "tool call summary",
  audit: {
    reason: "Look up rate limiter",
    kind: "search",
    status: "ok",
    layers: [
      { name: "with_required_reason", status: "ok" },
      { name: "with_consent", status: "approved" },
      { name: "with_audit_log", status: "ok" },
    ],
    receipt_uri: "file:///tmp/.harn/receipts/session.jsonl",
  },
}

const plainActivity: PortalActivity = {
  label: "read_file",
  kind: "tool_call",
  started_offset_ms: 50,
  duration_ms: 5,
  stage_node_id: "plan",
  call_id: "call-no-audit",
  summary: "tool call summary",
}

function buildDetail(activities: PortalActivity[]): PortalRunDetail {
  return {
    summary: baseSummary,
    task: "Demo audit chip rendering",
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
      reachable_nodes: [],
    },
    replay_summary: null,
    execution: null,
    insights: [],
    stages: [],
    spans: [],
    activities,
    transitions: [],
    checkpoints: [],
    artifacts: [],
    execution_summary: null,
    transcript_steps: [],
    template_renders: [],
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
      daemon_events: [],
    },
    skill_timeline: [],
    skill_match_events: [],
    tool_load_events: [],
    active_skills: [],
  }
}

function renderRunDetail(detail: PortalRunDetail) {
  return render(
    <IntlProvider locale="en">
      <RunDetail detail={detail} runs={[baseSummary]} onSelectRun={() => {}} />
    </IntlProvider>,
  )
}

function findActivityRow(label: string) {
  return screen
    .getAllByText(label)
    .map((node) => node.closest(".activity-item"))
    .find((node): node is HTMLElement => node instanceof HTMLElement)
}

describe("RunDetail audit chip", () => {
  it("renders the rationale chip, layer tooltip, and receipt link when audit is present", () => {
    renderRunDetail(buildDetail([auditedActivity]))

    const row = findActivityRow("search_files")
    expect(row).toBeTruthy()
    const scoped = within(row!)

    expect(scoped.getByText("Look up rate limiter")).toBeInTheDocument()
    expect(scoped.getByText(/3 layers/)).toBeInTheDocument()
    expect(
      scoped.getByText(
        "with_required_reason → with_consent → with_audit_log",
      ),
    ).toBeInTheDocument()

    const tooltip = scoped.getByRole("tooltip")
    expect(tooltip).toBeInTheDocument()
    expect(tooltip).toHaveTextContent(
      "with_required_reason → with_consent → with_audit_log",
    )

    const link = scoped.getByRole("link", { name: /view receipt/i })
    expect(link).toHaveAttribute(
      "href",
      "file:///tmp/.harn/receipts/session.jsonl",
    )
  })

  it("renders no audit container when audit is absent", () => {
    const { container } = renderRunDetail(buildDetail([plainActivity]))

    const row = findActivityRow("read_file")
    expect(row).toBeTruthy()
    expect(row!.querySelector(".activity-audit")).toBeNull()
    expect(container.querySelectorAll(".activity-audit")).toHaveLength(0)
  })
})
