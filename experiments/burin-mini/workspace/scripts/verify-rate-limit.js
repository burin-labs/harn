// Verify that the rate limiter exists, is exported, and is wired into the API.
//
// A sibling of verify-comment.js, and portable for the same reason: the harn
// `run` tool hands one command string to whatever shell the host picked, which
// on Windows is cmd.exe. A `./scripts/*.sh` invocation is unrunnable there --
// cmd has no `./`, no `sh`, and no `grep` -- so this check never ran on
// Windows and the run reported nothing about it (harn#7976).
//
// It fails closed. A missing file, an unreadable file, and a missing marker
// all exit non-zero, because the failure being repaired here is a verification
// that silently did not run.
const fs = require("node:fs")
const path = require("node:path")

// [file, marker] pairs, in the order the sh script checked them, so a failure
// reads the same way it used to.
const CHECKS = [
  [path.join("packages", "server", "src", "middleware", "rate-limit.ts"), null],
  [path.join("packages", "server", "src", "middleware", "index.ts"), "export { rateLimit }"],
  [path.join("packages", "server", "src", "routes", "api.ts"), "rateLimit"],
]

for (const [target, marker] of CHECKS) {
  let contents
  try {
    contents = fs.readFileSync(target, "utf8")
  } catch (error) {
    console.error(`verify-rate-limit: cannot read ${target}: ${error.message}`)
    process.exit(1)
  }
  if (marker !== null && !contents.includes(marker)) {
    console.error(`verify-rate-limit: ${target} does not mention ${marker}`)
    process.exit(1)
  }
}

console.log("rate-limit wiring looks present")
