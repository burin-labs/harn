Moved replay now refuses recorded live commands on unsupported hosts before
calling a model. Answer-only, filesystem-only, and fully served command prefixes
remain available. New full recordings measure command requests inside every
tool dispatch; older tapes remain inspectable and require re-recording to branch.

Migration: record a new full tape before branching on this runtime. Rust producers
of `TapeRecordKind::ToolCall` must supply `command_effect: Some(CommandEffect::None)`
when command requests were measured and none occurred, or
`Some(CommandEffect::Requested)` when the dispatch reached the command capability.
Missing evidence is unmeasured and cannot authorize branching.
