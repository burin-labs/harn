import { useEffect, useMemo, useState } from "react"
import { defineMessages, useIntl } from "react-intl"

import { fetchRunCompare } from "../lib/api"
import { formatDuration, formatNumber, pct, statusClass } from "../lib/format"
import type { PortalRunDetail, PortalRunDiff, RunSummary } from "../types"
import { SkillObservability } from "./SkillObservability"

const messages = defineMessages({
  noRunSelectedTitle: {
    id: "portal.detail.noRunSelectedTitle",
    defaultMessage: "No run selected",
  },
  noRunSelectedCopy: {
    id: "portal.detail.noRunSelectedCopy",
    defaultMessage:
      "Pick a run from the left to inspect its stages, spans, transcript story, and child runs.",
  },
  run: { id: "portal.detail.run", defaultMessage: "Run" },
  modelCalls: { id: "portal.detail.modelCalls", defaultMessage: "Model calls" },
  tokens: { id: "portal.detail.tokens", defaultMessage: "Tokens" },
  childRuns: { id: "portal.detail.childRuns", defaultMessage: "Child runs" },
  started: { id: "portal.detail.started", defaultMessage: "Started" },
  capabilityValidation: {
    id: "portal.detail.capabilityValidation",
    defaultMessage: "Capability and validation",
  },
  capabilityValidationCopy: {
    id: "portal.detail.capabilityValidationCopy",
    defaultMessage:
      "The effective top-level ceiling for this run, plus any saved workflow validation report.",
  },
  replayEval: { id: "portal.detail.replayEval", defaultMessage: "Replay and eval" },
  replayEvalCopy: {
    id: "portal.detail.replayEvalCopy",
    defaultMessage:
      "Saved replay expectations derived from this run so you can turn debugging into a repeatable check.",
  },
  lineageExecution: { id: "portal.detail.lineageExecution", defaultMessage: "Lineage and execution" },
  lineageExecutionCopy: {
    id: "portal.detail.lineageExecutionCopy",
    defaultMessage: "Where this run sits in a larger tree, and which local execution context it used.",
  },
  workflowFlow: { id: "portal.detail.workflowFlow", defaultMessage: "Workflow flow" },
  workflowFlowCopy: {
    id: "portal.detail.workflowFlowCopy",
    defaultMessage: "The path this run took through transitions and checkpoints.",
  },
  actionGraph: { id: "portal.detail.actionGraph", defaultMessage: "Action graph" },
  actionGraphCopy: {
    id: "portal.detail.actionGraphCopy",
    defaultMessage:
      "One derived debugging artifact that rolls planner rounds, worker lineage, verification, and transcript pointers into the same view.",
  },
  runComparison: { id: "portal.detail.runComparison", defaultMessage: "Run comparison" },
  runComparisonCopy: {
    id: "portal.detail.runComparisonCopy",
    defaultMessage: "Compare this run against any other persisted run of the same workflow.",
  },
  traceTimeline: { id: "portal.detail.traceTimeline", defaultMessage: "Trace timeline" },
  traceTimelineCopy: {
    id: "portal.detail.traceTimelineCopy",
    defaultMessage: "A horizontal view of where time went across workflow stages and nested runtime spans.",
  },
  stageSummary: { id: "portal.detail.stageSummary", defaultMessage: "Stage summary" },
  stageSummaryCopy: {
    id: "portal.detail.stageSummaryCopy",
    defaultMessage: "Big-picture workflow progress, retries, and verification output.",
  },
  runtimeActivity: { id: "portal.detail.runtimeActivity", defaultMessage: "Runtime activity" },
  runtimeActivityCopy: {
    id: "portal.detail.runtimeActivityCopy",
    defaultMessage: "Span-derived activity feed ordered by when things happened.",
  },
  producedArtifacts: { id: "portal.detail.producedArtifacts", defaultMessage: "Produced artifacts" },
  producedArtifactsCopy: {
    id: "portal.detail.producedArtifactsCopy",
    defaultMessage: "The durable outputs this run saved for later stages, child runs, or inspection.",
  },
  modelTurns: { id: "portal.detail.modelTurns", defaultMessage: "Model turns" },
  modelTurnsCopy: {
    id: "portal.detail.modelTurnsCopy",
    defaultMessage:
      "Saved request/response turns from llm_transcript.jsonl when a transcript sidecar exists.",
  },
  transcriptStory: { id: "portal.detail.transcriptStory", defaultMessage: "Transcript story" },
  transcriptStoryCopy: {
    id: "portal.detail.transcriptStoryCopy",
    defaultMessage: "Human-visible transcript sections from the run and its stages.",
  },
  children: { id: "portal.detail.children", defaultMessage: "Child runs" },
  childrenCopy: {
    id: "portal.detail.childrenCopy",
    defaultMessage: "Delegated work launched under this run.",
  },
  baselineRun: { id: "portal.detail.baselineRun", defaultMessage: "Baseline run" },
  comparisonFailed: { id: "portal.detail.comparisonFailed", defaultMessage: "Comparison failed: {message}" },
  noCompareCandidates: {
    id: "portal.detail.noCompareCandidates",
    defaultMessage: "No other runs of this workflow were found to compare against.",
  },
  noStageDiffs: {
    id: "portal.detail.noStageDiffs",
    defaultMessage: "No stage-level differences were detected.",
  },
  noObservabilityDiffs: {
    id: "portal.detail.noObservabilityDiffs",
    defaultMessage: "No observability differences were detected.",
  },
  noReplayFixture: {
    id: "portal.detail.noReplayFixture",
    defaultMessage: "No replay fixture was saved with this run yet.",
  },
  replayCommand: { id: "portal.detail.replayCommand", defaultMessage: "Replay command" },
  evalCommand: { id: "portal.detail.evalCommand", defaultMessage: "Eval command" },
  stageInternals: { id: "portal.detail.stageInternals", defaultMessage: "Stage internals" },
  openChildRun: { id: "portal.detail.openChildRun", defaultMessage: "Open child run" },
  noToolCalls: { id: "portal.detail.noToolCalls", defaultMessage: "No tool calls recorded." },
  noResponseText: {
    id: "portal.detail.noResponseText",
    defaultMessage: "No response text persisted for this step.",
  },
  noAddedContext: {
    id: "portal.detail.noAddedContext",
    defaultMessage: "No newly added context captured for this step.",
  },
  noTurns: {
    id: "portal.detail.noTurns",
    defaultMessage: "No saved model transcript sidecar found for this run.",
  },
  noStory: {
    id: "portal.detail.noStory",
    defaultMessage: "No human-visible transcript sections were saved for this run.",
  },
  noChildren: {
    id: "portal.detail.noChildren",
    defaultMessage: "No delegated child runs for this run.",
  },
  noPlannerRounds: {
    id: "portal.detail.noPlannerRounds",
    defaultMessage: "No planner-round summaries were persisted for this run.",
  },
  noTranscriptPointers: {
    id: "portal.detail.noTranscriptPointers",
    defaultMessage: "No transcript pointers were available for this run.",
  },
  noWorkerLineage: {
    id: "portal.detail.noWorkerLineage",
    defaultMessage: "No delegated worker lineage was captured for this run.",
  },
  noDaemonEvents: {
    id: "portal.detail.noDaemonEvents",
    defaultMessage: "No daemon lifecycle events were captured for this run.",
  },
  noActivity: {
    id: "portal.detail.noActivity",
    defaultMessage: "No span activity captured for this run.",
  },
  noArtifacts: {
    id: "portal.detail.noArtifacts",
    defaultMessage: "This run did not persist any artifacts.",
  },
  noTransitions: {
    id: "portal.detail.noTransitions",
    defaultMessage: "No workflow transitions were persisted.",
  },
  noCheckpoints: {
    id: "portal.detail.noCheckpoints",
    defaultMessage: "No checkpoints were persisted.",
  },
  validationPassed: { id: "portal.detail.validationPassed", defaultMessage: "Validation passed" },
  validationFailed: { id: "portal.detail.validationFailed", defaultMessage: "Validation failed" },
  noValidationReport: { id: "portal.detail.noValidationReport", defaultMessage: "No validation report" },
  unrestricted: { id: "portal.detail.unrestricted", defaultMessage: "unrestricted" },
  notDeclared: { id: "portal.detail.notDeclared", defaultMessage: "not declared" },
  noExplicitCapabilityOps: {
    id: "portal.detail.noExplicitCapabilityOps",
    defaultMessage: "No explicit capability operations",
  },
  noWorkspaceRoots: {
    id: "portal.detail.noWorkspaceRoots",
    defaultMessage: "No workspace roots persisted",
  },
  noArgConstraints: {
    id: "portal.detail.noArgConstraints",
    defaultMessage: "No tool arg constraints",
  },
  noValidationErrors: {
    id: "portal.detail.noValidationErrors",
    defaultMessage: "No validation errors saved",
  },
  noValidationWarnings: {
    id: "portal.detail.noValidationWarnings",
    defaultMessage: "No validation warnings saved",
  },
  noReachableNodes: {
    id: "portal.detail.noReachableNodes",
    defaultMessage: "No reachable-node report saved",
  },
  workflowStages: { id: "portal.detail.workflowStages", defaultMessage: "Workflow stages" },
  workflowStagesCopy: {
    id: "portal.detail.workflowStagesCopy",
    defaultMessage: "Big-picture progress across the run",
  },
  nestedSpans: { id: "portal.detail.nestedSpans", defaultMessage: "Nested spans" },
  nestedSpansCopy: {
    id: "portal.detail.nestedSpansCopy",
    defaultMessage: "Runtime calls and operations inside those stages",
  },
})

type RunDetailProps = {
  detail: PortalRunDetail | null
  runs: RunSummary[]
  onSelectRun: (path: string) => void
}

function compareCandidates(current: RunSummary, runs: RunSummary[]) {
  return runs.filter((run) => run.path !== current.path && run.workflow_name === current.workflow_name)
}

function findBaselineRun(current: RunSummary, runs: RunSummary[]) {
  return compareCandidates(current, runs).find((run) => run.started_at <= current.started_at) ?? null
}

function daemonKindLabel(kind: PortalRunDetail["observability"]["daemon_events"][number]["kind"]) {
  switch (kind) {
    case "spawned":
      return "Spawned"
    case "triggered":
      return "Triggered"
    case "snapshotted":
      return "Snapshotted"
    case "resumed":
      return "Resumed"
    case "stopped":
      return "Stopped"
    default:
      return kind
  }
}

function StageDetail({ label, value, open = false }: { label: string; value: string | null; open?: boolean }) {
  if (!value) {return null}
  return (
    <details open={open}>
      <summary>{label}</summary>
      <pre>{value}</pre>
    </details>
  )
}

export function RunDetail({ detail, runs, onSelectRun }: RunDetailProps) {
  const intl = useIntl()
  const [compareBaselinePath, setCompareBaselinePath] = useState<string | null>(null)
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
    if (!detail || compareOptions.length === 0) {return null}
    if (compareBaselinePath && compareOptions.some((run) => run.path === compareBaselinePath)) {
      return compareBaselinePath
    }
    return findBaselineRun(detail.summary, runs)?.path ?? compareOptions[0]?.path ?? null
  }, [compareBaselinePath, compareOptions, detail, runs])

  const compareRequestKey = detail && selectedBaselinePath ? `${selectedBaselinePath}::${detail.summary.path}` : null
  const compareDiff = compareResult?.requestKey === compareRequestKey ? compareResult.diff : null
  const compareError = compareResult?.requestKey === compareRequestKey ? compareResult.error : null

  useEffect(() => {
    if (!detail || !selectedBaselinePath || !compareRequestKey) {return}
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
    const offset = detail.stages.slice(0, index).reduce((sum, item) => sum + (item.duration_ms ?? 0), 0)
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
    kind: ["llm_call", "tool_call", "pipeline"].includes(span.kind) ? span.kind : "other",
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
  const graphNodeLabels = new Map(detail.observability.action_graph_nodes.map((node) => [node.id, node.label]))
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
          <div className="compare-row" key={`${item.tool_name}-${item.args_hash}`}>
            <div>
              <strong>{item.tool_name}</strong>
              <div className="meta">tool result changed</div>
            </div>
            <div className="meta">
              <div>args {item.args_hash}</div>
              <div>left {item.left_result ?? "none"}</div>
              <div>right {item.right_result ?? "none"}</div>
            </div>
          </div>
        )),
        ...compareDiff.observability_diffs.map((item, index) => (
          <div className="compare-row" key={`${item.section}-${item.label}-${index}`}>
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

  return (
    <section className="detail">
      <header className="hero">
        <div>
          <div className="eyebrow">{intl.formatMessage(messages.run)}</div>
          <h2>{detail.summary.workflow_name}</h2>
          <p className="muted">
            {detail.summary.status} • {formatDuration(detail.summary.duration_ms)} • {detail.summary.stage_count} stages
          </p>
          <p className="mono muted">{detail.summary.path}</p>
        </div>
        <div className="hero-meta">
          {[
            [intl.formatMessage(messages.modelCalls), formatNumber(detail.summary.call_count)],
            [
              intl.formatMessage(messages.tokens),
              `${formatNumber(detail.summary.input_tokens)} in / ${formatNumber(detail.summary.output_tokens)} out`,
            ],
            [intl.formatMessage(messages.childRuns), String(detail.summary.child_run_count)],
            [intl.formatMessage(messages.started), detail.summary.started_at],
          ].map(([label, value]) => (
            <div className="card" key={label}>
              <div className="eyebrow">{label}</div>
              <div>{value}</div>
            </div>
          ))}
        </div>
      </header>

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
              <strong>Tool and side-effect ceiling</strong>
              <span className="turn-chip">{validationBadge}</span>
            </div>
            <div className="policy-list">
              <div className="meta">
                tools{" "}
                {detail.policy_summary.tools.length
                  ? detail.policy_summary.tools.join(", ")
                  : intl.formatMessage(messages.unrestricted)}
              </div>
              <div className="meta">
                side effects {detail.policy_summary.side_effect_level ?? intl.formatMessage(messages.notDeclared)}
              </div>
              <div className="meta">
                recursion{" "}
                {detail.policy_summary.recursion_limit == null
                  ? "default"
                  : String(detail.policy_summary.recursion_limit)}
              </div>
            </div>
          </div>
          <div className="policy-item">
            <div className="row">
              <strong>Capabilities and roots</strong>
              <span className="turn-chip">{detail.policy_summary.capabilities.length} ops</span>
            </div>
            <div className="policy-list">
              <div className="meta">
                {detail.policy_summary.capabilities.length
                  ? detail.policy_summary.capabilities.join(", ")
                  : intl.formatMessage(messages.noExplicitCapabilityOps)}
              </div>
              <div className="meta">
                {detail.policy_summary.workspace_roots.length
                  ? `roots ${detail.policy_summary.workspace_roots.join(", ")}`
                  : intl.formatMessage(messages.noWorkspaceRoots)}
              </div>
              <div className="meta">
                {detail.policy_summary.tool_arg_constraints.length
                  ? `arg constraints ${detail.policy_summary.tool_arg_constraints.join(" • ")}`
                  : intl.formatMessage(messages.noArgConstraints)}
              </div>
            </div>
          </div>
          <div className="policy-item">
            <div className="row">
              <strong>Validation diagnostics</strong>
              <span className="turn-chip">{detail.policy_summary.validation_errors.length} errors</span>
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
                  ? `reachable ${detail.policy_summary.reachable_nodes.join(", ")}`
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

      <section className="panel">
        <div className="panel-header">
          <div>
            <h3>{intl.formatMessage(messages.actionGraph)}</h3>
            <p>{intl.formatMessage(messages.actionGraphCopy)}</p>
          </div>
        </div>
        <div className="policy-grid">
          <div className="policy-item">
            <div className="row">
              <strong>Derived artifact</strong>
              <span className="turn-chip">schema v{detail.observability.schema_version}</span>
            </div>
            <div className="policy-list">
              <div className="meta">{detail.observability.planner_rounds.length} planner rounds</div>
              <div className="meta">{detail.observability.research_fact_count} research facts</div>
              <div className="meta">
                {detail.observability.action_graph_nodes.length} nodes • {detail.observability.action_graph_edges.length} edges
              </div>
              <div className="meta">{detail.observability.worker_lineage.length} workers</div>
              <div className="meta">{detail.observability.transcript_pointers.length} transcript pointers</div>
              <div className="meta">{detail.observability.daemon_events.length} daemon events</div>
            </div>
          </div>
          <div className="policy-item">
            <div className="row">
              <strong>Planner rounds</strong>
              <span className="turn-chip">{detail.observability.planner_rounds.length}</span>
            </div>
            <div className="policy-list">
              {detail.observability.planner_rounds.length ? (
                detail.observability.planner_rounds.map((round) => {
                  const deliverableSummary = round.task_ledger?.deliverables.length
                    ? round.task_ledger.deliverables.map((item) => `${item.id}:${item.status}`).join(", ")
                    : "no deliverables"
                  return (
                    <div className="meta" key={round.stage_id}>
                      {round.node_id} • {round.iteration_count} iterations • {round.llm_call_count} llm calls
                      {round.tool_execution_count ? ` • ${round.tool_execution_count} tool executions` : ""}
                      {round.research_facts.length ? ` • facts ${round.research_facts.join(" | ")}` : ""}
                      {deliverableSummary ? ` • ${deliverableSummary}` : ""}
                    </div>
                  )
                })
              ) : (
                <div className="muted">{intl.formatMessage(messages.noPlannerRounds)}</div>
              )}
            </div>
          </div>
          <div className="policy-item">
            <div className="row">
              <strong>Worker lineage</strong>
              <span className="turn-chip">{detail.observability.worker_lineage.length}</span>
            </div>
            <div className="policy-list">
              {detail.observability.worker_lineage.length ? (
                detail.observability.worker_lineage.map((worker) => (
                  <div className="meta" key={worker.worker_id}>
                    {worker.worker_name} • {worker.status}
                    {worker.parent_stage_id ? ` • parent ${worker.parent_stage_id}` : ""}
                    {worker.run_path ?? worker.run_id ? ` • ${worker.run_path ?? worker.run_id}` : ""}
                  </div>
                ))
              ) : (
                <div className="muted">{intl.formatMessage(messages.noWorkerLineage)}</div>
              )}
            </div>
          </div>
          <div className="policy-item">
            <div className="row">
              <strong>Transcript pointers</strong>
              <span className="turn-chip">{detail.observability.transcript_pointers.length}</span>
            </div>
            <div className="policy-list">
              {detail.observability.transcript_pointers.length ? (
                detail.observability.transcript_pointers.map((pointer) => (
                  <div className="meta" key={pointer.id}>
                    {pointer.label} • {pointer.kind} • {pointer.available ? "available" : "missing"}
                    {pointer.path ? ` • ${pointer.path}` : ` • ${pointer.location}`}
                  </div>
                ))
              ) : (
                <div className="muted">{intl.formatMessage(messages.noTranscriptPointers)}</div>
              )}
            </div>
          </div>
          <div className="policy-item">
            <div className="row">
              <strong>Daemons</strong>
              <span className="turn-chip">{detail.observability.daemon_events.length}</span>
            </div>
            <div className="policy-list">
              {detail.observability.daemon_events.length ? (
                detail.observability.daemon_events.map((event, index) => (
                  <div className="meta" key={`${event.daemon_id}-${event.kind}-${event.timestamp}-${index}`}>
                    {event.name} • {daemonKindLabel(event.kind)} • {event.timestamp}
                    {event.persist_path ? ` • ${event.persist_path}` : ""}
                    {event.payload_summary ? ` • ${event.payload_summary}` : ""}
                  </div>
                ))
              ) : (
                <div className="muted">{intl.formatMessage(messages.noDaemonEvents)}</div>
              )}
            </div>
          </div>
        </div>
        <div className="flow-grid">
          <div className="flow-item">
            <div className="row">
              <strong>Graph edges</strong>
              <span className="turn-chip">{detail.observability.action_graph_edges.length}</span>
            </div>
            {detail.observability.action_graph_edges.length ? (
              detail.observability.action_graph_edges.slice(0, 16).map((edge, index) => (
                <div className="meta" key={`${edge.from_id}-${edge.to_id}-${index}`}>
                  {graphNodeLabels.get(edge.from_id) ?? edge.from_id} → {graphNodeLabels.get(edge.to_id) ?? edge.to_id}
                  {edge.label ? ` • ${edge.label}` : ""}
                  {edge.kind ? ` • ${edge.kind}` : ""}
                </div>
              ))
            ) : (
              <div className="muted">{intl.formatMessage(messages.noTransitions)}</div>
            )}
          </div>
          <div className="flow-item">
            <div className="row">
              <strong>Verification outcomes</strong>
              <span className="turn-chip">{detail.observability.verification_outcomes.length}</span>
            </div>
            {detail.observability.verification_outcomes.length ? (
              detail.observability.verification_outcomes.map((item) => (
                <div className="meta" key={item.stage_id}>
                  {item.node_id} • {item.passed == null ? item.status : item.passed ? "passed" : "failed"}
                  {item.summary ? ` • ${item.summary}` : ""}
                </div>
              ))
            ) : (
              <div className="muted">{intl.formatMessage(messages.noValidationReport)}</div>
            )}
          </div>
        </div>
      </section>

      <section className="panel">
        <div className="panel-header">
          <div>
            <h3>{intl.formatMessage(messages.replayEval)}</h3>
            <p>{intl.formatMessage(messages.replayEvalCopy)}</p>
          </div>
        </div>
        {!detail.replay_summary ? (
          <div className="muted">{intl.formatMessage(messages.noReplayFixture)}</div>
        ) : (
          <div className="policy-grid">
            <div className="policy-item">
              <div className="row">
                <strong>Fixture identity</strong>
                <span className="turn-chip">{detail.replay_summary.stage_assertions.length} stage checks</span>
              </div>
              <div className="policy-list">
                <div className="meta">fixture {detail.replay_summary.fixture_id}</div>
                <div className="meta">source run {detail.replay_summary.source_run_id}</div>
                <div className="meta">created {detail.replay_summary.created_at}</div>
                <div className="meta">expected status {detail.replay_summary.expected_status}</div>
              </div>
            </div>
            <div className="policy-item">
              <div className="row">
                <strong>Replay assertions</strong>
                <span className="turn-chip">{detail.replay_summary.stage_assertions.length}</span>
              </div>
              <div className="policy-list">
                {detail.replay_summary.stage_assertions.map((assertion) => (
                  <div className="meta" key={assertion.node_id}>
                    {assertion.node_id} → {assertion.expected_status}/{assertion.expected_outcome}
                    {assertion.expected_branch ? ` • branch ${assertion.expected_branch}` : ""}
                    {assertion.required_artifact_kinds.length
                      ? ` • artifacts ${assertion.required_artifact_kinds.join(", ")}`
                      : ""}
                    {assertion.visible_text_contains ? ` • text contains ${assertion.visible_text_contains}` : ""}
                  </div>
                ))}
              </div>
            </div>
            <div className="policy-item">
              <div className="row">
                <strong>CLI next steps</strong>
                <span className="turn-chip">{detail.summary.path}</span>
              </div>
              <div className="policy-list">
                <div className="meta">{intl.formatMessage(messages.replayCommand)}</div>
                <pre>{`harn replay .harn-runs/${detail.summary.path}`}</pre>
                <div className="meta">{intl.formatMessage(messages.evalCommand)}</div>
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
                <strong>Run lineage</strong>
                <span className={`pill ${statusClass(detail.summary.status)}`}>{detail.summary.status}</span>
              </div>
              <div className="meta">run {detail.summary.id}</div>
              <div className="meta">root {detail.root_run_id ?? detail.summary.id}</div>
              <div className="meta">parent {detail.parent_run_id ?? "none"}</div>
            </div>
            <div className="lineage-item">
              <div className="row">
                <strong>Execution context</strong>
                <span className="turn-chip">{detail.execution_summary?.adapter ?? "unknown adapter"}</span>
              </div>
              <div className="meta">
                {detail.execution_summary?.repo_path ??
                  detail.execution_summary?.worktree_path ??
                  detail.execution_summary?.cwd ??
                  "No repo or cwd persisted"}
              </div>
              <div className="meta">branch {detail.execution_summary?.branch ?? "not captured"}</div>
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
                <strong>Transitions</strong>
                <span className="turn-chip">{detail.transitions.length}</span>
              </div>
              {detail.transitions.length ? (
                detail.transitions.slice(0, 8).map((item, index) => (
                  <div className="meta" key={`${item.to_node_id}-${index}`}>
                    {item.from_node_id ?? "start"} → {item.to_node_id}
                    {item.branch ? ` • branch ${item.branch}` : ""} • {item.produced_count} produced
                  </div>
                ))
              ) : (
                <div className="muted">{intl.formatMessage(messages.noTransitions)}</div>
              )}
            </div>
            <div className="flow-item">
              <div className="row">
                <strong>Checkpoints</strong>
                <span className="turn-chip">{detail.checkpoints.length}</span>
              </div>
              {detail.checkpoints.length ? (
                detail.checkpoints.slice(0, 8).map((item, index) => (
                  <div className="meta" key={`${item.reason}-${index}`}>
                    {item.reason} • {item.ready_count} ready • {item.completed_count} completed
                    {item.last_stage_id ? ` • after ${item.last_stage_id}` : ""}
                  </div>
                ))
              ) : (
                <div className="muted">{intl.formatMessage(messages.noCheckpoints)}</div>
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
          <div className="muted">{intl.formatMessage(messages.noCompareCandidates)}</div>
        ) : (
          <div className="compare-panel">
            <div className="compare-summary">
              <label className="search compare-field">
                <span>{intl.formatMessage(messages.baselineRun)}</span>
                <select
                  className="compare-select"
                  value={selectedBaselinePath ?? ""}
                  onChange={(event) => setCompareBaselinePath(event.target.value)}
                >
                  {compareOptions.map((run) => (
                    <option key={run.path} value={run.path}>
                      {run.started_at} • {run.path}
                    </option>
                  ))}
                </select>
              </label>
              <div className="compare-badges">
                <span className="turn-chip">current {detail.summary.path}</span>
                {selectedBaselinePath ? <span className="turn-chip">baseline {selectedBaselinePath}</span> : null}
                {compareDiff ? (
                  <>
                    <span className="turn-chip">{compareDiff.identical ? "identical" : "changed"}</span>
                    <span className="turn-chip">
                      {compareDiff.left_status} → {compareDiff.right_status}
                    </span>
                    <span className="turn-chip">{compareDiff.stage_diffs.length} stage diffs</span>
                    <span className="turn-chip">{compareDiff.tool_diffs.length} tool diffs</span>
                    <span className="turn-chip">{compareDiff.observability_diffs.length} observability diffs</span>
                    <span className="turn-chip">
                      {compareDiff.transition_count_delta >= 0 ? "+" : ""}
                      {compareDiff.transition_count_delta} transitions
                    </span>
                    <span className="turn-chip">
                      {compareDiff.artifact_count_delta >= 0 ? "+" : ""}
                      {compareDiff.artifact_count_delta} artifacts
                    </span>
                    <span className="turn-chip">
                      {compareDiff.checkpoint_count_delta >= 0 ? "+" : ""}
                      {compareDiff.checkpoint_count_delta} checkpoints
                    </span>
                  </>
                ) : null}
              </div>
            </div>
            {compareError ? (
              <div className="muted">
                {intl.formatMessage(messages.comparisonFailed, { message: compareError })}
              </div>
            ) : compareRows.length ? (
              compareRows
            ) : (
              <div className="muted">
                {compareDiff && !compareDiff.stage_diffs.length && !compareDiff.tool_diffs.length && !compareDiff.observability_diffs.length
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
              height: Math.max(56, (Math.max(0, ...detail.spans.map((span) => span.lane)) + 1) * 40 + 20),
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
                  <span className={`pill ${statusClass(stage.outcome || stage.status)}`}>
                    {stage.outcome || stage.status}
                  </span>
                </div>
                <div className="meta">
                  {stage.kind || "stage"}
                  {stage.branch ? ` • branch ${stage.branch}` : ""} • {formatDuration(stage.duration_ms)} •{" "}
                  {stage.attempt_count} attempts • {stage.artifact_count} artifacts
                </div>
                <div className="meta">
                  {formatNumber(stage.debug.call_count)} calls • {formatNumber(stage.debug.input_tokens)} in /{" "}
                  {formatNumber(stage.debug.output_tokens)} out
                </div>
                {stage.verification_summary ? <div className="meta">{stage.verification_summary}</div> : null}
                <details>
                  <summary>{intl.formatMessage(messages.stageInternals)}</summary>
                  <div className="stage-debug">
                    {stage.debug.worker_id ? <div className="meta">worker {stage.debug.worker_id}</div> : null}
                    <div className="meta">
                      consumed {stage.debug.consumed_artifact_ids.join(", ") || "none"}
                    </div>
                    <div className="meta">
                      produced {stage.debug.produced_artifact_ids.join(", ") || "none"}
                    </div>
                    <div className="meta">
                      selected {stage.debug.selected_artifact_ids.join(", ") || "none"}
                    </div>
                    <StageDetail label="Capability policy" value={stage.debug.capability_policy} open />
                    <StageDetail label="Input contract" value={stage.debug.input_contract} />
                    <StageDetail label="Output contract" value={stage.debug.output_contract} />
                    <StageDetail label="Model policy" value={stage.debug.model_policy} />
                    <StageDetail label="Auto compact" value={stage.debug.auto_compact} />
                    <StageDetail label="Output visibility" value={stage.debug.output_visibility} />
                    <StageDetail label="Context policy" value={stage.debug.context_policy} />
                    <StageDetail label="Retry policy" value={stage.debug.retry_policy} />
                    <StageDetail label="Prompt" value={stage.debug.prompt} />
                    <StageDetail label="System prompt" value={stage.debug.system_prompt} />
                    <StageDetail label="Rendered context" value={stage.debug.rendered_context} />
                    <StageDetail label="Saved error" value={stage.debug.error} />
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
              detail.activities.slice(0, 40).map((item) => (
                <div className="activity-item" key={`${item.label}-${item.started_offset_ms}`}>
                  <div className="row">
                    <strong>{item.label}</strong>
                    <span>{formatDuration(item.duration_ms)}</span>
                  </div>
                  <div className="meta">
                    {item.kind} • +{formatDuration(item.started_offset_ms)}
                    {item.stage_node_id ? ` • ${item.stage_node_id}` : ""}
                  </div>
                  <div className="meta">{item.summary}</div>
                </div>
              ))
            ) : (
              <div className="muted">{intl.formatMessage(messages.noActivity)}</div>
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
                  {artifact.source || artifact.stage || "unknown source"} •{" "}
                  {artifact.estimated_tokens != null
                    ? `${formatNumber(artifact.estimated_tokens)} tokens est.`
                    : "no token estimate"}{" "}
                  • lineage {artifact.lineage_count}
                </div>
                <div className="meta">{artifact.preview}</div>
                <details>
                  <summary>Artifact id</summary>
                  <pre>{artifact.id}</pre>
                </details>
              </div>
            ))
          ) : (
            <div className="muted">{intl.formatMessage(messages.noArtifacts)}</div>
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
                  <strong>Step {step.call_index}</strong>
                  <span className="pill running">iteration {step.iteration}</span>
                </div>
                <div className="meta">
                  {step.model}
                  {step.provider ? ` • ${step.provider}` : ""}
                </div>
                <div className="meta">{step.summary}</div>
                <div className="turn-chip-row">
                  <span className="turn-chip">{step.total_messages} msgs</span>
                  <span className="turn-chip">{step.kept_messages} kept</span>
                  <span className="turn-chip">{step.added_messages} added</span>
                  {step.input_tokens != null ? (
                    <span className="turn-chip">
                      {formatNumber(step.input_tokens)} in / {formatNumber(step.output_tokens ?? 0)} out
                    </span>
                  ) : null}
                  {step.span_id != null ? <span className="turn-chip">span {step.span_id}</span> : null}
                </div>
                <div className="turn-grid">
                  <div className="turn-panel">
                    <h4>What changed right now</h4>
                    {step.system_prompt ? <div className="meta">Always-on instructions present</div> : null}
                    {step.added_context.length ? (
                      step.added_context.map((message, index) => (
                        <div className="turn-message" key={`${message.role}-${index}`}>
                          <div className="role">{message.role}</div>
                          <pre>{message.content}</pre>
                        </div>
                      ))
                    ) : (
                      <div className="muted">{intl.formatMessage(messages.noAddedContext)}</div>
                    )}
                  </div>
                  <div className="turn-panel">
                    <h4>What happened next</h4>
                    {step.tool_calls.length ? (
                      <div className="turn-chip-row">
                        {step.tool_calls.map((tool) => (
                          <span className="turn-chip" key={tool}>
                            {tool}
                          </span>
                        ))}
                      </div>
                    ) : (
                      <div className="muted">{intl.formatMessage(messages.noToolCalls)}</div>
                    )}
                    {step.response_text ? (
                      <details>
                        <summary>Expand full reply</summary>
                        <pre>{step.response_text}</pre>
                      </details>
                    ) : (
                      <div className="muted">{intl.formatMessage(messages.noResponseText)}</div>
                    )}
                    {step.thinking ? (
                      <details>
                        <summary>Expand reasoning notes</summary>
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
                    <summary>Expand full text</summary>
                    <pre>{section.text}</pre>
                  </details>
                </div>
              ))
            ) : (
              <div className="muted">{intl.formatMessage(messages.noStory)}</div>
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
              detail.child_runs.map((child) => (
                <div className="child-item" key={`${child.worker_name}-${child.started_at}`}>
                  <div className="row">
                    <strong>{child.worker_name}</strong>
                    <span className={`pill ${statusClass(child.status)}`}>{child.status}</span>
                  </div>
                  <div className="meta">{child.task}</div>
                  <div className="meta">{child.run_path ?? child.run_id ?? "No persisted child run path"}</div>
                  {child.run_path ? (
                    <button className="run-card-link" onClick={() => onSelectRun(child.run_path!)} type="button">
                      {intl.formatMessage(messages.openChildRun)}
                    </button>
                  ) : null}
                </div>
              ))
            ) : (
              <div className="muted">{intl.formatMessage(messages.noChildren)}</div>
            )}
          </div>
        </section>
      </div>
    </section>
  )
}
