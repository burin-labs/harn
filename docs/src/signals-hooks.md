# Deliver signals to Claude Code and Codex

This guide connects running Claude Code and Codex sessions to the local signals
inbox, so a signal fired on this machine reaches the session working on it. For
how routing and delivery work, see [Ambient signals](./concepts/ambient-signals.md).
For every flag and file, see the [signals reference](./signals-reference.md).

You need `harn` on the `PATH` that the harness uses to run hooks.

## Connect Claude Code

Add the hooks to `~/.claude/settings.json` so every session on the machine
checks in. Merge them into any `hooks` you already have:

```json
{
  "hooks": {
    "SessionStart": [
      {"hooks": [{"type": "command", "command": "harn signals hook --format claude-code --event SessionStart", "timeout": 10}]}
    ],
    "UserPromptSubmit": [
      {"hooks": [{"type": "command", "command": "harn signals hook --format claude-code --event UserPromptSubmit", "timeout": 10}]}
    ],
    "PostToolUse": [
      {"matcher": "*", "hooks": [{"type": "command", "command": "harn signals hook --format claude-code --event PostToolUse", "timeout": 10}]}
    ],
    "Stop": [
      {"hooks": [
        {"type": "command", "command": "harn signals hook --format claude-code --event Stop", "timeout": 10},
        {"type": "command", "command": "harn signals hook --format claude-code --event Stop --rewake", "asyncRewake": true}
      ]}
    ],
    "SessionEnd": [
      {"hooks": [{"type": "command", "command": "harn signals hook --format claude-code --event SessionEnd"}]}
    ]
  }
}
```

The second `Stop` entry is optional. It waits in the background while the
session is idle and wakes the session when an urgent signal arrives. The
wake carries only a fixed notice; the alert itself arrives as a system
reminder through the first `Stop` entry when the woken turn ends. It waits
nine minutes by default, inside the hook's default 600-second timeout. To watch
longer, raise both: for example `"timeout": 3600` with `--max-wait 55m`. Only
one watcher runs per session, so a session that stops repeatedly does not
collect watchers.

Start a new session, then confirm it registered:

```sh
harn signals sessions
```

## Connect Codex

Add the hooks to `~/.codex/hooks.json`:

```json
{
  "hooks": {
    "SessionStart": [
      {"hooks": [{"type": "command", "command": "harn signals hook --format codex --event SessionStart", "timeout": 10}]}
    ],
    "UserPromptSubmit": [
      {"hooks": [{"type": "command", "command": "harn signals hook --format codex --event UserPromptSubmit", "timeout": 10}]}
    ],
    "PostToolUse": [
      {"hooks": [{"type": "command", "command": "harn signals hook --format codex --event PostToolUse", "timeout": 10}]}
    ],
    "Stop": [
      {"hooks": [{"type": "command", "command": "harn signals hook --format codex --event Stop", "timeout": 10}]}
    ],
    "SessionEnd": [
      {"hooks": [{"type": "command", "command": "harn signals hook --format codex --event SessionEnd"}]}
    ]
  }
}
```

Codex asks you to review and trust new hooks before it runs them. Approve them
once, then start a new session.

Codex has no hook that can wake an idle session, and its `Stop` hook can keep a
session going only by sending a new user prompt, which an alert must never be.
An urgent signal therefore reaches a Codex session at its next prompt or tool
call.

## Choose a fallback session

A signal that matches no session's repository or working directory goes to a
session registered with the fallback role, `control` by default. Start the
session that should receive those signals with the role set:

```sh
HARN_EXT_SIGNALS_ROLE=control claude
```

Without a fallback session, unmatched signals wait in the inbox until a
matching session checks in.

## Send a test signal

From any terminal, fire a signal for a repository a running session is working
in:

```sh
harn signals fire \
  --key test:hello \
  --summary "Test signal from the terminal" \
  --source-url https://example.com/test \
  --repo OWNER/NAME
```

The session sees the reminder after its next tool call, and the hook that
delivered it acks it. List the signal's delivery:

```sh
harn signals list
```

Resolve the signal to send the matching recovery reminder to the same session:

```sh
harn signals resolve test:hello
```

## Hold signals back

Mute signals, set a minimum severity, or set quiet hours in `config.toml` in the
inbox directory. Every harness reading the inbox follows it:

```toml
min_severity = "warning"
muted = ["github.open_prs:*"]

[quiet_hours]
start = "22:00"
end = "07:00"
utc_offset = "-07:00"
```

During quiet hours only urgent signals route. Held signals wait in the inbox;
`harn signals list` shows each one as `held` with the reason.

## Run the monitor pack

The monitor pack fires and resolves signals for red default-branch CI, a pull
request backlog, offline self-hosted runners, a low GitHub rate limit, and
Sentry, Grafana, or Honeycomb alerts. Copy
`examples/triggers/ambient-signals`, set the repositories and thresholds in
`MONITORS` in its `lib.harn`, export `GITHUB_TOKEN`, add GitHub App
credentials for rate-limit entries, and serve it:

```sh
harn orchestrator serve --config path/to/ambient-signals/harn.toml
```

The GitHub monitors poll every five minutes and need no inbound network access.
