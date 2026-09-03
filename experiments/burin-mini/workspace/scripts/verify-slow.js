// A verifier that deliberately outlives a short foreground budget.
//
// Real test suites are slow because they compile and boot, not because they
// sleep, but the lifecycle the agent loop has to survive is identical: the
// command does not answer inline, so its exit status arrives only on the wait
// that resolves its handle. Sleeping keeps that lifecycle reproducible and
// free.
//
// Node rather than sh for the reason its siblings are: cmd.exe cannot run a
// `./scripts/*.sh` command, so on Windows this verifier never ran and the run
// reported nothing about it (harn#7976). The sleep is what this fixture is
// for, so it is the one thing that must behave identically on every platform.
const fs = require("node:fs")
const path = require("node:path")

const SLEEP_SECONDS = Number.parseFloat(process.env.MINI_VERIFY_SLEEP_SECONDS ?? "2")
if (!Number.isFinite(SLEEP_SECONDS) || SLEEP_SECONDS < 0) {
  console.error(
    `verify-slow: MINI_VERIFY_SLEEP_SECONDS must be a non-negative number, got ${process.env.MINI_VERIFY_SLEEP_SECONDS}`,
  )
  process.exit(1)
}

const CHECKS = [
  [path.join("packages", "server", "src", "middleware", "rate-limit.ts"), null],
  [path.join("packages", "server", "src", "middleware", "index.ts"), "export { rateLimit }"],
]

setTimeout(() => {
  for (const [target, marker] of CHECKS) {
    let contents
    try {
      contents = fs.readFileSync(target, "utf8")
    } catch (error) {
      console.error(`verify-slow: cannot read ${target}: ${error.message}`)
      process.exit(1)
    }
    if (marker !== null && !contents.includes(marker)) {
      console.error(`verify-slow: ${target} does not mention ${marker}`)
      process.exit(1)
    }
  }
  console.log("slow verifier passed")
}, SLEEP_SECONDS * 1000)
