import { useEffect, useMemo } from "react"
import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query"

import { fetchRunDetail, fetchRuns } from "../lib/api"
import { errorMessage } from "../lib/query"
import type { PortalRunDetail } from "../types"

function isFailed(status: string) {
  return ["failed", "error", "cancelled"].includes(status)
}

function isCompleted(status: string) {
  return ["complete", "completed", "success", "verified"].includes(status)
}

function isLive(status: string | undefined) {
  return status != null && !isFailed(status) && !isCompleted(status)
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

type Observability = PortalRunDetail["observability"]
type GraphDelta = Partial<Pick<Observability, "action_graph_nodes" | "action_graph_edges">>

function withGraph(detail: PortalRunDetail, delta: GraphDelta): PortalRunDetail {
  return {
    ...detail,
    observability: {
      ...detail.observability,
      action_graph_nodes: mergeActionGraphNodes(detail.observability.action_graph_nodes, delta.action_graph_nodes ?? []),
      action_graph_edges: mergeActionGraphEdges(detail.observability.action_graph_edges, delta.action_graph_edges ?? []),
    },
  }
}

export function useRunDetailData(path: string | null) {
  const queryClient = useQueryClient()
  const detailQuery = useQuery({
    queryKey: ["run-detail", path],
    // Keep streamed graph nodes the persisted snapshot has not caught up to yet.
    queryFn: async () => {
      const snapshot = await fetchRunDetail(path as string)
      const live = queryClient.getQueryData<PortalRunDetail>(["run-detail", path])
      if (!live) {
        return snapshot
      }
      const { action_graph_nodes, action_graph_edges } = withGraph(live, snapshot.observability).observability
      return { ...snapshot, observability: { ...snapshot.observability, action_graph_nodes, action_graph_edges } }
    },
    enabled: path != null,
    refetchInterval: (query) => (isLive(query.state.data?.summary.status) ? 5000 : false),
  })
  // A failed refresh drops the stale snapshot rather than showing a run that may be gone.
  const detail = detailQuery.isError ? null : (detailQuery.data ?? null)

  const workflowName = detail?.summary.workflow_name
  const compareQuery = useQuery({
    queryKey: ["runs", "compare", workflowName],
    queryFn: () => fetchRuns({ workflow: workflowName, status: "all", sort: "newest", page: 1, pageSize: 200 }),
    enabled: workflowName != null,
    placeholderData: keepPreviousData,
    refetchInterval: isLive(detail?.summary.status) ? 5000 : false,
  })

  const actionGraphTraceId = useMemo(
    () => detail?.observability.action_graph_nodes.find((node) => node.trace_id != null)?.trace_id ?? null,
    [detail],
  )

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
        queryClient.setQueryData<PortalRunDetail>(["run-detail", path], (current) =>
          current ? withGraph(current, nextObservability) : current,
        )
      } catch {
        // Ignore malformed live updates and keep the persisted snapshot.
      }
    }
    stream.addEventListener("action_graph_update", listener as EventListener)
    return () => {
      stream.removeEventListener("action_graph_update", listener as EventListener)
      stream.close()
    }
  }, [actionGraphTraceId, path, queryClient])

  return {
    detail,
    compareRuns: compareQuery.isError ? [] : (compareQuery.data?.runs ?? []),
    loading: detailQuery.isFetching,
    lastError: errorMessage(detailQuery.error),
    loadDetail: () => detailQuery.refetch(),
  }
}
