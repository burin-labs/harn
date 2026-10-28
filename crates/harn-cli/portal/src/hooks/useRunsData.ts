import { keepPreviousData, useQuery } from "@tanstack/react-query"

import { fetchRuns } from "../lib/api"
import { errorMessage } from "../lib/query"
import type { PortalPagination } from "../types"

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

export function useRunsData({ poll, ...params }: UseRunsDataArgs) {
  const query = useQuery({
    queryKey: ["runs", params],
    queryFn: () => fetchRuns(params),
    placeholderData: keepPreviousData,
    refetchInterval: poll ? 15000 : false,
  })
  return {
    stats: query.data?.stats ?? null,
    runs: query.data?.runs ?? [],
    filteredCount: query.data?.filtered_count ?? 0,
    pagination: query.data?.pagination ?? DEFAULT_PAGINATION,
    loading: query.isFetching,
    lastError: errorMessage(query.error),
    lastRefreshAt: query.dataUpdatedAt || null,
    loadRuns: () => query.refetch(),
  }
}
