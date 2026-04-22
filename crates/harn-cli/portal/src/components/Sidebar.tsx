import { NavLink } from "react-router-dom"
import { defineMessages, useIntl } from "react-intl"

import { formatDuration } from "../lib/format"
import type { PortalStats } from "../types"

const messages = defineMessages({
  title: {
    id: "portal.sidebar.title",
    defaultMessage: "Portal",
  },
  intro: {
    id: "portal.sidebar.intro",
    defaultMessage: "Local observability and launch control for persisted Harn workflows.",
  },
  launch: {
    id: "portal.sidebar.launch",
    defaultMessage: "Launch",
  },
  runs: {
    id: "portal.sidebar.runs",
    defaultMessage: "Runs",
  },
  costs: {
    id: "portal.sidebar.costs",
    defaultMessage: "Costs",
  },
  refreshNow: {
    id: "portal.sidebar.refreshNow",
    defaultMessage: "Refresh stats",
  },
  waiting: {
    id: "portal.sidebar.waiting",
    defaultMessage: "Waiting for first refresh…",
  },
  refreshing: {
    id: "portal.sidebar.refreshing",
    defaultMessage: "Refreshing…",
  },
  lastRefresh: {
    id: "portal.sidebar.lastRefresh",
    defaultMessage: "Last refresh {time}",
  },
  error: {
    id: "portal.sidebar.error",
    defaultMessage: "Error: {message}",
  },
  runsStat: { id: "portal.sidebar.runsStat", defaultMessage: "Runs" },
  complete: { id: "portal.sidebar.complete", defaultMessage: "Complete" },
  active: { id: "portal.sidebar.active", defaultMessage: "Active" },
  failed: { id: "portal.sidebar.failed", defaultMessage: "Failed" },
  avgRun: { id: "portal.sidebar.avgRun", defaultMessage: "Avg run" },
})

type SidebarProps = {
  stats: PortalStats | null
  loading: boolean
  lastRefreshAt: number | null
  lastError: string | null
  onRefresh: () => void
}

export function Sidebar({ stats, loading, lastRefreshAt, lastError, onRefresh }: SidebarProps) {
  const intl = useIntl()

  const statCards = stats
    ? [
        [intl.formatMessage(messages.runsStat), stats.total_runs],
        [intl.formatMessage(messages.complete), stats.completed_runs],
        [intl.formatMessage(messages.active), stats.active_runs],
        [intl.formatMessage(messages.failed), stats.failed_runs],
        [intl.formatMessage(messages.avgRun), formatDuration(stats.avg_duration_ms)],
      ]
    : []

  const statusCopy = loading
    ? intl.formatMessage(messages.refreshing)
    : lastRefreshAt
      ? intl.formatMessage(messages.lastRefresh, {
          time: new Date(lastRefreshAt).toLocaleTimeString([], {
            hour: "numeric",
            minute: "2-digit",
          }),
        })
      : intl.formatMessage(messages.waiting)

  return (
    <aside className="sidebar">
      <div className="sidebar-header">
        <div className="eyebrow">Harn</div>
        <h1>{intl.formatMessage(messages.title)}</h1>
        <p>{intl.formatMessage(messages.intro)}</p>
      </div>

      <nav className="sidebar-nav" aria-label="Portal navigation">
        <NavLink className={({ isActive }) => `nav-link ${isActive ? "active" : ""}`} to="/launch">
          {intl.formatMessage(messages.launch)}
        </NavLink>
        <NavLink className={({ isActive }) => `nav-link ${isActive ? "active" : ""}`} to="/runs">
          {intl.formatMessage(messages.runs)}
        </NavLink>
        <NavLink className={({ isActive }) => `nav-link ${isActive ? "active" : ""}`} to="/costs">
          {intl.formatMessage(messages.costs)}
        </NavLink>
      </nav>

      <div className="stats-grid">
        {statCards.map(([label, value]) => (
          <div className="card" key={label}>
            <div className="eyebrow">{label}</div>
            <div className="value">{value}</div>
          </div>
        ))}
      </div>

      <div className="sidebar-tools">
        <button className="action-button" disabled={loading} onClick={onRefresh} type="button">
          {intl.formatMessage(messages.refreshNow)}
        </button>
        <div className="muted">{statusCopy}</div>
        {lastError ? <div className="muted">{intl.formatMessage(messages.error, { message: lastError })}</div> : null}
      </div>
    </aside>
  )
}
