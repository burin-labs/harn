#!/bin/sh
set -eu
while [ "$#" -gt 0 ]; do
  if [ "$1" = --output-file ]; then
    cp "${0%/*}/raw.json" "$2"
    exit 0
  fi
  shift
done
exit 2
