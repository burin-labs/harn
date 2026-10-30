import { portalText } from "../i18n/portalText"
import { useState } from "react"
import { useIntl } from "react-intl"

import { replayTriggerEvent } from "../lib/api"
import { statusClass } from "../lib/format"
import type { PortalRunDetail } from "../types"
import { runDetailMessages as messages } from "./runDetailMessages"

function daemonKindLabel(
  kind: PortalRunDetail["observability"]["daemon_events"][number]["kind"],
) {
  switch (kind) {
    case "spawned":
      return portalText("Spawned")
    case "triggered":
      return portalText("Triggered")
    case "snapshotted":
      return portalText("Snapshotted")
    case "resumed":
      return portalText("Resumed")
    case "stopped":
      return portalText("Stopped")
    default:
      return kind
  }
}

type ActionGraphNode =
  PortalRunDetail["observability"]["action_graph_nodes"][number]

function nodeMeta(node: ActionGraphNode, key: string): unknown {
  return node.metadata?.[key]
}

function nodeMetaString(node: ActionGraphNode, key: string) {
  const value = nodeMeta(node, key)
  return typeof value === "string" ? value : null
}

function nodeMetaNumber(node: ActionGraphNode, key: string) {
  const value = nodeMeta(node, key)
  return typeof value === "number" ? value : null
}

function nodeMetaBoolean(node: ActionGraphNode, key: string) {
  const value = nodeMeta(node, key)
  return typeof value === "boolean" ? value : null
}

function actionNodeAccent(kind: string) {
  switch (kind) {
    case "trigger":
      return "action-node-trigger"
    case "predicate":
    case "trigger_predicate":
      return "action-node-predicate"
    case "a2a_hop":
      return "action-node-a2a"
    case "worker_enqueue":
      return "action-node-worker"
    case "dlq":
      return "action-node-dlq"
    case "retry":
      return "action-node-retry"
    case "approval":
      return "action-node-approval"
    default:
      return "action-node-dispatch"
  }
}

function actionNodeEyebrow(kind: string) {
  switch (kind) {
    case "trigger":
      return portalText("Inbound trigger")
    case "predicate":
    case "trigger_predicate":
      return portalText("Predicate gate")
    case "dispatch":
      return portalText("Local dispatch")
    case "a2a_hop":
      return portalText("A2A hop")
    case "worker_enqueue":
      return portalText("Worker enqueue")
    case "retry":
      return portalText("Retry")
    case "approval":
      return portalText("Approval gate")
    case "dlq":
      return portalText("DLQ")
    default:
      return kind
  }
}

function replayEventIdForNode(node: ActionGraphNode) {
  return (
    nodeMetaString(node, "event_id") ??
    (node.kind === "trigger" ? node.id.replace(/^trigger:/, "") : null)
  )
}

function actionNodeFacts(node: ActionGraphNode) {
  const facts: string[] = []
  switch (node.kind) {
    case "trigger": {
      const provider = nodeMetaString(node, "provider")
      const eventKind = nodeMetaString(node, "event_kind")
      const signature = nodeMetaString(node, "signature_status")
      if (provider && eventKind) {
        facts.push(`${provider}:${eventKind}`)
      }
      if (signature) {
        facts.push(portalText("signature {value}", { value: signature }))
      }
      break
    }
    case "predicate":
    case "trigger_predicate": {
      const result = nodeMetaBoolean(node, "result")
      const costUsd = nodeMetaNumber(node, "cost_usd")
      const latencyMs = nodeMetaNumber(node, "latency_ms")
      if (result != null) {
        facts.push(result ? portalText("passed") : portalText("blocked"))
      }
      if (costUsd != null) {
        facts.push(`$${costUsd.toFixed(4)}`)
      }
      if (latencyMs != null) {
        facts.push(`${latencyMs} ms`)
      }
      break
    }
    case "dispatch":
    case "a2a_hop":
    case "worker_enqueue": {
      const targetUri = nodeMetaString(node, "target_uri")
      const targetAgent = nodeMetaString(node, "target_agent")
      const queueName = nodeMetaString(node, "queue_name")
      const taskId = nodeMetaString(node, "task_id")
      const attempt = nodeMetaNumber(node, "attempt")
      if (queueName) {
        facts.push(portalText("queue {value}", { value: queueName }))
      } else if (targetAgent) {
        facts.push(portalText("agent {value}", { value: targetAgent }))
      } else if (targetUri) {
        facts.push(targetUri)
      }
      if (attempt != null) {
        facts.push(portalText("attempt {value}", { value: attempt }))
      }
      if (taskId) {
        facts.push(portalText("task {value}", { value: taskId }))
      }
      break
    }
    case "retry": {
      const delayMs = nodeMetaNumber(node, "delay_ms")
      if (delayMs != null) {
        facts.push(portalText("delay {value} ms", { value: delayMs }))
      }
      break
    }
    case "approval": {
      const requestId = nodeMetaString(node, "request_id")
      const reason = nodeMetaString(node, "reason")
      const reviewer = Array.isArray(node.metadata.reviewers)
        ? node.metadata.reviewers[0]
        : null
      if (requestId) {
        facts.push(requestId)
      }
      if (reason) {
        facts.push(reason)
      }
      if (typeof reviewer === "string") {
        facts.push(portalText("reviewer {value}", { value: reviewer }))
      }
      break
    }
    case "dlq": {
      const attempts = nodeMetaNumber(node, "attempt_count")
      if (attempts != null) {
        facts.push(portalText("{value} attempts", { value: attempts }))
      }
      break
    }
    default:
      break
  }
  if (node.trace_id) {
    facts.push(portalText("trace {value}", { value: node.trace_id }))
  }
  return facts
}

function actionNodeDetail(node: ActionGraphNode) {
  switch (node.kind) {
    case "predicate":
    case "trigger_predicate":
      return nodeMetaString(node, "reason") ?? nodeMetaString(node, "predicate")
    case "a2a_hop":
      return nodeMetaString(node, "target_uri")
    case "worker_enqueue":
      return nodeMetaString(node, "response_topic")
    case "retry":
    case "dlq":
      return (
        nodeMetaString(node, "error") ?? nodeMetaString(node, "final_error")
      )
    case "approval":
      return (
        nodeMetaString(node, "request_id") ?? nodeMetaString(node, "reason")
      )
    default:
      return nodeMetaString(node, "dedupe_key")
  }
}

function ActionGraphNodeCard({
  node,
  replayingEventId,
  onReplay,
}: {
  node: ActionGraphNode
  replayingEventId: string | null
  onReplay: (eventId: string) => void
}) {
  const replayEventId = replayEventIdForNode(node)
  const details = actionNodeDetail(node)
  const facts = actionNodeFacts(node)

  return (
    <div
      className={`action-node-card ${actionNodeAccent(node.kind)}`}
      title={JSON.stringify(node.metadata ?? {}, null, 2)}
    >
      <div className="row">
        <div>
          <div className="eyebrow">{actionNodeEyebrow(node.kind)}</div>
          <strong>{node.label}</strong>
        </div>
        <span className={`pill ${statusClass(node.status)}`}>
          {node.status}
        </span>
      </div>
      <div className="meta">
        {node.outcome}
        {facts.length ? ` • ${facts.join(" • ")}` : ""}
      </div>
      {details ? <div className="meta">{details}</div> : null}
      {replayEventId && (node.kind === "dlq" || node.kind === "trigger") ? (
        <div className="action-node-actions">
          <button
            className="action-button action-button-inline"
            disabled={replayingEventId === replayEventId}
            onClick={() => onReplay(replayEventId)}
            type="button"
          >
            {replayingEventId === replayEventId
              ? portalText("Replaying…")
              : portalText("Replay trigger")}
          </button>
        </div>
      ) : null}
    </div>
  )
}

type ActionGraphProps = {
  observability: PortalRunDetail["observability"]
}

export function ActionGraph({ observability }: ActionGraphProps) {
  const intl = useIntl()
  const [replayingEventId, setReplayingEventId] = useState<string | null>(null)
  const [replayStatus, setReplayStatus] = useState<string | null>(null)
  const graphNodeLabels = new Map(
    observability.action_graph_nodes.map((node) => [node.id, node.label]),
  )
  const actionGraphNodes = [...observability.action_graph_nodes].sort(
    (left, right) => {
      const order = [
        "trigger",
        "predicate",
        "trigger_predicate",
        "approval",
        "dispatch",
        "a2a_hop",
        "worker_enqueue",
        "retry",
        "dlq",
      ]
      return order.indexOf(left.kind) - order.indexOf(right.kind)
    },
  )

  return (
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
            <strong>{portalText("Derived artifact")}</strong>
            <span className="turn-chip">
              {portalText("schema v{value0}", {
                value0: observability.schema_version,
              })}
            </span>
          </div>
          <div className="policy-list">
            <div className="meta">
              {portalText("{value0} planner rounds", {
                value0: observability.planner_rounds.length,
              })}
            </div>
            <div className="meta">
              {portalText("{value0} research facts", {
                value0: observability.research_fact_count,
              })}
            </div>
            <div className="meta">
              {portalText("{value0} nodes • {value1} edges", {
                value0: observability.action_graph_nodes.length,
                value1: observability.action_graph_edges.length,
              })}
            </div>
            <div className="meta">
              {portalText("{value0} workers", {
                value0: observability.worker_lineage.length,
              })}
            </div>
            <div className="meta">
              {portalText("{value0} transcript pointers", {
                value0: observability.transcript_pointers.length,
              })}
            </div>
            <div className="meta">
              {portalText("{value0} daemon events", {
                value0: observability.daemon_events.length,
              })}
            </div>
          </div>
        </div>
        <div className="policy-item">
          <div className="row">
            <strong>{portalText("Planner rounds")}</strong>
            <span className="turn-chip">
              {observability.planner_rounds.length}
            </span>
          </div>
          <div className="policy-list">
            {observability.planner_rounds.length ? (
              observability.planner_rounds.map((round) => {
                const deliverableSummary = round.task_ledger?.deliverables
                  .length
                  ? round.task_ledger.deliverables
                      .map((item) => `${item.id}:${item.status}`)
                      .join(", ")
                  : portalText("no deliverables")
                return (
                  <div className="meta" key={round.stage_id}>
                    {portalText(
                      "{value0} • {value1} iterations • {value2} llm calls{value3}{value4}{value5}",
                      {
                        value0: round.node_id,
                        value1: round.iteration_count,
                        value2: round.llm_call_count,
                        value3: round.tool_execution_count
                          ? portalText(" • {value0} tool executions", {
                              value0: round.tool_execution_count,
                            })
                          : "",
                        value4: round.research_facts.length
                          ? portalText(" • facts {value0}", {
                              value0: round.research_facts.join(" | "),
                            })
                          : "",
                        value5: deliverableSummary
                          ? ` • ${deliverableSummary}`
                          : "",
                      },
                    )}
                  </div>
                )
              })
            ) : (
              <div className="muted">
                {intl.formatMessage(messages.noPlannerRounds)}
              </div>
            )}
          </div>
        </div>
        <div className="policy-item">
          <div className="row">
            <strong>{portalText("Worker lineage")}</strong>
            <span className="turn-chip">
              {observability.worker_lineage.length}
            </span>
          </div>
          <div className="policy-list">
            {observability.worker_lineage.length ? (
              observability.worker_lineage.map((worker) => (
                <div className="meta" key={worker.worker_id}>
                  {worker.worker_name} • {worker.status}
                  {worker.parent_stage_id
                    ? portalText(" • parent {value}", {
                        value: worker.parent_stage_id,
                      })
                    : ""}
                  {(worker.run_path ?? worker.run_id)
                    ? ` • ${worker.run_path ?? worker.run_id}`
                    : ""}
                </div>
              ))
            ) : (
              <div className="muted">
                {intl.formatMessage(messages.noWorkerLineage)}
              </div>
            )}
          </div>
        </div>
        <div className="policy-item">
          <div className="row">
            <strong>{portalText("Transcript pointers")}</strong>
            <span className="turn-chip">
              {observability.transcript_pointers.length}
            </span>
          </div>
          <div className="policy-list">
            {observability.transcript_pointers.length ? (
              observability.transcript_pointers.map((pointer) => (
                <div className="meta" key={pointer.id}>
                  {pointer.label} • {pointer.kind} •{" "}
                  {pointer.available
                    ? portalText("available")
                    : portalText("missing")}
                  {pointer.path
                    ? ` • ${pointer.path}`
                    : ` • ${pointer.location}`}
                </div>
              ))
            ) : (
              <div className="muted">
                {intl.formatMessage(messages.noTranscriptPointers)}
              </div>
            )}
          </div>
        </div>
        <div className="policy-item">
          <div className="row">
            <strong>{portalText("Daemons")}</strong>
            <span className="turn-chip">
              {observability.daemon_events.length}
            </span>
          </div>
          <div className="policy-list">
            {observability.daemon_events.length ? (
              observability.daemon_events.map((event, index) => (
                <div
                  className="meta"
                  key={`${event.daemon_id}-${event.kind}-${event.timestamp}-${index}`}
                >
                  {event.name} • {daemonKindLabel(event.kind)} •{" "}
                  {event.timestamp}
                  {event.persist_path ? ` • ${event.persist_path}` : ""}
                  {event.payload_summary ? ` • ${event.payload_summary}` : ""}
                </div>
              ))
            ) : (
              <div className="muted">
                {intl.formatMessage(messages.noDaemonEvents)}
              </div>
            )}
          </div>
        </div>
      </div>
      <div className="flow-grid">
        <div className="flow-item">
          <div className="row">
            <strong>{portalText("Action graph nodes")}</strong>
            <span className="turn-chip">
              {observability.action_graph_nodes.length}
            </span>
          </div>
          {replayStatus ? <div className="meta">{replayStatus}</div> : null}
          {actionGraphNodes.length ? (
            <div className="action-node-grid">
              {actionGraphNodes.map((node) => (
                <ActionGraphNodeCard
                  key={`${node.id}-${node.status}-${node.outcome}`}
                  node={node}
                  replayingEventId={replayingEventId}
                  onReplay={(eventId) => {
                    setReplayingEventId(eventId)
                    setReplayStatus(null)
                    void replayTriggerEvent(eventId)
                      .then((job) => {
                        setReplayStatus(
                          portalText("Queued {value}", {
                            value: job.target_label,
                          }),
                        )
                      })
                      .catch((error) => {
                        setReplayStatus(
                          error instanceof Error
                            ? error.message
                            : String(error),
                        )
                      })
                      .finally(() => {
                        setReplayingEventId(null)
                      })
                  }}
                />
              ))}
            </div>
          ) : (
            <div className="muted">
              {intl.formatMessage(messages.noTransitions)}
            </div>
          )}
        </div>
        <div className="flow-item">
          <div className="row">
            <strong>{portalText("Graph edges")}</strong>
            <span className="turn-chip">
              {observability.action_graph_edges.length}
            </span>
          </div>
          {observability.action_graph_edges.length ? (
            observability.action_graph_edges.slice(0, 16).map((edge, index) => (
              <div
                className="meta"
                key={`${edge.from_id}-${edge.to_id}-${index}`}
              >
                {graphNodeLabels.get(edge.from_id) ?? edge.from_id} →{" "}
                {graphNodeLabels.get(edge.to_id) ?? edge.to_id}
                {edge.label ? ` • ${edge.label}` : ""}
                {edge.kind ? ` • ${edge.kind}` : ""}
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
            <strong>{portalText("Verification outcomes")}</strong>
            <span className="turn-chip">
              {observability.verification_outcomes.length}
            </span>
          </div>
          {observability.verification_outcomes.length ? (
            observability.verification_outcomes.map((item) => (
              <div className="meta" key={item.stage_id}>
                {item.node_id} •{" "}
                {item.passed == null
                  ? item.status
                  : item.passed
                    ? portalText("passed")
                    : portalText("failed")}
                {item.summary ? ` • ${item.summary}` : ""}
              </div>
            ))
          ) : (
            <div className="muted">
              {intl.formatMessage(messages.noValidationReport)}
            </div>
          )}
        </div>
      </div>
    </section>
  )
}
