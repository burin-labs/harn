import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react"
import { IntlProvider } from "react-intl"
import { MemoryRouter } from "react-router"
import { afterEach, expect, it, vi } from "vitest"

import { DlqPage } from "./DlqPage"

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

function renderQueue(query = "") {
  const fetchMock = vi.fn(async (_url: string, init?: RequestInit) => ({
    ok: !init,
    status: init ? 409 : 200,
    json: async (): Promise<unknown> => init
      ? { error: "Binding version changed" }
      : { total: 0, entries: [], groups: [], alerts: [] },
  }))
  vi.stubGlobal("fetch", fetchMock)
  render(
    <MemoryRouter initialEntries={[`/dlq${query}`]}>
      <IntlProvider locale="en"><DlqPage /></IntlProvider>
    </MemoryRouter>,
  )
  return fetchMock
}

it.each(["Search", "Trigger", "Provider"])("preserves pending as a %s filter", async (label) => {
  const fetchMock = renderQueue()
  const input = screen.getByLabelText(label)
  fireEvent.change(input, { target: { value: "pending" } })
  expect(input).toHaveValue("pending")
  const key = { Search: "q", Trigger: "trigger_id", Provider: "provider" }[label]!
  await waitFor(() => expect(fetchMock).toHaveBeenLastCalledWith(
    expect.stringContaining(`${key}=pending`),
  ))
})

it.each(["2026-04-24T17:30:00Z", "2026-04-24T23:00:00+05:30"])(
  "round-trips the date filter %s through local time",
  async (since) => {
    const fetchMock = renderQueue(`?since=${encodeURIComponent(since)}`)
    const input = screen.getByLabelText<HTMLInputElement>("Since")
    expect(new Date(input.value).getTime()).toBe(new Date(since).getTime())
    fireEvent.change(input, { target: { value: "2026-04-24T11:30" } })
    expect(input).toHaveValue("2026-04-24T11:30")
    await waitFor(() => expect(fetchMock).toHaveBeenLastCalledWith(
      expect.stringContaining(`since=${encodeURIComponent(new Date(2026, 3, 24, 11, 30).toISOString())}`),
    ))
  },
)

it("shows a rejected operation and clears the error on the next attempt", async () => {
  const fetchMock = renderQueue()
  vi.stubGlobal("confirm", vi.fn(() => true))
  fireEvent.click(screen.getByRole("button", { name: "Purge old unknown" }))
  expect(await screen.findByText("Request failed: 409 Binding version changed")).toBeInTheDocument()
  fetchMock.mockResolvedValueOnce({
    ok: true,
    status: 200,
    json: async () => ({ operation: "purge", accepted_count: 1, skipped_count: 0 }),
  })
  fireEvent.click(screen.getByRole("button", { name: "Purge old unknown" }))
  expect(await screen.findByText("purge: 1 accepted, 0 skipped")).toBeInTheDocument()
  expect(screen.queryByText("Request failed: 409 Binding version changed")).not.toBeInTheDocument()
})
