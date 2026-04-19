# Mini Auth Demo

A tiny TypeScript auth API demo used by the Burin Mini playground experiment.

- `packages/server/src/routes/api.ts` is the route entrypoint
- `packages/server/src/middleware/auth-guard.ts` validates `x-api-key`
- `packages/server/src/context.ts` holds request and user state types

The playground tasks explain this repo, comment the auth guard, and add rate
limiting middleware to the auth path.
