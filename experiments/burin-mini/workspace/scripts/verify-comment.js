// Verify that auth-guard.ts carries its module comment.
//
// This runs on whatever shell the host picked, which on Windows is cmd.exe or
// PowerShell. The check it replaced was `grep -n 'Auth guard middleware' ...`,
// and that invocation is Unix-shell-shaped twice over: `grep` is not on a
// stock Windows PATH, and cmd.exe does not strip single quotes, so even where
// grep exists the pattern arrives as three separate arguments. The command
// therefore failed on Windows, and before harn#7915 a run whose only
// verification failed still sealed `done` -- so the Windows job was green on a
// run that had verified nothing (harn#7968).
//
// Node is the portable choice here: the workspace is a Node/TypeScript demo,
// node is on the PATH of every runner this playground executes on, and the
// invocation needs no shell quoting at all.
//
// It fails closed. A missing file, an unreadable file, or a missing marker all
// exit non-zero. The one thing this must never do is stay silent about a check
// it did not actually perform.
const fs = require("node:fs")
const path = require("node:path")

const TARGET = path.join("packages", "server", "src", "middleware", "auth-guard.ts")
const MARKER = "Auth guard middleware"

let contents
try {
  contents = fs.readFileSync(TARGET, "utf8")
} catch (error) {
  console.error(`verify-comment: cannot read ${TARGET}: ${error.message}`)
  process.exit(1)
}

const lines = contents.split(/\r?\n/)
const hits = []
for (let i = 0; i < lines.length; i += 1) {
  if (lines[i].includes(MARKER)) {
    hits.push(`${i + 1}:${lines[i]}`)
  }
}

if (hits.length === 0) {
  console.error(`verify-comment: ${TARGET} does not mention ${MARKER}`)
  process.exit(1)
}

// Same shape the grep it replaced printed, so the transcript still reads as a
// line-numbered match rather than a bare success.
for (const hit of hits) {
  console.log(hit)
}
