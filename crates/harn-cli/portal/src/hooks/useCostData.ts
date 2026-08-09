import { useCallback, useEffect, useState } from "react"

import { fetchCostReport } from "../lib/api"
import type { PortalCostReport } from "../types"

export function useCostData() {
  const [report, setReport] = useState<PortalCostReport | null>(null)
  const [loading, setLoading] = useState(false)
  const [lastError, setLastError] = useState<string | null>(null)
  const [lastRefreshAt, setLastRefreshAt] = useState<number | null>(null)

  const loadCosts = useCallback(async () => {
    setLoading(true)
    try {
      const data = await fetchCostReport()
      setReport(data)
      setLastRefreshAt(Date.now())
      setLastError(null)
    } catch (error) {
      setLastError(error instanceof Error ? error.message : String(error))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    queueMicrotask(() => {
      void loadCosts()
    })
  }, [loadCosts])

  useEffect(() => {
    const interval = window.setInterval(() => {
      if (document.visibilityState === "hidden") {
        return
      }
      void loadCosts()
    }, 15000)
    return () => window.clearInterval(interval)
  }, [loadCosts])

  return { report, loading, lastError, lastRefreshAt, loadCosts }
}
