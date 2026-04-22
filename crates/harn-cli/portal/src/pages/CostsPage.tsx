import { defineMessages, useIntl } from "react-intl"

import { formatNumber } from "../lib/format"
import { useCostData } from "../hooks/useCostData"

const messages = defineMessages({
  eyebrow: { id: "portal.costsPage.eyebrow", defaultMessage: "Cost dashboard" },
  title: { id: "portal.costsPage.title", defaultMessage: "LLM spend" },
  copy: {
    id: "portal.costsPage.copy",
    defaultMessage: "Track per-pipeline cost trend and provider breakdown from persisted run traces.",
  },
  refresh: { id: "portal.costsPage.refresh", defaultMessage: "Refresh" },
  totalCost: { id: "portal.costsPage.totalCost", defaultMessage: "Total cost" },
  calls: { id: "portal.costsPage.calls", defaultMessage: "Calls" },
  inputTokens: { id: "portal.costsPage.inputTokens", defaultMessage: "Input tokens" },
  outputTokens: { id: "portal.costsPage.outputTokens", defaultMessage: "Output tokens" },
  trend: { id: "portal.costsPage.trend", defaultMessage: "Pipeline trend" },
  providers: { id: "portal.costsPage.providers", defaultMessage: "Provider breakdown" },
  empty: { id: "portal.costsPage.empty", defaultMessage: "No LLM cost records found." },
  lastRefresh: { id: "portal.costsPage.lastRefresh", defaultMessage: "Last refresh {time}" },
  date: { id: "portal.costsPage.date", defaultMessage: "Date" },
  pipeline: { id: "portal.costsPage.pipeline", defaultMessage: "Pipeline" },
  provider: { id: "portal.costsPage.provider", defaultMessage: "Provider" },
  model: { id: "portal.costsPage.model", defaultMessage: "Model" },
  cost: { id: "portal.costsPage.cost", defaultMessage: "Cost" },
  usage: { id: "portal.costsPage.usage", defaultMessage: "Usage" },
})

function money(value: number) {
  return `$${value.toFixed(value >= 1 ? 2 : 4)}`
}

export function CostsPage() {
  const intl = useIntl()
  const { report, loading, lastError, lastRefreshAt, loadCosts } = useCostData()
  const maxTrendCost = Math.max(...(report?.trend.map((point) => point.cost_usd) ?? [0]), 0)

  return (
    <section className="workspace-section">
      <div className="section-heading">
        <div className="eyebrow">{intl.formatMessage(messages.eyebrow)}</div>
        <h2>{intl.formatMessage(messages.title)}</h2>
        <p>{intl.formatMessage(messages.copy)}</p>
      </div>

      <section className="panel runs-page-panel">
        <div className="runs-toolbar">
          <button className="action-button" disabled={loading} onClick={() => void loadCosts()} type="button">
            {intl.formatMessage(messages.refresh)}
          </button>
          <div className="runs-meta">
            {lastRefreshAt ? (
              <span>
                {intl.formatMessage(messages.lastRefresh, {
                  time: new Date(lastRefreshAt).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" }),
                })}
              </span>
            ) : null}
            {lastError ? <span>{lastError}</span> : null}
          </div>
        </div>

        {report ? (
          <>
            <div className="cost-summary-grid">
              <div className="card">
                <div className="eyebrow">{intl.formatMessage(messages.totalCost)}</div>
                <div className="value">{money(report.summary.total_cost_usd)}</div>
              </div>
              <div className="card">
                <div className="eyebrow">{intl.formatMessage(messages.calls)}</div>
                <div className="value">{formatNumber(report.summary.call_count)}</div>
              </div>
              <div className="card">
                <div className="eyebrow">{intl.formatMessage(messages.inputTokens)}</div>
                <div className="value">{formatNumber(report.summary.input_tokens)}</div>
              </div>
              <div className="card">
                <div className="eyebrow">{intl.formatMessage(messages.outputTokens)}</div>
                <div className="value">{formatNumber(report.summary.output_tokens)}</div>
              </div>
            </div>

            <div className="cost-sections">
              <section>
                <div className="panel-subheader">
                  <h3>{intl.formatMessage(messages.trend)}</h3>
                </div>
                {report.trend.length === 0 ? (
                  <div className="empty-state empty-state-inline">
                    <p>{intl.formatMessage(messages.empty)}</p>
                  </div>
                ) : (
                  <div className="cost-trend-list">
                    {report.trend.map((point) => {
                      const width = maxTrendCost > 0 ? Math.max(4, (point.cost_usd / maxTrendCost) * 100) : 4
                      return (
                        <div className="cost-trend-row" key={`${point.date}:${point.pipeline}`}>
                          <div>
                            <div className="table-primary">{point.pipeline}</div>
                            <div className="meta">{point.date}</div>
                          </div>
                          <div className="cost-bar" aria-label={money(point.cost_usd)}>
                            <span style={{ width: `${width}%` }} />
                          </div>
                          <div className="cost-amount">{money(point.cost_usd)}</div>
                        </div>
                      )
                    })}
                  </div>
                )}
              </section>

              <section>
                <div className="panel-subheader">
                  <h3>{intl.formatMessage(messages.providers)}</h3>
                </div>
                <div className="table-shell">
                  <table className="runs-table">
                    <thead>
                      <tr>
                        <th>{intl.formatMessage(messages.provider)}</th>
                        <th>{intl.formatMessage(messages.model)}</th>
                        <th>{intl.formatMessage(messages.cost)}</th>
                        <th>{intl.formatMessage(messages.usage)}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {report.provider_breakdown.map((row) => (
                        <tr key={`${row.provider}:${row.model}`}>
                          <td>{row.provider}</td>
                          <td className="mono table-path">{row.model}</td>
                          <td>{money(row.cost_usd)}</td>
                          <td>
                            {formatNumber(row.call_count)} calls
                            <div className="meta">
                              {formatNumber(row.input_tokens + row.output_tokens)} tokens
                            </div>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </section>
            </div>
          </>
        ) : null}
      </section>
    </section>
  )
}
