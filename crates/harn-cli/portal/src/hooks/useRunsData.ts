import { useCallback, useEffect, useState } from "react"

import { fetchRuns } from "../lib/api"
import type { PortalPagination, PortalStats, RunSummary } from "../types"

export type RunStatusFilter = "all" | "active" | "completed" | "failed"
export type RunSortOrder = "newest" | "oldest" | "duration"

type UseRunsDataArgs = {
  q: string
  workflow?: string
  status: RunStatusFilter
  sort: RunSortOrder
  page: number
  pageSize: number
  poll?: boolean
  skill?: string
}

const DEFAULT_PAGINATION: PortalPagination = {
  page: 1,
  page_size: 25,
  total_pages: 1,
  total_runs: 0,
  has_previous: false,
  has_next: false,
}

export function useRunsData(args: UseRunsDataArgs) {
  const [stats, setStats] = useState<PortalStats | null>(null)
  const [runs, setRuns] = useState<RunSummary[]>([])
  const [filteredCount, setFilteredCount] = useState(0)
  const [pagination, setPagination] = useState<PortalPagination>(DEFAULT_PAGINATION)
  const [loading, setLoading] = useState(false)
  const [lastError, setLastError] = useState<string | null>(null)
  const [lastRefreshAt, setLastRefreshAt] = useState<number | null>(null)

  const loadRuns = useCallback(async () => {
    setLoading(true)
    try {
      const data = await fetchRuns({
        q: args.q,
        workflow: args.workflow,
        status: args.status,
        sort: args.sort,
        page: args.page,
        pageSize: args.pageSize,
        skill: args.skill,
      })
      setStats(data.stats)
      setRuns(data.runs)
      setFilteredCount(data.filtered_count)
      setPagination(data.pagination)
      setLastRefreshAt(Date.now())
      setLastError(null)
    } catch (error) {
      setLastError(error instanceof Error ? error.message : String(error))
    } finally {
      setLoading(false)
    }
  }, [args.page, args.pageSize, args.q, args.sort, args.status, args.workflow, args.skill])

  useEffect(() => {
    queueMicrotask(() => {
      void loadRuns()
    })
  }, [loadRuns])

  useEffect(() => {
    if (!args.poll) {
      return
    }
    const interval = window.setInterval(() => {
      if (document.visibilityState === "hidden") {
        return
      }
      void loadRuns()
    }, 15000)
    return () => window.clearInterval(interval)
  }, [args.poll, loadRuns])

  return {
    stats,
    runs,
    filteredCount,
    pagination,
    loading,
    lastError,
    lastRefreshAt,
    loadRuns,
  }
}
