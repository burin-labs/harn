#!/bin/sh
# A verifier that deliberately outlives a short foreground budget.
#
# Real test suites are slow because they compile and boot, not because they
# sleep, but the lifecycle the agent loop has to survive is identical: the
# command does not answer inline, so its exit status arrives only on the wait
# that resolves its handle. Sleeping keeps that lifecycle reproducible and free.
set -eu

sleep "${MINI_VERIFY_SLEEP_SECONDS:-2}"

test -f packages/server/src/middleware/rate-limit.ts
grep -q 'export { rateLimit }' packages/server/src/middleware/index.ts
echo "slow verifier passed"
