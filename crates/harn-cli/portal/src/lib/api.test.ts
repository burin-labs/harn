import { describe, expect, it, vi } from "vitest"

import { fetchRunCompare } from "./api"

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
