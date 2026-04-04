import type {
  PortalHighlightKeywords,
  PortalLaunchJob,
  PortalLaunchJobList,
  PortalLlmOptions,
  PortalMeta,
  PortalLaunchTargetList,
  PortalListResponse,
  PortalRunDetail,
  PortalRunDiff,
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

export function fetchRunDetail(path: string): Promise<PortalRunDetail> {
  return fetchJson<PortalRunDetail>(`/api/run?path=${encodeURIComponent(path)}`)
}

export function fetchRunCompare(left: string, right: string): Promise<PortalRunDiff> {
  return fetchJson<PortalRunDiff>(
    `/api/compare?left=${encodeURIComponent(left)}&right=${encodeURIComponent(right)}`,
  )
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
