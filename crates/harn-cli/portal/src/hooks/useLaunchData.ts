import { useCallback, useEffect, useMemo, useState } from "react"

import { fetchLaunchJobs, fetchLaunchTargets, fetchLlmOptions, fetchPortalMeta, launchRun } from "../lib/api"
import type { PortalLaunchJob, PortalLaunchTarget, PortalLlmOptions, PortalMeta } from "../types"

export function useLaunchData() {
  const [portalMeta, setPortalMeta] = useState<PortalMeta | null>(null)
  const [llmOptions, setLlmOptions] = useState<PortalLlmOptions | null>(null)
  const [launchTargets, setLaunchTargets] = useState<PortalLaunchTarget[]>([])
  const [launchJobs, setLaunchJobs] = useState<PortalLaunchJob[]>([])
  const [loading, setLoading] = useState(false)
  const [lastError, setLastError] = useState<string | null>(null)

  const loadPortalOptions = useCallback(async () => {
    try {
      const [meta, nextLlmOptions] = await Promise.all([fetchPortalMeta(), fetchLlmOptions()])
      setPortalMeta(meta)
      setLlmOptions(nextLlmOptions)
      setLastError(null)
    } catch (error) {
      setLastError(error instanceof Error ? error.message : String(error))
    }
  }, [])

  const loadLaunchData = useCallback(async () => {
    setLoading(true)
    try {
      const [targets, jobs] = await Promise.all([fetchLaunchTargets(), fetchLaunchJobs()])
      setLaunchTargets(targets.targets)
      setLaunchJobs(jobs.jobs.sort((a, b) => b.started_at.localeCompare(a.started_at)))
      setLastError(null)
    } catch (error) {
      setLastError(error instanceof Error ? error.message : String(error))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void loadPortalOptions()
    void loadLaunchData()
  }, [loadLaunchData, loadPortalOptions])

  const hasRunningLaunches = useMemo(
    () => launchJobs.some((job) => job.status === "running"),
    [launchJobs],
  )

  useEffect(() => {
    if (!hasRunningLaunches) {
      return
    }
    const interval = window.setInterval(() => {
      if (document.visibilityState === "hidden") {
        return
      }
      void loadLaunchData()
    }, 5000)
    return () => window.clearInterval(interval)
  }, [hasRunningLaunches, loadLaunchData])

  return {
    portalMeta,
    llmOptions,
    launchTargets,
    launchJobs,
    loading,
    lastError,
    loadLaunchData,
    launchRun: async (payload: Parameters<typeof launchRun>[0]) => {
      const job = await launchRun(payload)
      await loadLaunchData()
      return job
    },
  }
}
