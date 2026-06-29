import { render, screen, within } from "@testing-library/react"
import { describe, expect, it } from "vitest"

import type { PortalActivity } from "../types"
import { RunActivityItem } from "./RunActivityItem"

const labels = {
  auditLayersTooltip: "Middleware layers (outer -> inner)",
  auditReceiptLink: "View receipt",
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

function renderActivity(item: PortalActivity) {
  return render(<RunActivityItem item={item} labels={labels} />)
}

function findActivityRow(label: string) {
  return screen
    .getAllByText(label)
    .map((node) => node.closest(".activity-item"))
    .find((node): node is HTMLElement => node instanceof HTMLElement)
}

describe("RunActivityItem audit chip", () => {
  it("renders the rationale chip, layer tooltip, and receipt link when audit is present", () => {
    renderActivity(auditedActivity)

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
    const { container } = renderActivity(plainActivity)

    const row = findActivityRow("read_file")
    expect(row).toBeTruthy()
    expect(row!.querySelector(".activity-audit")).toBeNull()
    expect(container.querySelectorAll(".activity-audit")).toHaveLength(0)
  })
})
