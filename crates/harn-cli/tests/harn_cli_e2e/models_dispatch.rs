//! Contract checks for the Harn-rendered `harn models` subcommands.
//!
//! Each subcommand's render pipeline lives in
//! `crates/harn-stdlib/src/stdlib/cli/models/*.harn`. The Rust dispatch shims
//! keep doing host-only work and hand JSON payloads across the dispatch wedge
//! to Harn for formatting.

#[path = "models_dispatch/support.rs"]
mod support;

#[path = "models_dispatch/batch_lifecycle_providers.rs"]
mod batch_lifecycle_providers;
#[path = "models_dispatch/batch_plan_prepare.rs"]
mod batch_plan_prepare;
#[path = "models_dispatch/core.rs"]
mod core;
#[path = "models_dispatch/lora_fixtures.rs"]
mod lora_fixtures;
#[path = "models_dispatch/lora_inspect_plan.rs"]
mod lora_inspect_plan;
#[path = "models_dispatch/lora_workflows.rs"]
mod lora_workflows;
