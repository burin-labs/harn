#!/bin/sh
set -eu
count=0
[ ! -f count ] || count=$(cat count)
count=$((count + 1))
printf '%s\n' "$count" > count
mode=$(cat mode)
case "$mode" in
  once) [ "$count" -gt 1 ] && exit 0 ;;
  three) [ "$count" -gt 3 ] && exit 0 ;;
  deterministic) echo 'lifecycle script failed its test' >&2; exit 19 ;;
esac
echo 'npm error ECONNRESET: socket hang up' >&2
exit 18
