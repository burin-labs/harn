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

  return {
    detail,
    compareRuns,
    loading,
    lastError,
    loadDetail,
  }
}
