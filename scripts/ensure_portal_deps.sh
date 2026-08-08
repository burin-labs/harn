#!/bin/sh
set -e

portal_dir="crates/harn-cli/portal"

if [ ! -f "${portal_dir}/package.json" ]; then
  exit 0
fi

if [ -x "${portal_dir}/node_modules/.bin/eslint" ] \
  && [ -x "${portal_dir}/node_modules/.bin/vite" ] \
  && [ -x "${portal_dir}/node_modules/.bin/vitest" ]; then
  exit 0
fi

echo "Bootstrapping portal frontend dependencies..."

if [ -f "${portal_dir}/package-lock.json" ]; then
  npm --prefix "${portal_dir}" ci
else
  npm --prefix "${portal_dir}" install
fi
