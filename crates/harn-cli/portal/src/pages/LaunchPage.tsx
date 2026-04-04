import { defineMessages, useIntl } from "react-intl"
import { useNavigate } from "react-router-dom"

import { LaunchPanel } from "../components/LaunchPanel"
import { useLaunchData } from "../hooks/useLaunchData"

const messages = defineMessages({
  eyebrow: {
    id: "portal.launchPage.eyebrow",
    defaultMessage: "Launch workspace",
  },
  title: {
    id: "portal.launchPage.title",
    defaultMessage: "Run or prototype workflows",
  },
  copy: {
    id: "portal.launchPage.copy",
    defaultMessage: "Use the playground, script editor, or an existing file without crowding the run debugger.",
  },
})

export function LaunchPage() {
  const intl = useIntl()
  const navigate = useNavigate()
  const { portalMeta, llmOptions, launchTargets, launchJobs, lastError, launchRun, loadLaunchData } = useLaunchData()

  return (
    <section className="workspace-section">
      <div className="section-heading">
        <div className="eyebrow">{intl.formatMessage(messages.eyebrow)}</div>
        <h2>{intl.formatMessage(messages.title)}</h2>
        <p>{intl.formatMessage(messages.copy)}</p>
        {lastError ? <p className="muted">{lastError}</p> : null}
      </div>
      <LaunchPanel
        meta={portalMeta}
        llmOptions={llmOptions}
        targets={launchTargets}
        jobs={launchJobs}
        onLaunch={async (payload) => {
          await launchRun(payload)
          await loadLaunchData()
        }}
        onOpenRun={(path) => {
          navigate(`/runs/detail?path=${encodeURIComponent(path)}`)
        }}
      />
    </section>
  )
}
