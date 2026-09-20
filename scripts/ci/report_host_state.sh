#!/usr/bin/env bash
# Report what the machine looked like when a build step died.
#
# A step killed by a signal leaves no explanation of its own: the process is
# gone before it can say anything, and the log's last line is whatever it
# happened to be printing. Absence of an out-of-memory message is not evidence
# that memory was fine, only that nothing looked. This says what was actually
# true of the host, so the next occurrence is a reading rather than a mystery.
#
# Never fails the job. It runs after something has already failed, and a
# diagnostic that can itself fail would replace the original cause with its
# own.
set -uo pipefail

label="${1:-build step}"
echo "=== Host state after a failed ${label} ==="

echo "--- memory ---"
if command -v free >/dev/null 2>&1; then
  free -m || echo "free failed"
elif command -v vm_stat >/dev/null 2>&1; then
  vm_stat || echo "vm_stat failed"
else
  echo "no memory reporting tool on this host"
fi

echo "--- largest processes by resident size ---"
# Portable across the BSD ps on macOS and procps on Linux.
ps -eo rss=,pid=,comm= 2>/dev/null | sort -rn | head -n 15 \
  || echo "process listing unavailable"

echo "--- kernel ring buffer, last 60 lines ---"
# The kernel names an out-of-memory kill and its victim here, which is the one
# fact that separates a memory guard from a scheduler or a broker timeout. It
# is frequently unreadable without privilege, and that is itself worth saying
# rather than passing over in silence.
if sudo -n true 2>/dev/null && sudo -n dmesg >/dev/null 2>&1; then
  sudo -n dmesg | tail -n 60 || echo "dmesg failed"
elif dmesg >/dev/null 2>&1; then
  dmesg | tail -n 60 || echo "dmesg failed"
else
  echo "kernel ring buffer not readable on this host; a kill by the kernel"
  echo "cannot be confirmed or ruled out from this run"
fi

echo "--- disk ---"
df -h 2>/dev/null || echo "df unavailable"

echo "=== end host state ==="
