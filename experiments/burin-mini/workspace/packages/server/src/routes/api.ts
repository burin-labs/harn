import type { AppContext } from "../context"
import { authGuard } from "../middleware"

function withMiddleware(ctx: AppContext, middleware, handler) {
  return middleware(ctx, { -> handler(ctx) })
}

export function handleApiRequest(ctx: AppContext): Response {
  return withMiddleware(ctx, authGuard, { authed_ctx ->
    return {
      status: 200,
      body: {
        ok: true,
        user: authed_ctx.state.user,
        route: "/api/projects",
      },
    }
  })
}
