import { useState } from "react"
import { useIntl } from "react-intl"

import { replayTriggerEvent } from "../lib/api"
import { statusClass } from "../lib/format"
import type { PortalRunDetail } from "../types"
import { runDetailMessages as messages } from "./runDetailMessages"

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

type ActionGraphNode = PortalRunDetail["observability"]["action_graph_nodes"][number]

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
      return "Inbound trigger"
    case "predicate":
    case "trigger_predicate":
      return "Predicate gate"
    case "dispatch":
      return "Local dispatch"
    case "a2a_hop":
      return "A2A hop"
    case "worker_enqueue":
      return "Worker enqueue"
    case "retry":
      return "Retry"
    case "approval":
      return "Approval gate"
    case "dlq":
      return "DLQ"
    default:
      return kind
  }
}

function replayEventIdForNode(node: ActionGraphNode) {
  return nodeMetaString(node, "event_id") ?? (node.kind === "trigger" ? node.id.replace(/^trigger:/, "") : null)
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
        facts.push(`signature ${signature}`)
      }
      break
    }
    case "predicate":
    case "trigger_predicate": {
      const result = nodeMetaBoolean(node, "result")
      const costUsd = nodeMetaNumber(node, "cost_usd")
      const latencyMs = nodeMetaNumber(node, "latency_ms")
      if (result != null) {
        facts.push(result ? "passed" : "blocked")
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
        facts.push(`queue ${queueName}`)
      } else if (targetAgent) {
        facts.push(`agent ${targetAgent}`)
      } else if (targetUri) {
        facts.push(targetUri)
      }
      if (attempt != null) {
        facts.push(`attempt ${attempt}`)
      }
      if (taskId) {
        facts.push(`task ${taskId}`)
      }
      break
    }
    case "retry": {
      const delayMs = nodeMetaNumber(node, "delay_ms")
      if (delayMs != null) {
        facts.push(`delay ${delayMs} ms`)
      }
      break
    }
    case "approval": {
      const requestId = nodeMetaString(node, "request_id")
      const reason = nodeMetaString(node, "reason")
      const reviewer = Array.isArray(node.metadata.reviewers) ? node.metadata.reviewers[0] : null
      if (requestId) {
        facts.push(requestId)
      }
      if (reason) {
        facts.push(reason)
      }
      if (typeof reviewer === "string") {
        facts.push(`reviewer ${reviewer}`)
      }
      break
    }
    case "dlq": {
      const attempts = nodeMetaNumber(node, "attempt_count")
      if (attempts != null) {
        facts.push(`${attempts} attempts`)
      }
      break
    }
    default:
      break
  }
  if (node.trace_id) {
    facts.push(`trace ${node.trace_id}`)
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
      return nodeMetaString(node, "error") ?? nodeMetaString(node, "final_error")
    case "approval":
      return nodeMetaString(node, "request_id") ?? nodeMetaString(node, "reason")
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
    <div className={`action-node-card ${actionNodeAccent(node.kind)}`} title={JSON.stringify(node.metadata ?? {}, null, 2)}>
      <div className="row">
        <div>
          <div className="eyebrow">{actionNodeEyebrow(node.kind)}</div>
          <strong>{node.label}</strong>
        </div>
        <span className={`pill ${statusClass(node.status)}`}>{node.status}</span>
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
            {replayingEventId === replayEventId ? "Replaying…" : "Replay trigger"}
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
  const graphNodeLabels = new Map(observability.action_graph_nodes.map((node) => [node.id, node.label]))
  const actionGraphNodes = [...observability.action_graph_nodes].sort((left, right) => {
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
  })

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
            <strong>Derived artifact</strong>
            <span className="turn-chip">schema v{observability.schema_version}</span>
          </div>
          <div className="policy-list">
            <div className="meta">{observability.planner_rounds.length} planner rounds</div>
            <div className="meta">{observability.research_fact_count} research facts</div>
            <div className="meta">
              {observability.action_graph_nodes.length} nodes • {observability.action_graph_edges.length} edges
            </div>
            <div className="meta">{observability.worker_lineage.length} workers</div>
            <div className="meta">{observability.transcript_pointers.length} transcript pointers</div>
            <div className="meta">{observability.daemon_events.length} daemon events</div>
          </div>
        </div>
        <div className="policy-item">
          <div className="row">
            <strong>Planner rounds</strong>
            <span className="turn-chip">{observability.planner_rounds.length}</span>
          </div>
          <div className="policy-list">
            {observability.planner_rounds.length ? (
              observability.planner_rounds.map((round) => {
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
            <span className="turn-chip">{observability.worker_lineage.length}</span>
          </div>
          <div className="policy-list">
            {observability.worker_lineage.length ? (
              observability.worker_lineage.map((worker) => (
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
            <span className="turn-chip">{observability.transcript_pointers.length}</span>
          </div>
          <div className="policy-list">
            {observability.transcript_pointers.length ? (
              observability.transcript_pointers.map((pointer) => (
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
            <span className="turn-chip">{observability.daemon_events.length}</span>
          </div>
          <div className="policy-list">
            {observability.daemon_events.length ? (
              observability.daemon_events.map((event, index) => (
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
            <strong>Action graph nodes</strong>
            <span className="turn-chip">{observability.action_graph_nodes.length}</span>
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
                        setReplayStatus(`Queued ${job.target_label}`)
                      })
                      .catch((error) => {
                        setReplayStatus(error instanceof Error ? error.message : String(error))
                      })
                      .finally(() => {
                        setReplayingEventId(null)
                      })
                  }}
                />
              ))}
            </div>
          ) : (
            <div className="muted">{intl.formatMessage(messages.noTransitions)}</div>
          )}
        </div>
        <div className="flow-item">
          <div className="row">
            <strong>Graph edges</strong>
            <span className="turn-chip">{observability.action_graph_edges.length}</span>
          </div>
          {observability.action_graph_edges.length ? (
            observability.action_graph_edges.slice(0, 16).map((edge, index) => (
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
            <span className="turn-chip">{observability.verification_outcomes.length}</span>
          </div>
          {observability.verification_outcomes.length ? (
            observability.verification_outcomes.map((item) => (
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
  )
}
