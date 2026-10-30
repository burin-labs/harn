import { portalText } from "../i18n/portalText"
import { useEffect, useMemo, useState } from "react"
import { useIntl } from "react-intl"

import { fetchRunCompare } from "../lib/api"
import { formatDuration, formatNumber, pct, statusClass } from "../lib/format"
import type { PortalRunDetail, PortalRunDiff, RunSummary } from "../types"
import { ActionGraph } from "./ActionGraph"
import { RunActivityItem } from "./RunActivityItem"
import { SkillObservability } from "./SkillObservability"
import { runDetailMessages as messages } from "./runDetailMessages"

type RunDetailProps = {
  detail: PortalRunDetail | null
  runs: RunSummary[]
  onSelectRun: (path: string) => void
}

function compareCandidates(current: RunSummary, runs: RunSummary[]) {
  return runs.filter(
    (run) =>
      run.path !== current.path && run.workflow_name === current.workflow_name,
  )
}

function findBaselineRun(current: RunSummary, runs: RunSummary[]) {
  return (
    compareCandidates(current, runs).find(
      (run) => run.started_at <= current.started_at,
    ) ?? null
  )
}

function StageDetail({
  label,
  value,
  open = false,
}: {
  label: string
  value: string | null
  open?: boolean
}) {
  if (!value) {
    return null
  }
  return (
    <details open={open}>
      <summary>{label}</summary>
      <pre>{value}</pre>
    </details>
  )
}

export function RunDetail({ detail, runs, onSelectRun }: RunDetailProps) {
  const intl = useIntl()
  const [compareBaselinePath, setCompareBaselinePath] = useState<string | null>(
    null,
  )
  const [compareResult, setCompareResult] = useState<{
    requestKey: string
    diff: PortalRunDiff | null
    error: string | null
  } | null>(null)

  const compareOptions = useMemo(
    () => (detail ? compareCandidates(detail.summary, runs) : []),
    [detail, runs],
  )

  const selectedBaselinePath = useMemo(() => {
    if (!detail || compareOptions.length === 0) {
      return null
    }
    if (
      compareBaselinePath &&
      compareOptions.some((run) => run.path === compareBaselinePath)
    ) {
      return compareBaselinePath
    }
    return (
      findBaselineRun(detail.summary, runs)?.path ??
      compareOptions[0]?.path ??
      null
    )
  }, [compareBaselinePath, compareOptions, detail, runs])

  const compareRequestKey =
    detail && selectedBaselinePath
      ? `${selectedBaselinePath}::${detail.summary.path}`
      : null
  const compareDiff =
    compareResult?.requestKey === compareRequestKey ? compareResult.diff : null
  const compareError =
    compareResult?.requestKey === compareRequestKey ? compareResult.error : null

  useEffect(() => {
    if (!detail || !selectedBaselinePath || !compareRequestKey) {
      return
    }
    let cancelled = false
    const currentPath = detail.summary.path
    const baselinePath = selectedBaselinePath
    const requestKey = compareRequestKey

    async function loadCompare() {
      try {
        const diff = await fetchRunCompare(baselinePath, currentPath)
        if (!cancelled) {
          setCompareResult({
            requestKey,
            diff,
            error: null,
          })
        }
      } catch (error) {
        if (!cancelled) {
          setCompareResult({
            requestKey,
            diff: null,
            error: error instanceof Error ? error.message : String(error),
          })
        }
      }
    }

    void loadCompare()
    return () => {
      cancelled = true
    }
  }, [compareRequestKey, detail, selectedBaselinePath])

  if (!detail) {
    return (
      <section className="empty-state">
        <h2>{intl.formatMessage(messages.noRunSelectedTitle)}</h2>
        <p>{intl.formatMessage(messages.noRunSelectedCopy)}</p>
      </section>
    )
  }

  const total = Math.max(
    ...detail.spans.map((span) => span.end_ms),
    detail.summary.duration_ms ?? 1,
    1,
  )
  const stageBars = detail.stages.reduce<
    Array<{
      key: string
      label: string
      kind: string
      left: string
      width: string
      subtitle: string
      top: number
    }>
  >((bars, stage, index) => {
    const offset = detail.stages
      .slice(0, index)
      .reduce((sum, item) => sum + (item.duration_ms ?? 0), 0)
    bars.push({
      key: stage.id,
      label: stage.node_id,
      kind: "stage",
      left: pct(offset, total),
      width: pct(stage.duration_ms ?? 0, total),
      subtitle: formatDuration(stage.duration_ms),
      top: 10,
    })
    return bars
  }, [])

  const spanBars = detail.spans.map((span) => ({
    key: `${span.span_id}`,
    label: span.label,
    kind: ["llm_call", "tool_call", "pipeline"].includes(span.kind)
      ? span.kind
      : "other",
    left: pct(span.start_ms, total),
    width: pct(span.duration_ms, total),
    subtitle: `${span.kind} • ${formatDuration(span.duration_ms)}`,
    top: span.lane * 40 + 10,
  }))

  const validationBadge =
    detail.policy_summary.validation_valid == null
      ? intl.formatMessage(messages.noValidationReport)
      : detail.policy_summary.validation_valid
        ? intl.formatMessage(messages.validationPassed)
        : intl.formatMessage(messages.validationFailed)
  const activityItemLabels = {
    auditLayersTooltip: intl.formatMessage(messages.auditLayersTooltip),
    auditReceiptLink: intl.formatMessage(messages.auditReceiptLink),
  }
  const compareRows = compareDiff
    ? [
        ...compareDiff.stage_diffs.map((item) => (
          <div className="compare-row" key={`${item.node_id}-${item.change}`}>
            <div>
              <strong>{item.node_id}</strong>
              <div className="meta">{item.change}</div>
            </div>
            <div className="meta">
              {item.details.map((detailLine) => (
                <div key={detailLine}>{detailLine}</div>
              ))}
            </div>
          </div>
        )),
        ...compareDiff.tool_diffs.map((item) => (
          <div
            className="compare-row"
            key={`${item.tool_name}-${item.args_hash}`}
          >
            <div>
              <strong>{item.tool_name}</strong>
              <div className="meta">{portalText("tool result changed")}</div>
            </div>
            <div className="meta">
              <div>
                {portalText("args {value0}", { value0: item.args_hash })}
              </div>
              <div>
                {portalText("left {value0}", {
                  value0: item.left_result ?? portalText("none"),
                })}
              </div>
              <div>
                {portalText("right {value0}", {
                  value0: item.right_result ?? portalText("none"),
                })}
              </div>
            </div>
          </div>
        )),
        ...compareDiff.observability_diffs.map((item, index) => (
          <div
            className="compare-row"
            key={`${item.section}-${item.label}-${index}`}
          >
            <div>
              <strong>{item.label}</strong>
              <div className="meta">{item.section}</div>
            </div>
            <div className="meta">
              {item.details.map((detailLine) => (
                <div key={detailLine}>{detailLine}</div>
              ))}
            </div>
          </div>
        )),
      ]
    : []
  const evidence = detail.view.evidence
  const flight = evidence.flight_recording

  return (
    <section className="detail">
      <header className="hero">
        <div>
          <div className="eyebrow">{intl.formatMessage(messages.run)}</div>
          <h2>{detail.summary.workflow_name}</h2>
          <p className="muted">
            {portalText("{value0} • {value1} • {value2} stages", {
              value0: detail.summary.status,
              value1: formatDuration(detail.summary.duration_ms),
              value2: detail.summary.stage_count,
            })}
          </p>
          <p className="mono muted">{detail.summary.path}</p>
        </div>
        <div className="hero-meta">
          {[
            [
              intl.formatMessage(messages.modelCalls),
              formatNumber(detail.summary.call_count),
            ],
            [
              intl.formatMessage(messages.tokens),
              portalText("{value0} in / {value1} out", { value0: formatNumber(detail.summary.input_tokens), value1: formatNumber(detail.summary.output_tokens) }),
            ],
            [
              intl.formatMessage(messages.childRuns),
              String(detail.summary.child_run_count),
            ],
            [intl.formatMessage(messages.started), detail.summary.started_at],
          ].map(([label, value]) => (
            <div className="card" key={label}>
              <div className="eyebrow">{label}</div>
              <div>{value}</div>
            </div>
          ))}
        </div>
      </header>

      <section className="panel">
        <div className="panel-header">
          <div>
            <h3>{portalText("Execution evidence")}</h3>
            <p>
              {portalText(
                "One Harn identity joins this run record, spans, and detailed artifacts.",
              )}
            </p>
          </div>
          <span
            className={`pill ${evidence.gaps.length ? "status-failed" : "status-completed"}`}
          >
            {evidence.gaps.length
              ? portalText("{count, plural, one {# gap} other {# gaps}}", { count: evidence.gaps.length })
              : portalText("complete")}
          </span>
        </div>
        <div className="policy-grid">
          <div className="policy-item">
            <div className="eyebrow">{portalText("Execution identity")}</div>
            <div className="mono">
              {evidence.execution_id || portalText("legacy record")}
            </div>
          </div>
          <div className="policy-item">
            <div className="row">
              <strong>{portalText("Flight recording")}</strong>
              <span className="turn-chip">
                {flight
                  ? portalText("{value0} events", { value0: formatNumber(flight.retained_events) })
                  : portalText("off")}
              </span>
            </div>
            {flight ? (
              <div className="policy-list">
                {flight.path ? (
                  <div className="mono meta">{flight.path}</div>
                ) : null}
                <div className="meta">
                  {portalText("{value0} dropped • values {value1}", {
                    value0: formatNumber(flight.dropped_events),
                    value1: flight.value_policy,
                  })}
                </div>
                <div className="mono meta">{flight.content_hash}</div>
              </div>
            ) : (
              <div className="meta">
                {portalText("No exact VM path was requested for this run.")}
              </div>
            )}
          </div>
          <div className="policy-item">
            <div className="row">
              <strong>{portalText("Evidence gaps")}</strong>
              <span className="turn-chip">{evidence.gaps.length}</span>
            </div>
            <div className="policy-list">
              {evidence.gaps.length ? (
                evidence.gaps.map((gap) => (
                  <div className="meta" key={`${gap.component}:${gap.code}`}>
                    {gap.component} / {gap.code}: {gap.message}
                  </div>
                ))
              ) : (
                <div className="meta">
                  {portalText("No requested evidence is missing.")}
                </div>
              )}
            </div>
          </div>
        </div>
      </section>

      <section className="card-grid">
        {detail.insights.map((item) => (
          <div className="card" key={item.label}>
            <div className="eyebrow">{item.label}</div>
            <div className="value">{item.value}</div>
            <div className="muted">{item.detail}</div>
          </div>
        ))}
      </section>

      <section className="panel">
        <div className="panel-header">
          <div>
            <h3>{intl.formatMessage(messages.capabilityValidation)}</h3>
            <p>{intl.formatMessage(messages.capabilityValidationCopy)}</p>
          </div>
        </div>
        <div className="policy-grid">
          <div className="policy-item">
            <div className="row">
              <strong>{portalText("Tool and side-effect ceiling")}</strong>
              <span className="turn-chip">{validationBadge}</span>
            </div>
            <div className="policy-list">
              <div className="meta">
                {portalText("tools{value0}{value1}", {
                  value0: " ",
                  value1: detail.policy_summary.tools.length
                    ? detail.policy_summary.tools.join(", ")
                    : intl.formatMessage(messages.unrestricted),
                })}
              </div>
              <div className="meta">
                {portalText("side effects {value0}", {
                  value0:
                    detail.policy_summary.side_effect_level ??
                    intl.formatMessage(messages.notDeclared),
                })}
              </div>
              <div className="meta">
                {portalText("recursion{value0}{value1}", {
                  value0: " ",
                  value1:
                    detail.policy_summary.recursion_limit == null
                      ? portalText("default")
                      : String(detail.policy_summary.recursion_limit),
                })}
              </div>
            </div>
          </div>
          <div className="policy-item">
            <div className="row">
              <strong>{portalText("Capabilities and roots")}</strong>
              <span className="turn-chip">
                {portalText("{value0} ops", {
                  value0: detail.policy_summary.capabilities.length,
                })}
              </span>
            </div>
            <div className="policy-list">
              <div className="meta">
                {detail.policy_summary.capabilities.length
                  ? detail.policy_summary.capabilities.join(", ")
                  : intl.formatMessage(messages.noExplicitCapabilityOps)}
              </div>
              <div className="meta">
                {detail.policy_summary.workspace_roots.length
                  ? portalText("roots {value}", { value: detail.policy_summary.workspace_roots.join(", ") })
                  : intl.formatMessage(messages.noWorkspaceRoots)}
              </div>
              <div className="meta">
                {detail.policy_summary.tool_arg_constraints.length
                  ? portalText("arg constraints {value}", { value: detail.policy_summary.tool_arg_constraints.join(" • ") })
                  : intl.formatMessage(messages.noArgConstraints)}
              </div>
            </div>
          </div>
          <div className="policy-item">
            <div className="row">
              <strong>{portalText("Validation diagnostics")}</strong>
              <span className="turn-chip">
                {portalText("{value0} errors", {
                  value0: detail.policy_summary.validation_errors.length,
                })}
              </span>
            </div>
            <div className="policy-list">
              <div className="meta">
                {detail.policy_summary.validation_errors.length
                  ? detail.policy_summary.validation_errors.join(" • ")
                  : intl.formatMessage(messages.noValidationErrors)}
              </div>
              <div className="meta">
                {detail.policy_summary.validation_warnings.length
                  ? detail.policy_summary.validation_warnings.join(" • ")
                  : intl.formatMessage(messages.noValidationWarnings)}
              </div>
              <div className="meta">
                {detail.policy_summary.reachable_nodes.length
                  ? portalText("reachable {value}", { value: detail.policy_summary.reachable_nodes.join(", ") })
                  : intl.formatMessage(messages.noReachableNodes)}
              </div>
            </div>
          </div>
        </div>
      </section>

      <SkillObservability
        timeline={detail.skill_timeline}
        matches={detail.skill_match_events}
        toolLoads={detail.tool_load_events}
      />

      <ActionGraph observability={detail.observability} />

      <section className="panel">
        <div className="panel-header">
          <div>
            <h3>{intl.formatMessage(messages.replayEval)}</h3>
            <p>{intl.formatMessage(messages.replayEvalCopy)}</p>
          </div>
        </div>
        {!detail.replay_summary ? (
          <div className="muted">
            {intl.formatMessage(messages.noReplayFixture)}
          </div>
        ) : (
          <div className="policy-grid">
            <div className="policy-item">
              <div className="row">
                <strong>{portalText("Fixture identity")}</strong>
                <span className="turn-chip">
                  {portalText("{value0} stage checks", {
                    value0: detail.replay_summary.stage_assertions.length,
                  })}
                </span>
              </div>
              <div className="policy-list">
                <div className="meta">
                  {portalText("fixture {value0}", {
                    value0: detail.replay_summary.fixture_id,
                  })}
                </div>
                <div className="meta">
                  {portalText("source run {value0}", {
                    value0: detail.replay_summary.source_run_id,
                  })}
                </div>
                <div className="meta">
                  {portalText("created {value0}", {
                    value0: detail.replay_summary.created_at,
                  })}
                </div>
                <div className="meta">
                  {portalText("expected status {value0}", {
                    value0: detail.replay_summary.expected_status,
                  })}
                </div>
              </div>
            </div>
            <div className="policy-item">
              <div className="row">
                <strong>{portalText("Replay assertions")}</strong>
                <span className="turn-chip">
                  {detail.replay_summary.stage_assertions.length}
                </span>
              </div>
              <div className="policy-list">
                {detail.replay_summary.stage_assertions.map((assertion) => (
                  <div className="meta" key={assertion.node_id}>
                    {assertion.node_id} → {assertion.expected_status}/
                    {assertion.expected_outcome}
                    {assertion.expected_branch
                      ? portalText(" • branch {value0}", {
                          value0: assertion.expected_branch,
                        })
                      : ""}
                    {assertion.required_artifact_kinds.length
                      ? portalText(" • artifacts {value0}", {
                          value0: assertion.required_artifact_kinds.join(", "),
                        })
                      : ""}
                    {assertion.visible_text_contains
                      ? portalText(" • text contains {value0}", {
                          value0: assertion.visible_text_contains,
                        })
                      : ""}
                  </div>
                ))}
              </div>
            </div>
            <div className="policy-item">
              <div className="row">
                <strong>{portalText("CLI next steps")}</strong>
                <span className="turn-chip">{detail.summary.path}</span>
              </div>
              <div className="policy-list">
                <div className="meta">
                  {intl.formatMessage(messages.replayCommand)}
                </div>
                <pre>{`harn replay .harn-runs/${detail.summary.path}`}</pre>
                <div className="meta">
                  {intl.formatMessage(messages.evalCommand)}
                </div>
                <pre>{`harn eval .harn-runs/${detail.summary.path}`}</pre>
              </div>
            </div>
          </div>
        )}
      </section>

      <div className="split split-tight">
        <section className="panel">
          <div className="panel-header">
            <div>
              <h3>{intl.formatMessage(messages.lineageExecution)}</h3>
              <p>{intl.formatMessage(messages.lineageExecutionCopy)}</p>
            </div>
          </div>
          <div className="lineage-grid">
            <div className="lineage-item">
              <div className="row">
                <strong>{portalText("Run lineage")}</strong>
                <span className={`pill ${statusClass(detail.summary.status)}`}>
                  {detail.summary.status}
                </span>
              </div>
              <div className="meta">
                {portalText("run {value0}", { value0: detail.summary.id })}
              </div>
              <div className="meta">
                {portalText("root {value0}", {
                  value0: detail.root_run_id ?? detail.summary.id,
                })}
              </div>
              <div className="meta">
                {portalText("parent {value0}", {
                  value0: detail.parent_run_id ?? portalText("none"),
                })}
              </div>
            </div>
            <div className="lineage-item">
              <div className="row">
                <strong>{portalText("Execution context")}</strong>
                <span className="turn-chip">
                  {detail.execution_summary?.adapter ??
                    portalText("unknown adapter")}
                </span>
              </div>
              <div className="meta">
                {detail.execution_summary?.repo_path ??
                  detail.execution_summary?.worktree_path ??
                  detail.execution_summary?.cwd ??
                  portalText("No repo or cwd persisted")}
              </div>
              <div className="meta">
                {portalText("branch {value0}", {
                  value0:
                    detail.execution_summary?.branch ??
                    portalText("not captured"),
                })}
              </div>
            </div>
          </div>
        </section>

        <section className="panel">
          <div className="panel-header">
            <div>
              <h3>{intl.formatMessage(messages.workflowFlow)}</h3>
              <p>{intl.formatMessage(messages.workflowFlowCopy)}</p>
            </div>
          </div>
          <div className="flow-grid">
            <div className="flow-item">
              <div className="row">
                <strong>{portalText("Transitions")}</strong>
                <span className="turn-chip">{detail.transitions.length}</span>
              </div>
              {detail.transitions.length ? (
                detail.transitions.slice(0, 8).map((item, index) => (
                  <div className="meta" key={`${item.to_node_id}-${index}`}>
                    {portalText(
                      "{value0} → {value1}{value2} • {value3} produced",
                      {
                        value0: item.from_node_id ?? portalText("start"),
                        value1: item.to_node_id,
                        value2: item.branch
                          ? portalText(" • branch {value0}", {
                              value0: item.branch,
                            })
                          : "",
                        value3: item.produced_count,
                      },
                    )}
                  </div>
                ))
              ) : (
                <div className="muted">
                  {intl.formatMessage(messages.noTransitions)}
                </div>
              )}
            </div>
            <div className="flow-item">
              <div className="row">
                <strong>{portalText("Checkpoints")}</strong>
                <span className="turn-chip">{detail.checkpoints.length}</span>
              </div>
              {detail.checkpoints.length ? (
                detail.checkpoints.slice(0, 8).map((item, index) => (
                  <div className="meta" key={`${item.reason}-${index}`}>
                    {portalText(
                      "{value0} • {value1} ready • {value2} completed{value3}",
                      {
                        value0: item.reason,
                        value1: item.ready_count,
                        value2: item.completed_count,
                        value3: item.last_stage_id
                          ? portalText(" • after {value0}", {
                              value0: item.last_stage_id,
                            })
                          : "",
                      },
                    )}
                  </div>
                ))
              ) : (
                <div className="muted">
                  {intl.formatMessage(messages.noCheckpoints)}
                </div>
              )}
            </div>
          </div>
        </section>
      </div>

      <section className="panel">
        <div className="panel-header">
          <div>
            <h3>{intl.formatMessage(messages.runComparison)}</h3>
            <p>{intl.formatMessage(messages.runComparisonCopy)}</p>
          </div>
        </div>
        {compareOptions.length === 0 ? (
          <div className="muted">
            {intl.formatMessage(messages.noCompareCandidates)}
          </div>
        ) : (
          <div className="compare-panel">
            <div className="compare-summary">
              <label className="search compare-field">
                <span>{intl.formatMessage(messages.baselineRun)}</span>
                <select
                  className="compare-select"
                  value={selectedBaselinePath ?? ""}
                  onChange={(event) =>
                    setCompareBaselinePath(event.target.value)
                  }
                >
                  {compareOptions.map((run) => (
                    <option key={run.path} value={run.path}>
                      {run.started_at} • {run.path}
                    </option>
                  ))}
                </select>
              </label>
              <div className="compare-badges">
                <span className="turn-chip">
                  {portalText("current {value0}", {
                    value0: detail.summary.path,
                  })}
                </span>
                {selectedBaselinePath ? (
                  <span className="turn-chip">
                    {portalText("baseline {value0}", {
                      value0: selectedBaselinePath,
                    })}
                  </span>
                ) : null}
                {compareDiff ? (
                  <>
                    <span className="turn-chip">
                      {compareDiff.identical
                        ? portalText("identical")
                        : portalText("changed")}
                    </span>
                    <span className="turn-chip">
                      {compareDiff.left_status} → {compareDiff.right_status}
                    </span>
                    <span className="turn-chip">
                      {portalText("{value0} stage diffs", {
                        value0: compareDiff.stage_diffs.length,
                      })}
                    </span>
                    <span className="turn-chip">
                      {portalText("{value0} tool diffs", {
                        value0: compareDiff.tool_diffs.length,
                      })}
                    </span>
                    <span className="turn-chip">
                      {portalText("{value0} observability diffs", {
                        value0: compareDiff.observability_diffs.length,
                      })}
                    </span>
                    <span className="turn-chip">
                      {portalText("{value0}{value1} transitions", {
                        value0:
                          compareDiff.transition_count_delta >= 0 ? "+" : "",
                        value1: compareDiff.transition_count_delta,
                      })}
                    </span>
                    <span className="turn-chip">
                      {portalText("{value0}{value1} artifacts", {
                        value0:
                          compareDiff.artifact_count_delta >= 0 ? "+" : "",
                        value1: compareDiff.artifact_count_delta,
                      })}
                    </span>
                    <span className="turn-chip">
                      {portalText("{value0}{value1} checkpoints", {
                        value0:
                          compareDiff.checkpoint_count_delta >= 0 ? "+" : "",
                        value1: compareDiff.checkpoint_count_delta,
                      })}
                    </span>
                  </>
                ) : null}
              </div>
            </div>
            {compareError ? (
              <div className="muted">
                {intl.formatMessage(messages.comparisonFailed, {
                  message: compareError,
                })}
              </div>
            ) : compareRows.length ? (
              compareRows
            ) : (
              <div className="muted">
                {compareDiff &&
                !compareDiff.stage_diffs.length &&
                !compareDiff.tool_diffs.length &&
                !compareDiff.observability_diffs.length
                  ? intl.formatMessage(messages.noObservabilityDiffs)
                  : intl.formatMessage(messages.noStageDiffs)}
              </div>
            )}
          </div>
        )}
      </section>

      <section className="panel">
        <div className="panel-header">
          <div>
            <h3>{intl.formatMessage(messages.traceTimeline)}</h3>
            <p>{intl.formatMessage(messages.traceTimelineCopy)}</p>
          </div>
        </div>
        <div className="flamegraph">
          {[
            {
              title: intl.formatMessage(messages.workflowStages),
              copy: intl.formatMessage(messages.workflowStagesCopy),
              items: stageBars,
              height: 56,
            },
            {
              title: intl.formatMessage(messages.nestedSpans),
              copy: intl.formatMessage(messages.nestedSpansCopy),
              items: spanBars,
              height: Math.max(
                56,
                (Math.max(0, ...detail.spans.map((span) => span.lane)) + 1) *
                  40 +
                  20,
              ),
            },
          ].map((lane) => (
            <div className="lane" key={lane.title}>
              <div className="lane-header">
                <div>
                  <strong>{lane.title}</strong>
                  <div className="muted">{lane.copy}</div>
                </div>
              </div>
              <div className="track" style={{ height: `${lane.height}px` }}>
                <div className="grid">
                  {Array.from({ length: 12 }).map((_, index) => (
                    <span key={index} />
                  ))}
                </div>
                {lane.items.map((item) => (
                  <div
                    className={`bar ${item.kind}`}
                    key={item.key}
                    title={`${item.label} • ${item.subtitle}`}
                    style={{
                      left: `${item.left}%`,
                      width: `${Math.max(Number(item.width), 1.5)}%`,
                      top: `${item.top}px`,
                    }}
                  >
                    {item.label} <span className="muted">{item.subtitle}</span>
                  </div>
                ))}
              </div>
            </div>
          ))}
        </div>
      </section>

      <div className="split">
        <section className="panel">
          <div className="panel-header">
            <div>
              <h3>{intl.formatMessage(messages.stageSummary)}</h3>
              <p>{intl.formatMessage(messages.stageSummaryCopy)}</p>
            </div>
          </div>
          <div className="table-like">
            {detail.stages.map((stage) => (
              <div className="stage-row" key={stage.id}>
                <div className="row">
                  <strong>{stage.node_id}</strong>
                  <span
                    className={`pill ${statusClass(stage.outcome || stage.status)}`}
                  >
                    {stage.outcome || stage.status}
                  </span>
                </div>
                <div className="meta">
                  {portalText(
                    "{value0}{value1} • {value2} •{value3}{value4} attempts • {value5} artifacts",
                    {
                      value0: stage.kind || portalText("stage"),
                      value1: stage.branch
                        ? portalText(" • branch {value0}", {
                            value0: stage.branch,
                          })
                        : "",
                      value2: formatDuration(stage.duration_ms),
                      value3: " ",
                      value4: stage.attempt_count,
                      value5: stage.artifact_count,
                    },
                  )}
                </div>
                <div className="meta">
                  {portalText(
                    "{value0} calls • {value1} in /{value2}{value3} out",
                    {
                      value0: formatNumber(stage.debug.call_count),
                      value1: formatNumber(stage.debug.input_tokens),
                      value2: " ",
                      value3: formatNumber(stage.debug.output_tokens),
                    },
                  )}
                </div>
                {stage.verification_summary ? (
                  <div className="meta">{stage.verification_summary}</div>
                ) : null}
                <details>
                  <summary>
                    {intl.formatMessage(messages.stageInternals)}
                  </summary>
                  <div className="stage-debug">
                    {stage.debug.worker_id ? (
                      <div className="meta">
                        {portalText("worker {value0}", {
                          value0: stage.debug.worker_id,
                        })}
                      </div>
                    ) : null}
                    <div className="meta">
                      {portalText("consumed {value0}", {
                        value0:
                          stage.debug.consumed_artifact_ids.join(", ") ||
                          portalText("none"),
                      })}
                    </div>
                    <div className="meta">
                      {portalText("produced {value0}", {
                        value0:
                          stage.debug.produced_artifact_ids.join(", ") ||
                          portalText("none"),
                      })}
                    </div>
                    <div className="meta">
                      {portalText("selected {value0}", {
                        value0:
                          stage.debug.selected_artifact_ids.join(", ") ||
                          portalText("none"),
                      })}
                    </div>
                    <StageDetail
                      label={portalText("Capability policy")}
                      value={stage.debug.capability_policy}
                      open
                    />
                    <StageDetail
                      label={portalText("Input contract")}
                      value={stage.debug.input_contract}
                    />
                    <StageDetail
                      label={portalText("Output contract")}
                      value={stage.debug.output_contract}
                    />
                    <StageDetail
                      label={portalText("Model policy")}
                      value={stage.debug.model_policy}
                    />
                    <StageDetail
                      label={portalText("Auto compact")}
                      value={stage.debug.auto_compact}
                    />
                    <StageDetail
                      label={portalText("Output visibility")}
                      value={stage.debug.output_visibility}
                    />
                    <StageDetail
                      label={portalText("Context policy")}
                      value={stage.debug.context_policy}
                    />
                    <StageDetail
                      label={portalText("Retry policy")}
                      value={stage.debug.retry_policy}
                    />
                    <StageDetail
                      label={portalText("Prompt")}
                      value={stage.debug.prompt}
                    />
                    <StageDetail
                      label={portalText("System prompt")}
                      value={stage.debug.system_prompt}
                    />
                    <StageDetail
                      label={portalText("Rendered context")}
                      value={stage.debug.rendered_context}
                    />
                    <StageDetail
                      label={portalText("Saved error")}
                      value={stage.debug.error}
                    />
                  </div>
                </details>
              </div>
            ))}
          </div>
        </section>

        <section className="panel">
          <div className="panel-header">
            <div>
              <h3>{intl.formatMessage(messages.runtimeActivity)}</h3>
              <p>{intl.formatMessage(messages.runtimeActivityCopy)}</p>
            </div>
          </div>
          <div className="activity-list">
            {detail.activities.length ? (
              detail.activities.slice(0, 40).map((item) => {
                return (
                  <RunActivityItem
                    key={`${item.label}-${item.started_offset_ms}`}
                    item={item}
                    labels={activityItemLabels}
                  />
                )
              })
            ) : (
              <div className="muted">
                {intl.formatMessage(messages.noActivity)}
              </div>
            )}
          </div>
        </section>
      </div>

      <section className="panel">
        <div className="panel-header">
          <div>
            <h3>{intl.formatMessage(messages.producedArtifacts)}</h3>
            <p>{intl.formatMessage(messages.producedArtifactsCopy)}</p>
          </div>
        </div>
        <div className="table-like">
          {detail.artifacts.length ? (
            detail.artifacts.map((artifact) => (
              <div className="artifact-item" key={artifact.id}>
                <div className="row">
                  <strong>{artifact.title}</strong>
                  <span className="turn-chip">{artifact.kind}</span>
                </div>
                <div className="meta">
                  {portalText(
                    "{value0} •{value1}{value2}{value3}• lineage {value4}",
                    {
                      value0:
                        artifact.source ||
                        artifact.stage ||
                        portalText("unknown source"),
                      value1: " ",
                      value2:
                        artifact.estimated_tokens != null
                          ? portalText("{value0} tokens est.", {
                              value0: formatNumber(artifact.estimated_tokens),
                            })
                          : portalText("no token estimate"),
                      value3: " ",
                      value4: artifact.lineage_count,
                    },
                  )}
                </div>
                <div className="meta">{artifact.preview}</div>
                <details>
                  <summary>{portalText("Artifact id")}</summary>
                  <pre>{artifact.id}</pre>
                </details>
              </div>
            ))
          ) : (
            <div className="muted">
              {intl.formatMessage(messages.noArtifacts)}
            </div>
          )}
        </div>
      </section>

      <section className="panel">
        <div className="panel-header">
          <div>
            <h3>{intl.formatMessage(messages.variantResolution)}</h3>
            <p>{intl.formatMessage(messages.variantResolutionCopy)}</p>
          </div>
        </div>
        <div className="turn-list">
          {detail.template_renders.length ? (
            detail.template_renders.map((render, index) => {
              const capabilityKeys = Object.keys(render.capabilities)
              return (
                <div
                  className="turn-item"
                  key={`${render.template_uri}-${index}`}
                >
                  <div className="row">
                    <strong>
                      {render.template_uri || portalText("(inline template)")}
                    </strong>
                    <span className="pill running">
                      {render.family || portalText("unknown")}
                    </span>
                  </div>
                  <div className="meta">
                    {render.provider}
                    {render.model ? ` • ${render.model}` : ""}
                    {render.rendered_bytes
                      ? portalText(" • {value0} bytes", {
                          value0: formatNumber(render.rendered_bytes),
                        })
                      : ""}
                  </div>
                  <div className="turn-chip-row">
                    {render.branches.length ? (
                      render.branches.map((branch, branchIndex) => (
                        <span
                          className="turn-chip"
                          key={`${branch.template_uri}:${branch.line}:${branch.col}:${branchIndex}`}
                          title={branch.branch_label ?? undefined}
                        >
                          {branch.kind}:{branch.branch_id}
                          {branch.branch_label
                            ? ` — ${branch.branch_label}`
                            : ""}
                        </span>
                      ))
                    ) : (
                      <span className="muted">
                        {intl.formatMessage(messages.variantBranchesNone)}
                      </span>
                    )}
                  </div>
                  <details>
                    <summary>{portalText("Capability snapshot")}</summary>
                    {capabilityKeys.length ? (
                      <pre>
                        {capabilityKeys
                          .sort()
                          .map(
                            (key) =>
                              `${key}: ${JSON.stringify(render.capabilities[key])}`,
                          )
                          .join("\n")}
                      </pre>
                    ) : (
                      <div className="muted">
                        {intl.formatMessage(messages.variantCapabilitiesNone)}
                      </div>
                    )}
                  </details>
                </div>
              )
            })
          ) : (
            <div className="muted">
              {intl.formatMessage(messages.noTemplateRenders)}
            </div>
          )}
        </div>
      </section>

      <section className="panel">
        <div className="panel-header">
          <div>
            <h3>{intl.formatMessage(messages.modelTurns)}</h3>
            <p>{intl.formatMessage(messages.modelTurnsCopy)}</p>
          </div>
        </div>
        <div className="turn-list">
          {detail.transcript_steps.length ? (
            detail.transcript_steps.map((step) => (
              <div className="turn-item" key={step.call_id}>
                <div className="row">
                  <strong>
                    {portalText("Step {value0}", { value0: step.call_index })}
                  </strong>
                  <span className="pill running">
                    {portalText("iteration {value0}", {
                      value0: step.iteration,
                    })}
                  </span>
                </div>
                <div className="meta">
                  {step.model}
                  {step.provider ? ` • ${step.provider}` : ""}
                </div>
                <div className="meta">{step.summary}</div>
                <div className="turn-chip-row">
                  <span className="turn-chip">
                    {portalText("{value0} msgs", {
                      value0: step.total_messages,
                    })}
                  </span>
                  <span className="turn-chip">
                    {portalText("{value0} kept", {
                      value0: step.kept_messages,
                    })}
                  </span>
                  <span className="turn-chip">
                    {portalText("{value0} added", {
                      value0: step.added_messages,
                    })}
                  </span>
                  {step.input_tokens != null ? (
                    <span className="turn-chip">
                      {portalText("{value0} in / {value1} out", {
                        value0: formatNumber(step.input_tokens),
                        value1: formatNumber(step.output_tokens ?? 0),
                      })}
                    </span>
                  ) : null}
                  {step.span_id != null ? (
                    <span className="turn-chip">
                      {portalText("span {value0}", { value0: step.span_id })}
                    </span>
                  ) : null}
                </div>
                <div className="turn-grid">
                  <div className="turn-panel">
                    <h4>{portalText("What changed right now")}</h4>
                    {step.system_prompt ? (
                      <div className="meta">
                        {portalText("Always-on instructions present")}
                      </div>
                    ) : null}
                    {step.added_context.length ? (
                      step.added_context.map((message, index) => (
                        <div
                          className="turn-message"
                          key={`${message.role}-${index}`}
                        >
                          <div className="role">{message.role}</div>
                          <pre>{message.content}</pre>
                        </div>
                      ))
                    ) : (
                      <div className="muted">
                        {intl.formatMessage(messages.noAddedContext)}
                      </div>
                    )}
                  </div>
                  <div className="turn-panel">
                    <h4>{portalText("What happened next")}</h4>
                    {step.tool_calls.length ? (
                      <div className="turn-chip-row">
                        {step.tool_calls.map((tool) => (
                          <span className="turn-chip" key={tool}>
                            {tool}
                          </span>
                        ))}
                      </div>
                    ) : (
                      <div className="muted">
                        {intl.formatMessage(messages.noToolCalls)}
                      </div>
                    )}
                    {step.response_text ? (
                      <details>
                        <summary>{portalText("Expand full reply")}</summary>
                        <pre>{step.response_text}</pre>
                      </details>
                    ) : (
                      <div className="muted">
                        {intl.formatMessage(messages.noResponseText)}
                      </div>
                    )}
                    {step.thinking ? (
                      <details>
                        <summary>
                          {portalText("Expand reasoning notes")}
                        </summary>
                        <pre>{step.thinking}</pre>
                      </details>
                    ) : null}
                  </div>
                </div>
              </div>
            ))
          ) : (
            <div className="muted">{intl.formatMessage(messages.noTurns)}</div>
          )}
        </div>
      </section>

      <div className="split">
        <section className="panel">
          <div className="panel-header">
            <div>
              <h3>{intl.formatMessage(messages.transcriptStory)}</h3>
              <p>{intl.formatMessage(messages.transcriptStoryCopy)}</p>
            </div>
          </div>
          <div className="story-list">
            {detail.story.length ? (
              detail.story.map((section, index) => (
                <div className="story-item" key={`${section.title}-${index}`}>
                  <div className="row">
                    <strong>{section.title}</strong>
                    <span className="pill running">{section.role}</span>
                  </div>
                  <div className="meta">
                    {section.scope} • {section.source}
                  </div>
                  <div className="meta">{section.preview}</div>
                  <details open={index < 2}>
                    <summary>{portalText("Expand full text")}</summary>
                    <pre>{section.text}</pre>
                  </details>
                </div>
              ))
            ) : (
              <div className="muted">
                {intl.formatMessage(messages.noStory)}
              </div>
            )}
          </div>
        </section>

        <section className="panel">
          <div className="panel-header">
            <div>
              <h3>{intl.formatMessage(messages.children)}</h3>
              <p>{intl.formatMessage(messages.childrenCopy)}</p>
            </div>
          </div>
          <div className="child-list">
            {detail.child_runs.length ? (
              detail.child_runs.map((child) => {
                const runPath = child.run_path
                return (
                  <div
                    className="child-item"
                    key={`${child.worker_name}-${child.started_at}`}
                  >
                    <div className="row">
                      <strong>{child.worker_name}</strong>
                      <span className={`pill ${statusClass(child.status)}`}>
                        {child.status}
                      </span>
                    </div>
                    <div className="meta">{child.task}</div>
                    <div className="meta">
                      {runPath ??
                        child.run_id ??
                        portalText("No persisted child run path")}
                    </div>
                    {runPath ? (
                      <button
                        className="run-card-link"
                        onClick={() => onSelectRun(runPath)}
                        type="button"
                      >
                        {intl.formatMessage(messages.openChildRun)}
                      </button>
                    ) : null}
                  </div>
                )
              })
            ) : (
              <div className="muted">
                {intl.formatMessage(messages.noChildren)}
              </div>
            )}
          </div>
        </section>
      </div>
    </section>
  )
}
