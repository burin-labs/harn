import { useState } from "react"
import { defineMessages, useIntl } from "react-intl"

import type {
  PortalSkillMatchEvent,
  PortalSkillTimelineEntry,
  PortalToolLoadEvent,
} from "../types"

const messages = defineMessages({
  timelineTitle: {
    id: "portal.skills.timelineTitle",
    defaultMessage: "Skill timeline",
  },
  timelineCopy: {
    id: "portal.skills.timelineCopy",
    defaultMessage:
      "Skills that activated this run, laid out by the iteration they joined and left the context.",
  },
  waterfallTitle: {
    id: "portal.skills.waterfallTitle",
    defaultMessage: "Tool-load waterfall",
  },
  waterfallCopy: {
    id: "portal.skills.waterfallCopy",
    defaultMessage:
      "Every time the agent issued a tool-search query and which deferred tools it brought into context as a result.",
  },
  matcherTitle: {
    id: "portal.skills.matcherTitle",
    defaultMessage: "Matcher decisions",
  },
  matcherCopy: {
    id: "portal.skills.matcherCopy",
    defaultMessage:
      "Ranked candidates the matcher considered on each turn. Click a row to see the scoring breakdown.",
  },
  noTimeline: {
    id: "portal.skills.noTimeline",
    defaultMessage: "No skills activated during this run.",
  },
  noWaterfall: {
    id: "portal.skills.noWaterfall",
    defaultMessage: "No tool-search queries fired for this run.",
  },
  noMatches: {
    id: "portal.skills.noMatches",
    defaultMessage: "No skill-match events were persisted for this run.",
  },
})

type Props = {
  timeline?: PortalSkillTimelineEntry[] | null
  matches?: PortalSkillMatchEvent[] | null
  toolLoads?: PortalToolLoadEvent[] | null
}

function maxIteration(
  timeline: PortalSkillTimelineEntry[],
  matches: PortalSkillMatchEvent[],
): number {
  let max = 0
  for (const entry of timeline) {
    max = Math.max(max, entry.activated_iteration, entry.deactivated_iteration ?? 0)
  }
  for (const match of matches) {
    max = Math.max(max, match.iteration)
  }
  return Math.max(max, 1)
}

function pctOf(value: number, total: number) {
  if (total <= 0) {
    return 0
  }
  return Math.min(100, Math.max(0, (value / total) * 100))
}

export function SkillObservability(props: Props) {
  const intl = useIntl()
  const [expanded, setExpanded] = useState<number | null>(null)
  const timeline = props.timeline ?? []
  const matches = props.matches ?? []
  const toolLoads = props.toolLoads ?? []
  const horizon = maxIteration(timeline, matches)

  return (
    <>
      <section className="panel">
        <div className="panel-header">
          <div>
            <h3>{intl.formatMessage(messages.timelineTitle)}</h3>
            <p>{intl.formatMessage(messages.timelineCopy)}</p>
          </div>
          <span className="turn-chip">{timeline.length} skills</span>
        </div>
        {timeline.length === 0 ? (
          <div className="muted">{intl.formatMessage(messages.noTimeline)}</div>
        ) : (
          <div className="skill-timeline">
            {timeline.map((entry, idx) => {
              const end = entry.deactivated_iteration ?? horizon
              const left = pctOf(entry.activated_iteration, horizon + 1)
              const width = Math.max(
                3,
                pctOf(end - entry.activated_iteration + 1, horizon + 1),
              )
              return (
                <div className="skill-timeline-row" key={`${entry.name}-${idx}`}>
                  <div className="skill-timeline-label">
                    <strong>{entry.name}</strong>
                    <div className="muted">{entry.description || "—"}</div>
                  </div>
                  <div className="skill-timeline-track">
                    <div
                      className="skill-timeline-bar"
                      title={`iter ${entry.activated_iteration}${
                        entry.deactivated_iteration != null
                          ? `–${entry.deactivated_iteration}`
                          : "+"
                      }${entry.score != null ? ` • score ${entry.score.toFixed(2)}` : ""}${
                        entry.reason ? ` • ${entry.reason}` : ""
                      }`}
                      style={{ left: `${left}%`, width: `${width}%` }}
                    >
                      iter {entry.activated_iteration}
                      {entry.deactivated_iteration != null
                        ? `–${entry.deactivated_iteration}`
                        : "+"}
                    </div>
                  </div>
                  <div className="skill-timeline-meta">
                    {entry.allowed_tools.length ? (
                      <span className="turn-chip">{entry.allowed_tools.length} tools</span>
                    ) : (
                      <span className="muted">any tools</span>
                    )}
                  </div>
                </div>
              )
            })}
          </div>
        )}
      </section>

      <section className="panel">
        <div className="panel-header">
          <div>
            <h3>{intl.formatMessage(messages.waterfallTitle)}</h3>
            <p>{intl.formatMessage(messages.waterfallCopy)}</p>
          </div>
          <span className="turn-chip">{toolLoads.length} queries</span>
        </div>
        {toolLoads.length === 0 ? (
          <div className="muted">{intl.formatMessage(messages.noWaterfall)}</div>
        ) : (
          <div className="tool-waterfall">
            {toolLoads.map((entry, idx) => (
              <div className="tool-waterfall-row" key={entry.tool_use_id ?? idx}>
                <div className="row">
                  <strong>{entry.query || "(no query)"}</strong>
                  <span className="turn-chip">
                    {entry.strategy || "?"}
                    {entry.mode ? ` • ${entry.mode}` : ""}
                  </span>
                </div>
                <div className="meta">
                  {entry.promoted.length
                    ? `promoted ${entry.promoted.join(", ")}`
                    : entry.references.length
                      ? `already loaded ${entry.references.join(", ")}`
                      : "no tools matched"}
                </div>
                {entry.scope !== "run" ? (
                  <div className="meta">scope {entry.scope}</div>
                ) : null}
              </div>
            ))}
          </div>
        )}
      </section>

      {matches.length === 0 ? null : (
        <section className="panel">
          <div className="panel-header">
            <div>
              <h3>{intl.formatMessage(messages.matcherTitle)}</h3>
              <p>{intl.formatMessage(messages.matcherCopy)}</p>
            </div>
            <span className="turn-chip">{matches.length} events</span>
          </div>
          <div className="table-like">
            {matches.map((match, idx) => {
              const open = expanded === idx
              return (
                <div className="activity-item" key={`match-${idx}`}>
                  <button
                    type="button"
                    className="match-expand"
                    onClick={() => setExpanded(open ? null : idx)}
                  >
                    <div className="row">
                      <strong>
                        iter {match.iteration}
                        {match.reassess ? " • reassess" : ""}
                      </strong>
                      <span>
                        {match.strategy || "metadata"} • {match.candidates.length}{" "}
                        candidates
                      </span>
                    </div>
                  </button>
                  {open ? (
                    <div className="match-detail">
                      {match.working_files.length ? (
                        <div className="meta">
                          working files {match.working_files.join(", ")}
                        </div>
                      ) : null}
                      {match.candidates.length ? (
                        <ul className="match-candidates">
                          {match.candidates.map((cand, ci) => (
                            <li key={`${cand.name}-${ci}`}>
                              <strong>{cand.name}</strong>
                              <span className="turn-chip">{cand.score.toFixed(2)}</span>
                              <span className="muted">{cand.reason || "—"}</span>
                            </li>
                          ))}
                        </ul>
                      ) : (
                        <div className="muted">No candidates considered.</div>
                      )}
                    </div>
                  ) : null}
                </div>
              )
            })}
          </div>
        </section>
      )}
    </>
  )
}
