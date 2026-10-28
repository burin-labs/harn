import { QueryClient } from "@tanstack/react-query"

// Failures surface in the UI immediately; polling intervals are the retry.
// react-query pauses `refetchInterval` while the tab is hidden.
export function createQueryClient() {
  return new QueryClient({ defaultOptions: { queries: { retry: false } } })
}

export function errorMessage(error: unknown): string | null {
  if (error == null) {
    return null
  }
  return error instanceof Error ? error.message : String(error)
}
