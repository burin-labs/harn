#!/bin/sh
set -eu

test -f packages/server/src/middleware/rate-limit.ts
grep -q 'export { rateLimit }' packages/server/src/middleware/index.ts
grep -q 'rateLimit' packages/server/src/routes/api.ts
echo "rate-limit wiring looks present"
