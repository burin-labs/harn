#!/usr/bin/env bash
# Print a timestamped host snapshot during a build or after it fails. Stream
# readings to stdout so they do not depend on a final artifact upload.
# Diagnostics are best effort and must not replace the build's exit status.
set -uo pipefail

phase="after a failed"
if [[ "${1:-}" == --during ]]; then
  phase="during"
  shift
fi
label="${1:-build step}"
printf '=== Host state %s %s at %s ===\n' "$phase" "$label" "$(date -u +%Y-%m-%dT%H:%M:%SZ)"

echo "--- memory ---"
if command -v free >/dev/null 2>&1; then
  if memory="$(free -m)" && [[ -n "$memory" ]]; then
    printf '%s\n' "$memory"
  else
    echo "free failed or returned no memory reading"
  fi
elif command -v vm_stat >/dev/null 2>&1; then
  vm_stat || echo "vm_stat failed"
else
  echo "no memory reporting tool on this host"
fi

if [[ -r /proc/vmstat ]]; then
  echo "--- kernel memory counters ---"
  awk '
    BEGIN { count = split("oom_kill pgmajfault pswpin pswpout", fields) }
    $1 == "oom_kill" || $1 == "pgmajfault" || $1 == "pswpin" || $1 == "pswpout" {
      if ($2 ~ /^[0-9]+$/) values[$1] = $2
    }
    END {
      for (i = 1; i <= count; i++)
        print fields[i], (fields[i] in values ? values[fields[i]] : "unavailable")
    }
  ' /proc/vmstat \
    || echo "kernel memory counters unavailable"
else
  echo "kernel memory counters unavailable on this host"
fi

echo "--- largest processes by resident size ---"
# Portable across the BSD ps on macOS and procps on Linux.
if processes="$(ps -eo rss=,pid=,comm= 2>/dev/null)" && [[ -n "$processes" ]]; then
  printf '%s\n' "$processes" | sort -rn | sed -n '1,15p' \
    || echo "process listing unavailable"
else
  echo "process listing unavailable"
fi

echo "--- kernel ring buffer, last 60 lines ---"
# The kernel names an out-of-memory kill and its victim here, which is the one
# fact that separates a memory guard from a scheduler or a broker timeout. It
# is frequently unreadable without privilege, and that is itself worth saying
# rather than passing over in silence.
if kernel="$(sudo -n dmesg 2>/dev/null)" && [[ -n "$kernel" ]]; then
  printf '%s\n' "$kernel" | tail -n 60 || echo "dmesg failed"
elif kernel="$(dmesg 2>/dev/null)" && [[ -n "$kernel" ]]; then
  printf '%s\n' "$kernel" | tail -n 60 || echo "dmesg failed"
else
  echo "kernel ring buffer not readable on this host; a kill by the kernel"
  echo "cannot be confirmed or ruled out from this run"
fi

echo "--- disk ---"
df -h 2>/dev/null || echo "df unavailable"

echo "=== end host state ==="
