import { useQuery } from "@tanstack/react-query"

import { fetchCostReport } from "../lib/api"
import { errorMessage } from "../lib/query"

export function useCostData() {
  const query = useQuery({
    queryKey: ["costs"],
    queryFn: fetchCostReport,
    refetchInterval: 15000,
  })
  return {
    report: query.data ?? null,
    loading: query.isFetching,
    lastError: errorMessage(query.error),
    lastRefreshAt: query.dataUpdatedAt || null,
    loadCosts: () => query.refetch(),
  }
}
