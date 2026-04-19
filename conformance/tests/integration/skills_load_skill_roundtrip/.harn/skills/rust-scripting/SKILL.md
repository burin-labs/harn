---
name: rust-scripting
short: Load the Rust scripting runbook when Rust automation guidance is needed
description: Load the Rust scripting runbook
when-to-use: User needs Rust scripting instructions
allowed-tools: [deploy_service]
---
# Rust Scripting

Use `cargo run --bin harn`.
Session: ${HARN_SESSION_ID}
Dir: ${HARN_SKILL_DIR}
Args: [$ARGUMENTS]
