import type {
  PortalHighlightKeywords,
  PortalCostReport,
  PortalLaunchJob,
  PortalLaunchJobList,
  PortalLlmOptions,
  PortalMeta,
  PortalLaunchTargetList,
  PortalListResponse,
  PortalRunDetail,
  PortalRunDiff,
  PortalTrustGraphResponse,
} from "../types"

async function fetchJson<T>(url: string): Promise<T> {
  const response = await fetch(url)
  if (response.ok) {
    return response.json() as Promise<T>
  }

  let message = `Request failed: ${response.status}`
  try {
    const payload = (await response.json()) as { error?: string }
    if (payload.error) {
      message = `${message} ${payload.error}`
    }
  } catch {
    // Ignore parse failures on error bodies and keep the status-based message.
  }
  throw new Error(message)
}

export function fetchRuns(params?: {
  q?: string
  workflow?: string
  status?: string
  sort?: string
  page?: number
  pageSize?: number
  skill?: string
}): Promise<PortalListResponse> {
  const search = new URLSearchParams()
  if (params?.q) {
    search.set("q", params.q)
  }
  if (params?.workflow) {
    search.set("workflow", params.workflow)
  }
  if (params?.status) {
    search.set("status", params.status)
  }
  if (params?.sort) {
    search.set("sort", params.sort)
  }
  if (params?.page) {
    search.set("page", String(params.page))
  }
  if (params?.pageSize) {
    search.set("page_size", String(params.pageSize))
  }
  if (params?.skill) {
    search.set("skill", params.skill)
  }
  const suffix = search.toString()
  return fetchJson<PortalListResponse>(`/api/runs${suffix ? `?${suffix}` : ""}`)
}

export function fetchPortalMeta(): Promise<PortalMeta> {
  return fetchJson<PortalMeta>("/api/meta")
}

export function fetchHighlightKeywords(): Promise<PortalHighlightKeywords> {
  return fetchJson<PortalHighlightKeywords>("/api/highlight/keywords")
}

export function fetchLlmOptions(): Promise<PortalLlmOptions> {
  return fetchJson<PortalLlmOptions>("/api/llm/options")
}

export function fetchCostReport(): Promise<PortalCostReport> {
  return fetchJson<PortalCostReport>("/api/costs")
}

export function fetchRunDetail(path: string): Promise<PortalRunDetail> {
  return fetchJson<PortalRunDetail>(`/api/run?path=${encodeURIComponent(path)}`)
}

export function fetchRunCompare(left: string, right: string): Promise<PortalRunDiff> {
  return fetchJson<PortalRunDiff>(
    `/api/compare?left=${encodeURIComponent(left)}&right=${encodeURIComponent(right)}`,
  )
}

export function fetchTrustGraph(params?: {
  agent?: string
  action?: string
  limit?: number
  groupedByTrace?: boolean
}): Promise<PortalTrustGraphResponse> {
  const search = new URLSearchParams()
  if (params?.agent) {
    search.set("agent", params.agent)
  }
  if (params?.action) {
    search.set("action", params.action)
  }
  if (params?.limit) {
    search.set("limit", String(params.limit))
  }
  if (params?.groupedByTrace) {
    search.set("grouped_by_trace", "true")
  }
  const suffix = search.toString()
  return fetchJson<PortalTrustGraphResponse>(`/api/trust-graph${suffix ? `?${suffix}` : ""}`)
}

export function fetchLaunchTargets(): Promise<PortalLaunchTargetList> {
  return fetchJson<PortalLaunchTargetList>("/api/launch/targets")
}

export function fetchLaunchJobs(): Promise<PortalLaunchJobList> {
  return fetchJson<PortalLaunchJobList>("/api/launch/jobs")
}

export async function launchRun(payload: {
  file_path?: string
  source?: string
  task?: string
  provider?: string
  model?: string
  env?: Record<string, string>
}): Promise<PortalLaunchJob> {
  const response = await fetch("/api/launch", {
    method: "POST",
    headers: {
      "content-type": "application/json",
    },
    body: JSON.stringify(payload),
  })
  if (response.ok) {
    return response.json() as Promise<PortalLaunchJob>
  }

  let message = `Request failed: ${response.status}`
  try {
    const payload = (await response.json()) as { error?: string }
    if (payload.error) {
      message = `${message} ${payload.error}`
    }
  } catch {
    // Keep status-based fallback.
  }
  throw new Error(message)
}

export async function replayTriggerEvent(event_id: string): Promise<PortalLaunchJob> {
  const response = await fetch("/api/trigger/replay", {
    method: "POST",
    headers: {
      "content-type": "application/json",
    },
    body: JSON.stringify({ event_id }),
  })
  if (response.ok) {
    return response.json() as Promise<PortalLaunchJob>
  }

  let message = `Request failed: ${response.status}`
  try {
    const payload = (await response.json()) as { error?: string }
    if (payload.error) {
      message = `${message} ${payload.error}`
    }
  } catch {
    // Keep status-based fallback.
  }
  throw new Error(message)
}
