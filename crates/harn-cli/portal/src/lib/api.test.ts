import { describe, expect, it, vi } from "vitest"

import { fetchPersonaStatus, fetchRunCompare } from "./api"

describe("fetchRunCompare", () => {
  it("surfaces JSON error payloads", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: false,
        status: 400,
        json: async () => ({ error: "bad compare input" }),
      }),
    )

    await expect(fetchRunCompare("left", "right")).rejects.toThrow(
      "Request failed: 400 bad compare input",
    )
  })
})

describe("fetchPersonaStatus", () => {
  it("encodes name and deterministic status timestamp", async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ name: "merge_captain", state: "paused" }),
    })
    vi.stubGlobal("fetch", fetchMock)

    await fetchPersonaStatus("merge captain", "2026-04-24T12:00:00Z")

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/persona/status?name=merge+captain&at=2026-04-24T12%3A00%3A00Z",
    )
  })
})
