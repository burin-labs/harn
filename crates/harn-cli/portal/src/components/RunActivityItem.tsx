import { formatDuration } from "../lib/format"
import type { PortalActivity } from "../types"

export type RunActivityItemLabels = {
  auditLayersTooltip: string
  auditReceiptLink: string
}

type RunActivityItemProps = {
  item: PortalActivity
  labels: RunActivityItemLabels
}

export function RunActivityItem({ item, labels }: RunActivityItemProps) {
  const audit = item.audit
  const layerChain = audit?.layers.length ? audit.layers.map((layer) => layer.name).join(" → ") : null
  const tooltipId = audit ? `activity-audit-${item.call_id ?? item.label}-${item.started_offset_ms}` : undefined

  return (
    <div className="activity-item">
      <div className="row">
        <strong>{item.label}</strong>
        <span>{formatDuration(item.duration_ms)}</span>
      </div>
      {audit ? (
        <div className="activity-audit">
          {audit.reason ? (
            <span className="turn-chip activity-audit-reason" title={audit.reason}>
              {audit.reason}
            </span>
          ) : null}
          {layerChain ? (
            <span className="turn-chip activity-audit-layers" aria-describedby={tooltipId} title={layerChain}>
              {audit.layers.length} layer
              {audit.layers.length === 1 ? "" : "s"}
              <span id={tooltipId} role="tooltip" className="activity-audit-tooltip">
                <span className="activity-audit-tooltip-title">{labels.auditLayersTooltip}</span>
                <span className="activity-audit-tooltip-chain">{layerChain}</span>
              </span>
            </span>
          ) : null}
          {audit.receipt_uri ? (
            <a className="turn-chip activity-audit-receipt" href={audit.receipt_uri} target="_blank" rel="noreferrer">
              {labels.auditReceiptLink}
            </a>
          ) : null}
        </div>
      ) : null}
      <div className="meta">
        {item.kind} • +{formatDuration(item.started_offset_ms)}
        {item.stage_node_id ? ` • ${item.stage_node_id}` : ""}
      </div>
      <div className="meta">{item.summary}</div>
    </div>
  )
}
