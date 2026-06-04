#!/bin/sh
set -eu

cargo run --quiet --bin harn -- run scripts/check_rust_prompt_prose.harn -- --self-test
cargo run --quiet --bin harn -- run scripts/check_rust_prompt_prose.harn
