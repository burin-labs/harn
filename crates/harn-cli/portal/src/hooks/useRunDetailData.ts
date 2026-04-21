import { useCallback, useEffect, useMemo, useState } from "react"

import { fetchRunDetail, fetchRuns } from "../lib/api"
import { type RunSortOrder, type RunStatusFilter } from "./useRunsData"
import type { PortalRunDetail, RunSummary } from "../types"

function isFailed(status: string) {
  return ["failed", "error", "cancelled"].includes(status)
}

function isCompleted(status: string) {
  return ["complete", "completed", "success", "verified"].includes(status)
}

function mergeActionGraphNodes(
  current: PortalRunDetail["observability"]["action_graph_nodes"],
  incoming: PortalRunDetail["observability"]["action_graph_nodes"],
) {
  const merged = new Map(current.map((node) => [node.id, node]))
  for (const node of incoming) {
    const previous = merged.get(node.id)
    merged.set(node.id, {
      ...previous,
      ...node,
      metadata: {
        ...(previous?.metadata ?? {}),
        ...(node.metadata ?? {}),
      },
    })
  }
  return [...merged.values()]
}

function mergeActionGraphEdges(
  current: PortalRunDetail["observability"]["action_graph_edges"],
  incoming: PortalRunDetail["observability"]["action_graph_edges"],
) {
  const merged = [...current]
  const seen = new Set(current.map((edge) => `${edge.from_id}|${edge.to_id}|${edge.kind}|${edge.label ?? ""}`))
  for (const edge of incoming) {
    const key = `${edge.from_id}|${edge.to_id}|${edge.kind}|${edge.label ?? ""}`
    if (!seen.has(key)) {
      seen.add(key)
      merged.push(edge)
    }
  }
  return merged
}

export function useRunDetailData(path: string | null) {
  const [detail, setDetail] = useState<PortalRunDetail | null>(null)
  const [compareRuns, setCompareRuns] = useState<RunSummary[]>([])
  const [loading, setLoading] = useState(false)
  const [lastError, setLastError] = useState<string | null>(null)

  const loadDetail = useCallback(async () => {
    if (!path) {
      setDetail(null)
      setCompareRuns([])
      return
    }
    setLoading(true)
    try {
      const nextDetail = await fetchRunDetail(path)
      setDetail(nextDetail)
      setLastError(null)
    } catch (error) {
      setDetail(null)
      setCompareRuns([])
      setLastError(error instanceof Error ? error.message : String(error))
    } finally {
      setLoading(false)
    }
  }, [path])

  useEffect(() => {
    void loadDetail()
  }, [loadDetail])

  useEffect(() => {
    if (!detail) {
      setCompareRuns([])
      return
    }
    let cancelled = false
    const workflowName = detail.summary.workflow_name

    async function loadCompareRuns() {
      try {
        const data = await fetchRuns({
          workflow: workflowName,
          status: "all" satisfies RunStatusFilter,
          sort: "newest" satisfies RunSortOrder,
          page: 1,
          pageSize: 200,
        })
        if (!cancelled) {
          setCompareRuns(data.runs)
        }
      } catch {
        if (!cancelled) {
          setCompareRuns([])
        }
      }
    }

    void loadCompareRuns()
    return () => {
      cancelled = true
    }
  }, [detail])

  const needsPolling = useMemo(() => {
    if (!detail) {
      return false
    }
    return !isFailed(detail.summary.status) && !isCompleted(detail.summary.status)
  }, [detail])
  const actionGraphTraceId = useMemo(
    () => detail?.observability.action_graph_nodes.find((node) => node.trace_id != null)?.trace_id ?? null,
    [detail],
  )

  useEffect(() => {
    if (!needsPolling || !path) {
      return
    }
    const interval = window.setInterval(() => {
      if (document.visibilityState === "hidden") {
        return
      }
      void loadDetail()
    }, 5000)
    return () => window.clearInterval(interval)
  }, [loadDetail, needsPolling, path])

  useEffect(() => {
    if (!path || !actionGraphTraceId || typeof EventSource === "undefined") {
      return
    }
    const stream = new EventSource(`/api/run/action-graph/stream?path=${encodeURIComponent(path)}`)
    const listener = (event: MessageEvent<string>) => {
      try {
        const payload = JSON.parse(event.data) as {
          payload?: {
            observability?: {
              action_graph_nodes?: PortalRunDetail["observability"]["action_graph_nodes"]
              action_graph_edges?: PortalRunDetail["observability"]["action_graph_edges"]
            }
          }
        }
        const nextObservability = payload.payload?.observability
        if (!nextObservability) {
          return
        }
        setDetail((current) => {
          if (!current) {
            return current
          }
          return {
            ...current,
            observability: {
              ...current.observability,
              action_graph_nodes: mergeActionGraphNodes(
                current.observability.action_graph_nodes,
                nextObservability.action_graph_nodes ?? [],
              ),
              action_graph_edges: mergeActionGraphEdges(
                current.observability.action_graph_edges,
                nextObservability.action_graph_edges ?? [],
              ),
            },
          }
        })
      } catch {
        // Ignore malformed live updates and keep the persisted snapshot.
      }
    }
    stream.addEventListener("action_graph_update", listener as EventListener)
    return () => {
      stream.removeEventListener("action_graph_update", listener as EventListener)
      stream.close()
    }
  }, [actionGraphTraceId, path])

  return {
    detail,
    compareRuns,
    loading,
    lastError,
    loadDetail,
  }
}
