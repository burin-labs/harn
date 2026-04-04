import { defineMessages, useIntl } from "react-intl"
import { Link, useNavigate, useSearchParams } from "react-router-dom"

import { RunDetail } from "../components/RunDetail"
import { useRunDetailData } from "../hooks/useRunDetailData"

const messages = defineMessages({
  back: { id: "portal.runDetailPage.back", defaultMessage: "Back to runs" },
  eyebrow: { id: "portal.runDetailPage.eyebrow", defaultMessage: "Run inspector" },
  title: { id: "portal.runDetailPage.title", defaultMessage: "Inspect persisted run" },
  missing: { id: "portal.runDetailPage.missing", defaultMessage: "Choose a run from the runs page." },
})

export function RunDetailPage() {
  const intl = useIntl()
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const path = searchParams.get("path")
  const { detail, compareRuns, loading, lastError } = useRunDetailData(path)

  return (
    <section className="workspace-section">
      <div className="section-heading section-heading-inline">
        <div>
          <div className="eyebrow">{intl.formatMessage(messages.eyebrow)}</div>
          <h2>{intl.formatMessage(messages.title)}</h2>
        </div>
        <Link className="ghost-button" to="/runs">
          {intl.formatMessage(messages.back)}
        </Link>
      </div>
      {!path ? (
        <section className="empty-state empty-state-inline">
          <p>{intl.formatMessage(messages.missing)}</p>
        </section>
      ) : detail ? (
        <RunDetail
          detail={detail}
          runs={compareRuns}
          onSelectRun={(nextPath) => {
            navigate(`/runs/detail?path=${encodeURIComponent(nextPath)}`)
          }}
        />
      ) : (
        <section className="empty-state empty-state-inline">
          <p>{loading ? "Loading run…" : lastError ?? intl.formatMessage(messages.missing)}</p>
        </section>
      )}
    </section>
  )
}
