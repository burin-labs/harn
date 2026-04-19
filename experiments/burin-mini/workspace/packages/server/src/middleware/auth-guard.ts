import type { AppContext } from "../context"

const API_KEYS: Record<string, { id: string; name: string; plan: string }> = {
  "demo-admin-key": { id: "user_admin", name: "Admin", plan: "enterprise" },
  "demo-viewer-key": { id: "user_viewer", name: "Viewer", plan: "free" },
}

export function authGuard(ctx: AppContext, next: () => Response): Response {
  const apiKey = ctx.headers["x-api-key"] ?? ""
  const user = API_KEYS[apiKey]
  if (user == nil) {
    return { status: 401, body: { error: "unauthorized" } }
  }

  ctx.state.user = user
  return next()
}
