#!/bin/sh
set -eu

python3 scripts/check_rust_prompt_prose.py --self-test
python3 scripts/check_rust_prompt_prose.py
