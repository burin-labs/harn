import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"

import { fetchLaunchJobs, fetchLaunchTargets, fetchLlmOptions, fetchPortalMeta, launchRun } from "../lib/api"
import { errorMessage } from "../lib/query"

export function useLaunchData() {
  const queryClient = useQueryClient()
  const options = useQuery({
    queryKey: ["launch", "options"],
    queryFn: async () => {
      const [portalMeta, llmOptions] = await Promise.all([fetchPortalMeta(), fetchLlmOptions()])
      return { portalMeta, llmOptions }
    },
  })
  const launch = useQuery({
    queryKey: ["launch", "targets-and-jobs"],
    queryFn: async () => {
      const [targets, jobs] = await Promise.all([fetchLaunchTargets(), fetchLaunchJobs()])
      return {
        launchTargets: targets.targets,
        launchJobs: [...jobs.jobs].sort((a, b) => b.started_at.localeCompare(a.started_at)),
      }
    },
    refetchInterval: (query) => (query.state.data?.launchJobs.some((job) => job.status === "running") ? 5000 : false),
  })
  const launchMutation = useMutation({
    mutationFn: launchRun,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["launch", "targets-and-jobs"] }),
  })

  return {
    portalMeta: options.data?.portalMeta ?? null,
    llmOptions: options.data?.llmOptions ?? null,
    launchTargets: launch.data?.launchTargets ?? [],
    launchJobs: launch.data?.launchJobs ?? [],
    loading: launch.isFetching,
    lastError: errorMessage(launch.error ?? options.error),
    loadLaunchData: () => launch.refetch(),
    launchRun: launchMutation.mutateAsync,
  }
}
