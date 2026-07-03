//! `harn provider dispatch-explain <provider> <model>` — deterministically
//! explain how a (provider, model) pair would dispatch, from the capability
//! registry alone. No network, no LLM call.
//!
//! This is the static counterpart to the runtime `resolved_dispatch` transcript
//! record: instead of asking "what did this call dispatch", it answers "what
//! WOULD this pair dispatch" — so anyone can confirm "does anthropic
//! claude-sonnet route native?" without running an eval and grepping a
//! transcript.

use crate::cli::ProviderDispatchExplainArgs;

pub(crate) fn run(args: &ProviderDispatchExplainArgs) {
    let caps = harn_vm::llm::capabilities::lookup(&args.provider, &args.model);
    let wire_format = harn_vm::llm::resolved_dispatch::wire_format_for(&args.provider, &args.model);
    let base_url = harn_vm::llm_config::provider_config(&args.provider)
        .map(|def| harn_vm::llm_config::resolve_base_url(&def))
        .unwrap_or_else(|| default_base_url_for(wire_format));
    let base_url_host = host_of(&base_url);

    let tool_format = args
        .tool_format
        .clone()
        .or_else(|| caps.preferred_tool_format.clone())
        .unwrap_or_else(|| {
            if caps.native_tools {
                "native".to_string()
            } else {
                "text".to_string()
            }
        });

    // A model advertises thinking when the capability row lists any thinking
    // mode; requesting thinking against a model that advertises none is a
    // footgun (the operator asked for reasoning the route will silently drop).
    let advertises_thinking = !caps.thinking_modes.is_empty();
    let thinking_note = if args.thinking && !advertises_thinking {
        Some(format!(
            "requested --thinking but {}:{} advertises no thinking modes; the route will not return reasoning content",
            args.provider, args.model
        ))
    } else {
        None
    };

    if args.json {
        let report = serde_json::json!({
            "provider": args.provider,
            "model": args.model,
            "wire_format": wire_format,
            "message_wire_format": caps.message_wire_format,
            "native_tool_wire_format": caps.native_tool_wire_format,
            "base_url_host": base_url_host,
            "tool_format": tool_format,
            "native_tools": caps.native_tools,
            "advertises_thinking": advertises_thinking,
            "thinking_modes": caps.thinking_modes,
            "requested_thinking": args.thinking,
            "thinking_note": thinking_note,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_else(|_| report.to_string())
        );
        return;
    }

    println!("dispatch-explain {}:{}", args.provider, args.model);
    println!("  wire_format:      {wire_format}");
    println!("  message_wire:     {}", caps.message_wire_format);
    println!("  tool_wire:        {}", caps.native_tool_wire_format);
    println!("  base_url_host:    {base_url_host}");
    println!("  tool_format:      {tool_format}");
    println!("  native_tools:     {}", caps.native_tools);
    println!(
        "  thinking:         advertised={advertises_thinking}{}",
        if args.thinking { " (requested)" } else { "" }
    );
    if let Some(note) = thinking_note {
        println!("  NOTE: {note}");
    }
}

fn default_base_url_for(wire_format: &str) -> String {
    if wire_format == "anthropic_native" {
        "https://api.anthropic.com/v1".to_string()
    } else {
        "https://api.openai.com/v1".to_string()
    }
}

fn host_of(base_url: &str) -> String {
    base_url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .map(str::to_string)
        .unwrap_or_else(|| base_url.to_string())
}
