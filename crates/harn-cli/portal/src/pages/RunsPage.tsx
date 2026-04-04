import { defineMessages, useIntl } from "react-intl"
import { useNavigate, useSearchParams } from "react-router-dom"

import { formatDuration, formatNumber, statusClass } from "../lib/format"
import { type RunSortOrder, type RunStatusFilter, useRunsData } from "../hooks/useRunsData"

const messages = defineMessages({
  eyebrow: { id: "portal.runsPage.eyebrow", defaultMessage: "Run library" },
  title: { id: "portal.runsPage.title", defaultMessage: "Persisted runs" },
  copy: {
    id: "portal.runsPage.copy",
    defaultMessage: "Search, filter, and page through saved runs without loading the whole dataset into the sidebar.",
  },
  refreshNow: { id: "portal.runsPage.refreshNow", defaultMessage: "Refresh" },
  filterRuns: { id: "portal.runsPage.filterRuns", defaultMessage: "Filter runs" },
  statusFilter: { id: "portal.runsPage.statusFilter", defaultMessage: "Status" },
  sortBy: { id: "portal.runsPage.sortBy", defaultMessage: "Sort by" },
  pageSize: { id: "portal.runsPage.pageSize", defaultMessage: "Rows" },
  filterPlaceholder: { id: "portal.runsPage.filterPlaceholder", defaultMessage: "workflow, model, status..." },
  allStatuses: { id: "portal.runsPage.allStatuses", defaultMessage: "All statuses" },
  activeOnly: { id: "portal.runsPage.activeOnly", defaultMessage: "Active only" },
  completedOnly: { id: "portal.runsPage.completedOnly", defaultMessage: "Completed only" },
  failedOnly: { id: "portal.runsPage.failedOnly", defaultMessage: "Failed only" },
  newestFirst: { id: "portal.runsPage.newestFirst", defaultMessage: "Newest first" },
  oldestFirst: { id: "portal.runsPage.oldestFirst", defaultMessage: "Oldest first" },
  longestDuration: { id: "portal.runsPage.longestDuration", defaultMessage: "Longest duration" },
  results: { id: "portal.runsPage.results", defaultMessage: "{shown} of {total} matching runs" },
  lastRefresh: { id: "portal.runsPage.lastRefresh", defaultMessage: "Last refresh {time}" },
  path: { id: "portal.runsPage.path", defaultMessage: "Path" },
  workflow: { id: "portal.runsPage.workflow", defaultMessage: "Workflow" },
  status: { id: "portal.runsPage.status", defaultMessage: "Status" },
  started: { id: "portal.runsPage.started", defaultMessage: "Started" },
  duration: { id: "portal.runsPage.duration", defaultMessage: "Duration" },
  usage: { id: "portal.runsPage.usage", defaultMessage: "Usage" },
  actions: { id: "portal.runsPage.actions", defaultMessage: "Actions" },
  inspect: { id: "portal.runsPage.inspect", defaultMessage: "Inspect" },
  empty: { id: "portal.runsPage.empty", defaultMessage: "No runs match the current query." },
  previous: { id: "portal.runsPage.previous", defaultMessage: "Previous" },
  next: { id: "portal.runsPage.next", defaultMessage: "Next" },
  pageSummary: { id: "portal.runsPage.pageSummary", defaultMessage: "Page {page} of {total}" },
})

function readNumber(value: string | null, fallback: number) {
  const parsed = Number(value)
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback
}

export function RunsPage() {
  const intl = useIntl()
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()
  const q = searchParams.get("q") ?? ""
  const status = (searchParams.get("status") as RunStatusFilter | null) ?? "all"
  const sort = (searchParams.get("sort") as RunSortOrder | null) ?? "newest"
  const page = readNumber(searchParams.get("page"), 1)
  const pageSize = readNumber(searchParams.get("page_size"), 25)
  const { runs, filteredCount, pagination, loading, lastError, lastRefreshAt, loadRuns } = useRunsData({
    q,
    status,
    sort,
    page,
    pageSize,
    poll: true,
  })

  function updateParams(next: Record<string, string | number | null>) {
    const updated = new URLSearchParams(searchParams)
    for (const [key, value] of Object.entries(next)) {
      if (value == null || value === "" || value === "all" || (key === "sort" && value === "newest")) {
        updated.delete(key)
      } else {
        updated.set(key, String(value))
      }
    }
    setSearchParams(updated)
  }

  return (
    <section className="workspace-section">
      <div className="section-heading">
        <div className="eyebrow">{intl.formatMessage(messages.eyebrow)}</div>
        <h2>{intl.formatMessage(messages.title)}</h2>
        <p>{intl.formatMessage(messages.copy)}</p>
      </div>

      <section className="panel runs-page-panel">
        <div className="runs-toolbar">
          <label className="search runs-search">
            <span>{intl.formatMessage(messages.filterRuns)}</span>
            <input
              type="search"
              value={q}
              placeholder={intl.formatMessage(messages.filterPlaceholder)}
              onChange={(event) => updateParams({ q: event.target.value, page: 1 })}
            />
          </label>
          <label className="search">
            <span>{intl.formatMessage(messages.statusFilter)}</span>
            <select
              className="compare-select"
              value={status}
              onChange={(event) => updateParams({ status: event.target.value, page: 1 })}
            >
              <option value="all">{intl.formatMessage(messages.allStatuses)}</option>
              <option value="active">{intl.formatMessage(messages.activeOnly)}</option>
              <option value="completed">{intl.formatMessage(messages.completedOnly)}</option>
              <option value="failed">{intl.formatMessage(messages.failedOnly)}</option>
            </select>
          </label>
          <label className="search">
            <span>{intl.formatMessage(messages.sortBy)}</span>
            <select
              className="compare-select"
              value={sort}
              onChange={(event) => updateParams({ sort: event.target.value, page: 1 })}
            >
              <option value="newest">{intl.formatMessage(messages.newestFirst)}</option>
              <option value="oldest">{intl.formatMessage(messages.oldestFirst)}</option>
              <option value="duration">{intl.formatMessage(messages.longestDuration)}</option>
            </select>
          </label>
          <label className="search">
            <span>{intl.formatMessage(messages.pageSize)}</span>
            <select
              className="compare-select"
              value={String(pageSize)}
              onChange={(event) => updateParams({ page_size: event.target.value, page: 1 })}
            >
              {[25, 50, 100].map((size) => (
                <option key={size} value={size}>
                  {size}
                </option>
              ))}
            </select>
          </label>
          <button className="action-button" disabled={loading} onClick={() => void loadRuns()} type="button">
            {intl.formatMessage(messages.refreshNow)}
          </button>
        </div>

        <div className="runs-meta">
          <span>{intl.formatMessage(messages.results, { shown: runs.length, total: filteredCount })}</span>
          {lastRefreshAt ? (
            <span>
              {intl.formatMessage(messages.lastRefresh, {
                time: new Date(lastRefreshAt).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" }),
              })}
            </span>
          ) : null}
          {lastError ? <span>{lastError}</span> : null}
        </div>

        {runs.length === 0 ? (
          <div className="empty-state empty-state-inline">
            <p>{intl.formatMessage(messages.empty)}</p>
          </div>
        ) : (
          <div className="table-shell">
            <table className="runs-table">
              <thead>
                <tr>
                  <th>{intl.formatMessage(messages.workflow)}</th>
                  <th>{intl.formatMessage(messages.path)}</th>
                  <th>{intl.formatMessage(messages.status)}</th>
                  <th>{intl.formatMessage(messages.started)}</th>
                  <th>{intl.formatMessage(messages.duration)}</th>
                  <th>{intl.formatMessage(messages.usage)}</th>
                  <th>{intl.formatMessage(messages.actions)}</th>
                </tr>
              </thead>
              <tbody>
                {runs.map((run) => (
                  <tr key={run.path}>
                    <td>
                      <div className="table-primary">{run.workflow_name}</div>
                      {run.failure_summary ? <div className="meta">{run.failure_summary}</div> : null}
                    </td>
                    <td className="mono table-path">{run.path}</td>
                    <td>
                      <span className={`pill ${statusClass(run.status)}`}>{run.status}</span>
                    </td>
                    <td>{run.started_at}</td>
                    <td>{formatDuration(run.duration_ms)}</td>
                    <td>
                      {formatNumber(run.call_count)} calls
                      <div className="meta">
                        {formatNumber(run.input_tokens + run.output_tokens)} tokens • {run.stage_count} stages
                      </div>
                    </td>
                    <td>
                      <button
                        className="action-button action-button-inline"
                        onClick={() => navigate(`/runs/detail?path=${encodeURIComponent(run.path)}`)}
                        type="button"
                      >
                        {intl.formatMessage(messages.inspect)}
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        <div className="pagination-bar">
          <button
            className="action-button"
            disabled={!pagination.has_previous}
            onClick={() => updateParams({ page: pagination.page - 1 })}
            type="button"
          >
            {intl.formatMessage(messages.previous)}
          </button>
          <div className="muted">
            {intl.formatMessage(messages.pageSummary, {
              page: pagination.page,
              total: pagination.total_pages,
            })}
          </div>
          <button
            className="action-button"
            disabled={!pagination.has_next}
            onClick={() => updateParams({ page: pagination.page + 1 })}
            type="button"
          >
            {intl.formatMessage(messages.next)}
          </button>
        </div>
      </section>
    </section>
  )
}
