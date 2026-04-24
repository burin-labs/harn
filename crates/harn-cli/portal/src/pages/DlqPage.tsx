import { useCallback, useEffect, useMemo, useState } from "react"
import { defineMessages, useIntl } from "react-intl"
import { useNavigate, useSearchParams } from "react-router-dom"

import {
  exportDlqEntry,
  fetchDlq,
  fetchLaunchJobs,
  purgeDlqBulk,
  purgeDlqEntry,
  replayDlqBulk,
  replayDlqEntry,
} from "../lib/api"
import type { PortalDlqEntry, PortalDlqListResponse, PortalLaunchJob } from "../types"

const ERROR_CLASSES = [
  "provider_5xx",
  "predicate_panic",
  "handler_panic",
  "handler_timeout",
  "auth_failed",
  "budget_exhausted",
  "unknown",
]

const messages = defineMessages({
  eyebrow: { id: "portal.dlqPage.eyebrow", defaultMessage: "Operations" },
  title: { id: "portal.dlqPage.title", defaultMessage: "Dead-letter queue" },
  copy: {
    id: "portal.dlqPage.copy",
    defaultMessage: "Inspect failed trigger deliveries, replay known-good fixes, and purge stale entries.",
  },
  refresh: { id: "portal.dlqPage.refresh", defaultMessage: "Refresh" },
  filter: { id: "portal.dlqPage.filter", defaultMessage: "Search" },
  filterPlaceholder: { id: "portal.dlqPage.filterPlaceholder", defaultMessage: "trigger, event, error..." },
  trigger: { id: "portal.dlqPage.trigger", defaultMessage: "Trigger" },
  provider: { id: "portal.dlqPage.provider", defaultMessage: "Provider" },
  errorClass: { id: "portal.dlqPage.errorClass", defaultMessage: "Error class" },
  allClasses: { id: "portal.dlqPage.allClasses", defaultMessage: "All classes" },
  allStates: { id: "portal.dlqPage.allStates", defaultMessage: "All states" },
  pending: { id: "portal.dlqPage.pending", defaultMessage: "Pending" },
  discarded: { id: "portal.dlqPage.discarded", defaultMessage: "Discarded" },
  state: { id: "portal.dlqPage.state", defaultMessage: "State" },
  since: { id: "portal.dlqPage.since", defaultMessage: "Since" },
  until: { id: "portal.dlqPage.until", defaultMessage: "Until" },
  entries: { id: "portal.dlqPage.entries", defaultMessage: "{count} entries" },
  replayAll: { id: "portal.dlqPage.replayAll", defaultMessage: "Replay filtered" },
  purgeOld: { id: "portal.dlqPage.purgeOld", defaultMessage: "Purge old unknown" },
  replay: { id: "portal.dlqPage.replay", defaultMessage: "Replay" },
  replayDrift: { id: "portal.dlqPage.replayDrift", defaultMessage: "Replay with drift accept" },
  purge: { id: "portal.dlqPage.purge", defaultMessage: "Purge" },
  exportFixture: { id: "portal.dlqPage.exportFixture", defaultMessage: "Export fixture" },
  event: { id: "portal.dlqPage.event", defaultMessage: "Event" },
  failedAt: { id: "portal.dlqPage.failedAt", defaultMessage: "Failed at" },
  retries: { id: "portal.dlqPage.retries", defaultMessage: "Retries" },
  lastError: { id: "portal.dlqPage.lastError", defaultMessage: "Last error" },
  actions: { id: "portal.dlqPage.actions", defaultMessage: "Actions" },
  detail: { id: "portal.dlqPage.detail", defaultMessage: "Detail" },
  payload: { id: "portal.dlqPage.payload", defaultMessage: "Payload" },
  headers: { id: "portal.dlqPage.headers", defaultMessage: "Headers" },
  attempts: { id: "portal.dlqPage.attempts", defaultMessage: "Attempt history" },
  predicateTrace: { id: "portal.dlqPage.predicateTrace", defaultMessage: "Predicate trace" },
  groups: { id: "portal.dlqPage.groups", defaultMessage: "Error groups" },
  alerts: { id: "portal.dlqPage.alerts", defaultMessage: "Alerts" },
  noAlerts: { id: "portal.dlqPage.noAlerts", defaultMessage: "No active DLQ spike alerts." },
  empty: { id: "portal.dlqPage.empty", defaultMessage: "No DLQ entries match the current filters." },
  queued: { id: "portal.dlqPage.queued", defaultMessage: "Queued {label}" },
  bulkDone: {
    id: "portal.dlqPage.bulkDone",
    defaultMessage: "{operation}: {accepted} accepted, {skipped} skipped",
  },
})

function pretty(value: unknown) {
  return JSON.stringify(value, null, 2)
}

function toInputDateTime(value: string | null) {
  if (!value) {
    return ""
  }
  return value.replace("Z", "").slice(0, 16)
}

function fromInputDateTime(value: string) {
  return value ? new Date(value).toISOString() : ""
}

export function DlqPage() {
  const intl = useIntl()
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()
  const [data, setData] = useState<PortalDlqListResponse | null>(null)
  const [selectedId, setSelectedId] = useState<string | null>(searchParams.get("entry"))
  const [loading, setLoading] = useState(false)
  const [lastError, setLastError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)

  const filters = useMemo(
    () => ({
      q: searchParams.get("q") ?? "",
      trigger_id: searchParams.get("trigger_id") ?? "",
      provider: searchParams.get("provider") ?? "",
      error_class: searchParams.get("error_class") ?? "",
      state: searchParams.get("state") ?? "pending",
      since: searchParams.get("since") ?? "",
      until: searchParams.get("until") ?? "",
    }),
    [searchParams],
  )

  const load = useCallback(async () => {
    setLoading(true)
    setLastError(null)
    try {
      const next = await fetchDlq({
        q: filters.q || undefined,
        trigger_id: filters.trigger_id || undefined,
        provider: filters.provider || undefined,
        error_class: filters.error_class || undefined,
        state: filters.state || undefined,
        since: filters.since || undefined,
        until: filters.until || undefined,
      })
      setData(next)
      if (!selectedId && next.entries.length > 0) {
        setSelectedId(next.entries[0].id)
      }
    } catch (error) {
      setLastError(error instanceof Error ? error.message : String(error))
    } finally {
      setLoading(false)
    }
  }, [filters, selectedId])

  useEffect(() => {
    void load()
  }, [load])

  const selected = data?.entries.find((entry) => entry.id === selectedId) ?? data?.entries[0] ?? null

  function updateParams(next: Record<string, string | null>) {
    const updated = new URLSearchParams(searchParams)
    for (const [key, value] of Object.entries(next)) {
      if (!value || value === "pending") {
        updated.delete(key)
      } else {
        updated.set(key, value)
      }
    }
    setSearchParams(updated)
  }

  async function runReplay(entry: PortalDlqEntry, driftAccept = false) {
    const job = await replayDlqEntry(entry.id, driftAccept)
    handleJob(job)
  }

  function handleJob(job: PortalLaunchJob) {
    setNotice(intl.formatMessage(messages.queued, { label: job.target_label }))
    if (job.discovered_run_paths[0]) {
      navigate(`/runs/detail?path=${encodeURIComponent(job.discovered_run_paths[0])}`)
      return
    }
    const startedAt = Date.now()
    const timer = window.setInterval(() => {
      void fetchLaunchJobs()
        .then((payload) => {
          const next = payload.jobs.find((candidate) => candidate.id === job.id)
          const runPath = next?.discovered_run_paths[0]
          if (runPath) {
            window.clearInterval(timer)
            navigate(`/runs/detail?path=${encodeURIComponent(runPath)}`)
          }
          if (next?.status === "failed" || Date.now() - startedAt > 30_000) {
            window.clearInterval(timer)
          }
        })
        .catch(() => {
          window.clearInterval(timer)
        })
    }, 1000)
  }

  async function runPurge(entry: PortalDlqEntry) {
    if (!window.confirm(`Purge DLQ entry ${entry.id}?`)) {
      return
    }
    await purgeDlqEntry(entry.id)
    setNotice(`Purged ${entry.id}`)
    await load()
  }

  async function runExport(entry: PortalDlqEntry) {
    const fixture = await exportDlqEntry(entry.id)
    const anchor = document.createElement("a")
    anchor.href = `data:application/json;charset=utf-8,${encodeURIComponent(pretty(fixture))}`
    anchor.download = `${entry.id}.json`
    anchor.click()
    setNotice(`Exported ${entry.id}`)
  }

  async function runBulkReplay() {
    const result = await replayDlqBulk({
      trigger_id: filters.trigger_id || undefined,
      provider: filters.provider || undefined,
      error_class: filters.error_class || undefined,
      since: filters.since || undefined,
      until: filters.until || undefined,
      rate_limit_per_second: 2,
    })
    setNotice(
      intl.formatMessage(messages.bulkDone, {
        operation: result.operation,
        accepted: result.accepted_count,
        skipped: result.skipped_count,
      }),
    )
  }

  async function runBulkPurgeOldUnknown() {
    if (!window.confirm("Purge pending unknown DLQ entries older than 30 days?")) {
      return
    }
    const result = await purgeDlqBulk({
      error_class: "unknown",
      older_than_seconds: 30 * 24 * 60 * 60,
      rate_limit_per_second: 2,
    })
    setNotice(
      intl.formatMessage(messages.bulkDone, {
        operation: result.operation,
        accepted: result.accepted_count,
        skipped: result.skipped_count,
      }),
    )
    await load()
  }

  return (
    <section className="workspace-section dlq-page">
      <div className="section-heading">
        <div className="eyebrow">{intl.formatMessage(messages.eyebrow)}</div>
        <h2>{intl.formatMessage(messages.title)}</h2>
        <p>{intl.formatMessage(messages.copy)}</p>
      </div>

      <section className="panel dlq-panel">
        <div className="runs-toolbar">
          <label className="search runs-search">
            <span>{intl.formatMessage(messages.filter)}</span>
            <input
              type="search"
              value={filters.q}
              placeholder={intl.formatMessage(messages.filterPlaceholder)}
              onChange={(event) => updateParams({ q: event.target.value })}
            />
          </label>
          <label className="search">
            <span>{intl.formatMessage(messages.trigger)}</span>
            <input
              type="search"
              value={filters.trigger_id}
              onChange={(event) => updateParams({ trigger_id: event.target.value })}
            />
          </label>
          <label className="search">
            <span>{intl.formatMessage(messages.provider)}</span>
            <input
              type="search"
              value={filters.provider}
              onChange={(event) => updateParams({ provider: event.target.value })}
            />
          </label>
          <label className="search">
            <span>{intl.formatMessage(messages.errorClass)}</span>
            <select
              className="compare-select"
              value={filters.error_class}
              onChange={(event) => updateParams({ error_class: event.target.value })}
            >
              <option value="">{intl.formatMessage(messages.allClasses)}</option>
              {ERROR_CLASSES.map((errorClass) => (
                <option key={errorClass} value={errorClass}>
                  {errorClass}
                </option>
              ))}
            </select>
          </label>
          <label className="search">
            <span>{intl.formatMessage(messages.state)}</span>
            <select
              className="compare-select"
              value={filters.state}
              onChange={(event) => updateParams({ state: event.target.value })}
            >
              <option value="pending">{intl.formatMessage(messages.pending)}</option>
              <option value="discarded">{intl.formatMessage(messages.discarded)}</option>
              <option value="all">{intl.formatMessage(messages.allStates)}</option>
            </select>
          </label>
          <label className="search">
            <span>{intl.formatMessage(messages.since)}</span>
            <input
              type="datetime-local"
              value={toInputDateTime(filters.since)}
              onChange={(event) => updateParams({ since: fromInputDateTime(event.target.value) })}
            />
          </label>
          <label className="search">
            <span>{intl.formatMessage(messages.until)}</span>
            <input
              type="datetime-local"
              value={toInputDateTime(filters.until)}
              onChange={(event) => updateParams({ until: fromInputDateTime(event.target.value) })}
            />
          </label>
          <button className="action-button" disabled={loading} onClick={() => void load()} type="button">
            {intl.formatMessage(messages.refresh)}
          </button>
        </div>

        <div className="runs-meta">
          <span>{intl.formatMessage(messages.entries, { count: data?.total ?? 0 })}</span>
          {lastError ? <span>{lastError}</span> : null}
          {notice ? <span>{notice}</span> : null}
        </div>

        <div className="dlq-summary-grid">
          <section className="subpanel">
            <h3>{intl.formatMessage(messages.groups)}</h3>
            <div className="chip-row">
              {(data?.groups ?? []).map((group) => (
                <span className="pill" key={group.error_class}>
                  {group.error_class}: {group.count}
                </span>
              ))}
            </div>
          </section>
          <section className="subpanel">
            <h3>{intl.formatMessage(messages.alerts)}</h3>
            {data?.alerts.length ? (
              <div className="chip-row">
                {data.alerts.map((alert) => (
                  <span className="pill danger" key={`${alert.trigger_id}-${alert.error_class}`}>
                    {alert.trigger_id} {alert.error_class} {alert.count}/{alert.threshold_entries}
                  </span>
                ))}
              </div>
            ) : (
              <div className="muted">{intl.formatMessage(messages.noAlerts)}</div>
            )}
          </section>
        </div>

        <div className="dlq-bulk-bar">
          <button className="action-button" disabled={!data?.entries.length} onClick={() => void runBulkReplay()} type="button">
            {intl.formatMessage(messages.replayAll)}
          </button>
          <button className="action-button" onClick={() => void runBulkPurgeOldUnknown()} type="button">
            {intl.formatMessage(messages.purgeOld)}
          </button>
        </div>

        {!data?.entries.length ? (
          <div className="empty-state empty-state-inline">
            <p>{intl.formatMessage(messages.empty)}</p>
          </div>
        ) : (
          <div className="dlq-layout">
            <div className="table-shell">
              <table className="runs-table">
                <thead>
                  <tr>
                    <th>{intl.formatMessage(messages.trigger)}</th>
                    <th>{intl.formatMessage(messages.event)}</th>
                    <th>{intl.formatMessage(messages.failedAt)}</th>
                    <th>{intl.formatMessage(messages.errorClass)}</th>
                    <th>{intl.formatMessage(messages.retries)}</th>
                    <th>{intl.formatMessage(messages.lastError)}</th>
                  </tr>
                </thead>
                <tbody>
                  {data.entries.map((entry) => (
                    <tr
                      className={entry.id === selected?.id ? "selected-row" : ""}
                      key={entry.id}
                      onClick={() => setSelectedId(entry.id)}
                    >
                      <td>
                        <div className="table-primary">{entry.trigger_id}</div>
                        <div className="meta">{entry.provider}</div>
                      </td>
                      <td>
                        <div className="mono">{entry.event_id}</div>
                        <div className="meta">{entry.event_kind}</div>
                      </td>
                      <td>{entry.failed_at}</td>
                      <td>
                        <span className="pill danger">{entry.error_class}</span>
                      </td>
                      <td>{entry.retry_count}</td>
                      <td className="dlq-error-cell">{entry.last_error}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>

            {selected ? (
              <aside className="subpanel dlq-detail-panel">
                <div className="panel-subheader">
                  <h3>{intl.formatMessage(messages.detail)}</h3>
                  <p className="mono">{selected.id}</p>
                </div>
                <div className="dlq-action-grid">
                  <button className="action-button" onClick={() => void runReplay(selected)} type="button">
                    {intl.formatMessage(messages.replay)}
                  </button>
                  <button className="action-button" onClick={() => void runReplay(selected, true)} type="button">
                    {intl.formatMessage(messages.replayDrift)}
                  </button>
                  <button className="action-button" onClick={() => void runExport(selected)} type="button">
                    {intl.formatMessage(messages.exportFixture)}
                  </button>
                  <button className="action-button danger-button" onClick={() => void runPurge(selected)} type="button">
                    {intl.formatMessage(messages.purge)}
                  </button>
                </div>
                <h4>{intl.formatMessage(messages.headers)}</h4>
                <pre>{pretty(selected.headers)}</pre>
                <h4>{intl.formatMessage(messages.payload)}</h4>
                <pre>{pretty(selected.payload)}</pre>
                <h4>{intl.formatMessage(messages.attempts)}</h4>
                <pre>{pretty(selected.attempt_history)}</pre>
                <h4>{intl.formatMessage(messages.predicateTrace)}</h4>
                <pre>{pretty(selected.predicate_trace)}</pre>
              </aside>
            ) : null}
          </div>
        )}
      </section>
    </section>
  )
}
