#!/bin/sh
set -eu
printf '%s\n' "$@" > "${0%/*}/args"
cat "${0%/*}/assets.json"
