use serde_json::{json, Value};

use super::*;

pub fn schema_json() -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&schema_value()).map(|mut text| {
        text.push('\n');
        text
    })
}

pub fn schema_value() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": PROVIDER_CATALOG_SCHEMA_ID,
        "title": "Harn provider catalog",
        "type": "object",
        "required": ["schema_version", "schema", "generated_by", "providers", "models", "aliases", "variants", "qc_defaults"],
        "properties": {
            "schema_version": {"const": PROVIDER_CATALOG_SCHEMA_VERSION},
            "schema": {"const": PROVIDER_CATALOG_SCHEMA_ID},
            "generated_by": {"type": "string"},
            "providers": {"type": "array", "items": {"$ref": "#/$defs/provider"}},
            "models": {"type": "array", "items": {"$ref": "#/$defs/model"}},
            "aliases": {"type": "array", "items": {"$ref": "#/$defs/alias"}},
            "variants": {"type": "array", "items": {"$ref": "#/$defs/variant"}},
            "routing_routes": {"type": "array", "items": {"$ref": "#/$defs/routing_route"}},
            "qc_defaults": {"type": "object", "additionalProperties": {"type": "string"}}
        },
        "additionalProperties": false,
        "$defs": {
            "provider": {
                "type": "object",
                "required": ["id", "display_name", "classification", "endpoint", "auth", "protocols", "features", "caveats"],
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "display_name": {"type": "string", "minLength": 1},
                    "icon": {"type": "string"},
                    "classification": {"enum": ["hosted", "local"]},
                    "endpoint": {"$ref": "#/$defs/endpoint"},
                    "auth": {"$ref": "#/$defs/auth"},
                    "extra_headers": {"type": "object", "additionalProperties": {"type": "string"}},
                    "healthcheck": {"$ref": "#/$defs/healthcheck"},
                    "protocols": {"type": "array", "items": {"type": "string"}},
                    "features": {"type": "array", "items": {"type": "string"}},
                    "caveats": {"type": "array", "items": {"type": "string"}},
                    "rpm": {"type": "integer", "minimum": 1},
                    "rate_limits": {"$ref": "#/$defs/rate_limits"},
                    "local_runtime": {"$ref": "#/$defs/local_runtime"},
                    "latency_p50_ms": {"type": "integer", "minimum": 0},
                    "performance": {"$ref": "#/$defs/performance"}
                },
                "additionalProperties": false
            },
            "local_runtime": {
                "type": "object",
                "required": ["kind"],
                "properties": {
                    "kind": {"enum": ["daemon_api", "managed_process", "external"]},
                    "command": {"type": "string", "minLength": 1},
                    "prefix_args": {"type": "array", "items": {"type": "string"}},
                    "model_source": {"type": "string"},
                    "model_source_env": {"type": "string"},
                    "default_port": {"type": "integer", "minimum": 1, "maximum": 65535},
                    "model_arg": {"type": "string"},
                    "served_model_arg": {"type": "string"},
                    "host_arg": {"type": "string"},
                    "port_arg": {"type": "string"},
                    "ctx_arg": {"type": "string"},
                    "parallel_arg": {"type": "string"},
                    "gpu_layers_arg": {"type": "string"},
                    "cache_type_k_arg": {"type": "string"},
                    "cache_type_v_arg": {"type": "string"},
                    "cache_ram_arg": {"type": "string"},
                    "enable_lora_arg": {"type": "string"},
                    "lora_modules_arg": {"type": "string"},
                    "lora_modules_value_format": {"enum": ["name_path", "json_with_base_model"]},
                    "max_lora_rank_arg": {"type": "string"},
                    "default_args": {"type": "array", "items": {"type": "string"}},
                    "stop": {"enum": ["keep_alive_zero", "pid", "external"]},
                    "source_url": {"type": "string"},
                    "last_verified": {"type": "string"},
                    "notes": {"type": "string"}
                },
                "additionalProperties": false
            },
            "local_memory": {
                "type": "object",
                "properties": {
                    "measured_resident_gib": {"type": "number", "exclusiveMinimum": 0},
                    "measured_context_window": {"type": "integer", "minimum": 1},
                    "measured_cache_type": {"type": "string", "minLength": 1},
                    "base_resident_gib": {"type": "number", "exclusiveMinimum": 0},
                    "kv_cache_gib_per_1k_ctx": {"type": "number", "minimum": 0},
                    "cache_type_multipliers": {"type": "object", "additionalProperties": {"type": "number", "exclusiveMinimum": 0}},
                    "default_cache_type": {"type": "string", "minLength": 1},
                    "safety_margin_gib": {"type": "number", "minimum": 0},
                    "max_recommended_context": {"type": "integer", "minimum": 1},
                    "source_url": {"type": "string"},
                    "last_verified": {"type": "string"},
                    "notes": {"type": "string"}
                },
                "additionalProperties": false
            },
            "endpoint": {
                "type": "object",
                "required": ["base_url", "chat_endpoint"],
                "properties": {
                    "base_url": {"type": "string"},
                    "base_url_env": {"type": "string"},
                    "region_env": {"type": "string"},
                    "regions": {
                        "type": "object",
                        "additionalProperties": {"$ref": "#/$defs/endpoint_region"}
                    },
                    "chat_endpoint": {"type": "string", "minLength": 1},
                    "completion_endpoint": {"type": "string"}
                },
                "additionalProperties": false
            },
            "endpoint_region": {
                "type": "object",
                "required": ["base_url"],
                "properties": {
                    "base_url": {"type": "string", "minLength": 1},
                    "label": {"type": "string", "minLength": 1},
                    "source_url": {"type": "string", "minLength": 1},
                    "last_verified": {"type": "string", "minLength": 1},
                    "notes": {"type": "string", "minLength": 1}
                },
                "additionalProperties": false
            },
            "auth": {
                "type": "object",
                "required": ["style", "env", "required"],
                "properties": {
                    "style": {"type": "string"},
                    "header": {"type": "string"},
                    "env": {"type": "array", "items": {"type": "string"}},
                    "required": {"type": "boolean"}
                },
                "additionalProperties": false
            },
            "healthcheck": {
                "type": "object",
                "required": ["method"],
                "properties": {
                    "method": {"type": "string", "minLength": 1},
                    "path": {"type": "string", "minLength": 1},
                    "url": {"type": "string", "minLength": 1},
                    "body": {"type": "string", "minLength": 1}
                },
                "anyOf": [
                    {"required": ["path"]},
                    {"required": ["url"]}
                ],
                "additionalProperties": false
            },
            "alias": {
                "type": "object",
                "required": ["name", "model_id", "provider"],
                "properties": {
                    "name": {"type": "string", "minLength": 1},
                    "model_id": {"type": "string", "minLength": 1},
                    "provider": {"type": "string", "minLength": 1},
                    "tool_format": {"type": "string"},
                    "tool_calling": {
                        "type": "object",
                        "properties": {
                            "native": {"type": "string"},
                            "text": {"type": "string"},
                            "streaming_native": {"type": "string"},
                            "fallback_mode": {"type": "string"},
                            "failure_reason": {"type": "string"},
                            "last_probe_at": {"type": "string"}
                        },
                        "additionalProperties": false
                    }
                },
                "additionalProperties": false
            },
            "model": {
                "type": "object",
                "required": [
                    "id",
                    "name",
                    "provider",
                    "aliases",
                    "context_window",
                    "modalities",
                    "tool_support",
                    "structured_output",
                    "format_preferences",
                    "reasoning",
                    "prompt_cache",
                    "deprecation",
                    "availability",
                    "quality_tags",
                    "capability_tags",
                    "family",
                    "lineage",
                    "tier"
                ],
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "name": {"type": "string", "minLength": 1},
                    "provider": {"type": "string", "minLength": 1},
                    "aliases": {"type": "array", "items": {"type": "string"}},
                    "context_window": {"type": "integer", "minimum": 1},
                    "logical_model": {"type": "string", "minLength": 1},
                    "equivalence_group": {"type": "string", "minLength": 1},
                    "served_variant": {"type": "string", "minLength": 1},
                    "wire_model": {"type": "string", "minLength": 1},
                    "api_dialect": {"type": "string", "minLength": 1},
                    "rate_limits": {"$ref": "#/$defs/rate_limits"},
                    "performance": {"$ref": "#/$defs/performance"},
                    "architecture": {"$ref": "#/$defs/architecture"},
                    "local_memory": {"$ref": "#/$defs/local_memory"},
                    "runtime_context_window": {"type": "integer", "minimum": 1},
                    "stream_timeout": {"type": "number", "exclusiveMinimum": 0},
                    "modalities": {"$ref": "#/$defs/modalities"},
                    "tool_support": {"$ref": "#/$defs/tool_support"},
                    "structured_output": {"type": "string"},
                    "format_preferences": {"$ref": "#/$defs/format_preferences"},
                    "reasoning": {"$ref": "#/$defs/reasoning"},
                    "prompt_cache": {"type": "boolean"},
                    "batch": {"$ref": "#/$defs/model_batch_support"},
                    "pricing": {"$ref": "#/$defs/pricing"},
                    "deprecation": {"$ref": "#/$defs/deprecation"},
                    "availability": {"enum": ["serverless", "dedicated", "unknown"]},
                    "quality_tags": {"type": "array", "items": {"type": "string"}},
                    "capability_tags": {"type": "array", "items": {"type": "string"}},
                    "family": {"type": "string", "pattern": "^[a-z0-9][a-z0-9-]*$"},
                    "lineage": {"type": "string", "pattern": "^[a-z0-9][a-z0-9-]*$"},
                    "complementary_with": {"type": "array", "items": {"type": "string", "pattern": "^[a-z0-9][a-z0-9-]*$"}},
                    "avoid_as_reviewer_for": {"type": "array", "items": {"type": "string", "minLength": 1}},
                    "tier": {"enum": ["small", "mid", "frontier", "reasoning"]},
                    "open_weight": {"type": "boolean"},
                    "strengths": {"type": "array", "items": {"type": "string"}},
                    "benchmarks": {"type": "object", "additionalProperties": {"type": "number"}},
                    "serving_tiers": {
                        "type": "array",
                        "items": {"$ref": "#/$defs/serving_tier"}
                    }
                },
                "additionalProperties": false
            },
            "modalities": {
                "type": "object",
                "required": ["input", "output"],
                "properties": {
                    "input": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                    "output": {"type": "array", "items": {"type": "string"}, "minItems": 1}
                },
                "additionalProperties": false
            },
            "tool_support": {
                "type": "object",
                "required": ["native", "text", "tool_search"],
                "properties": {
                    "native": {"type": "boolean"},
                    "text": {"type": "boolean"},
                    "preferred_format": {"type": "string"},
                    "parity": {"type": "string"},
                    "parity_notes": {"type": "string"},
                    "empirical_parity": {"$ref": "#/$defs/tool_empirical_parity"},
                    "tool_search": {"type": "array", "items": {"type": "string"}},
                    "max_tools": {"type": "integer", "minimum": 1}
                },
                "additionalProperties": false
            },
            "tool_empirical_parity": {
                "type": "object",
                "required": [
                    "verdict",
                    "preferred_format",
                    "confidence",
                    "sample_size",
                    "last_evaluated",
                    "native_pass_rate",
                    "text_pass_rate",
                    "verifier_divergence_rate"
                ],
                "properties": {
                    "verdict": {"type": "string"},
                    "preferred_format": {"type": "string"},
                    "confidence": {"type": "string"},
                    "sample_size": {"type": "integer", "minimum": 1},
                    "last_evaluated": {"type": "string", "minLength": 1},
                    "native_pass_rate": {"type": "number", "minimum": 0, "maximum": 1},
                    "text_pass_rate": {"type": "number", "minimum": 0, "maximum": 1},
                    "verifier_divergence_rate": {"type": "number", "minimum": 0, "maximum": 1}
                },
                "additionalProperties": false
            },
            "model_batch_support": {
                "type": "object",
                "required": [
                    "wire_format",
                    "input_mode",
                    "result_ordering",
                    "partial_failure",
                    "cancellation"
                ],
                "properties": {
                    "wire_format": {
                        "enum": [
                            "openai",
                            "anthropic_messages",
                            "gemini",
                            "mistral",
                            "fireworks",
                            "xai"
                        ]
                    },
                    "input_mode": {
                        "enum": [
                            "jsonl_file",
                            "inline_requests",
                            "jsonl_or_inline"
                        ]
                    },
                    "result_ordering": {
                        "enum": ["custom_id_rejoin", "provider_ordered", "unknown"]
                    },
                    "partial_failure": {
                        "enum": ["per_request", "whole_batch", "unknown"]
                    },
                    "cancellation": {
                        "enum": ["supported", "not_supported", "unknown"]
                    },
                    "discount_percent": {"type": "integer", "minimum": 0, "maximum": 100},
                    "turnaround_hours": {"type": "integer", "minimum": 1},
                    "max_requests": {"type": "integer", "minimum": 1},
                    "max_input_bytes": {"type": "integer", "minimum": 1},
                    "result_retention_days": {"type": "integer", "minimum": 1},
                    "security_notes": {
                        "type": "array",
                        "items": {"type": "string", "minLength": 1}
                    },
                    "operational_notes": {
                        "type": "array",
                        "items": {"type": "string", "minLength": 1}
                    }
                },
                "additionalProperties": false
            },
            "format_preferences": {
                "type": "object",
                "required": [
                    "prefers_xml_scaffolding",
                    "prefers_markdown_scaffolding",
                    "structured_output_mode",
                    "supports_assistant_prefill",
                    "prefers_role_developer",
                    "prefers_xml_tools",
                    "thinking_block_style"
                ],
                "properties": {
                    "prefers_xml_scaffolding": {"type": "boolean"},
                    "prefers_markdown_scaffolding": {"type": "boolean"},
                    "structured_output_mode": {"enum": ["native_json", "delimited", "xml_tagged", "none"]},
                    "supports_assistant_prefill": {"type": "boolean"},
                    "prefers_role_developer": {"type": "boolean"},
                    "prefers_xml_tools": {"type": "boolean"},
                    "thinking_block_style": {"enum": ["none", "thinking_blocks", "reasoning_summary", "inline"]}
                },
                "additionalProperties": false
            },
            "reasoning": {
                "type": "object",
                "required": ["modes", "effort_supported", "none_supported", "interleaved_supported", "preserve_thinking"],
                "properties": {
                    "modes": {"type": "array", "items": {"type": "string"}},
                    "effort_supported": {"type": "boolean"},
                    "none_supported": {"type": "boolean"},
                    "interleaved_supported": {"type": "boolean"},
                    "preserve_thinking": {"type": "boolean"}
                },
                "additionalProperties": false
            },
            "pricing": {
                "type": "object",
                "required": ["input_per_mtok", "output_per_mtok"],
                "properties": {
                    "input_per_mtok": {"type": "number", "minimum": 0},
                    "output_per_mtok": {"type": "number", "minimum": 0},
                    "cache_read_per_mtok": {"type": ["number", "null"], "minimum": 0},
                    "cache_write_per_mtok": {"type": ["number", "null"], "minimum": 0},
                    "input_token_bands": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["minimum_input_tokens", "input_multiplier", "output_multiplier"],
                            "properties": {
                                "minimum_input_tokens": {"type": "integer", "minimum": 1},
                                "input_multiplier": {"type": "number", "exclusiveMinimum": 0},
                                "output_multiplier": {"type": "number", "exclusiveMinimum": 0}
                            },
                            "additionalProperties": false
                        }
                    }
                },
                "additionalProperties": false
            },
            "rate_limits": {
                "type": "object",
                "properties": {
                    "rpm": {"type": "integer", "minimum": 1},
                    "rph": {"type": "integer", "minimum": 1},
                    "rpd": {"type": "integer", "minimum": 1},
                    "tpm": {"type": "integer", "minimum": 1},
                    "tph": {"type": "integer", "minimum": 1},
                    "tpd": {"type": "integer", "minimum": 1},
                    "input_tpm": {"type": "integer", "minimum": 1},
                    "output_tpm": {"type": "integer", "minimum": 1},
                    "concurrency": {"type": "integer", "minimum": 1},
                    "tier": {"type": "string", "minLength": 1},
                    "source_url": {"type": "string", "minLength": 1},
                    "last_verified": {"type": "string", "minLength": 1},
                    "notes": {"type": "string", "minLength": 1}
                },
                "additionalProperties": false
            },
            "performance": {
                "type": "object",
                "properties": {
                    "observed_ttft_ms": {"type": "integer", "minimum": 0},
                    "output_tokens_per_sec": {"type": "number", "exclusiveMinimum": 0},
                    "time_to_answer_s": {"type": "number", "exclusiveMinimum": 0},
                    "source": {"type": "string", "minLength": 1},
                    "source_url": {"type": "string", "minLength": 1},
                    "last_verified": {"type": "string", "minLength": 1},
                    "sample_size": {"type": "integer", "minimum": 1},
                    "notes": {"type": "string", "minLength": 1}
                },
                "additionalProperties": false
            },
            "architecture": {
                "type": "object",
                "properties": {
                    "parameter_count_b": {"type": "number", "exclusiveMinimum": 0},
                    "active_parameter_count_b": {"type": "number", "exclusiveMinimum": 0},
                    "moe": {"type": "boolean"},
                    "quantization": {"type": "string", "minLength": 1},
                    "precision": {"type": "string", "minLength": 1},
                    "license": {"type": "string", "minLength": 1},
                    "tokenizer": {"type": "string", "minLength": 1},
                    "knowledge_cutoff": {"type": "string", "minLength": 1},
                    "source_url": {"type": "string", "minLength": 1},
                    "last_verified": {"type": "string", "minLength": 1}
                },
                "additionalProperties": false
            },
            "serving_tier_request": {
                "type": "object",
                "required": ["param", "value"],
                "properties": {
                    "param": {"type": "string", "minLength": 1},
                    "value": {"type": "string", "minLength": 1},
                    "beta_header": {"type": "string", "minLength": 1}
                },
                "additionalProperties": false
            },
            "serving_tier": {
                "type": "object",
                "required": ["id", "mode", "economics"],
                "properties": {
                    "id": {"type": "string", "pattern": "^[a-z0-9][a-z0-9_-]*$"},
                    "label": {"type": "string", "minLength": 1},
                    "mode": {"enum": ["synchronous"]},
                    "economics": {"enum": ["discounted", "standard", "premium"]},
                    "request": {"$ref": "#/$defs/serving_tier_request"},
                    "otps_speedup": {"type": "number", "exclusiveMinimum": 0},
                    "cost_multiplier": {"type": "number", "exclusiveMinimum": 0},
                    "discount_percent": {"type": "integer", "minimum": 0, "maximum": 100},
                    "status": {"type": "string"},
                    "pricing": {"$ref": "#/$defs/pricing"},
                    "latency": {"type": "string", "minLength": 1},
                    "reliability": {"type": "string", "minLength": 1},
                    "quota": {"type": "string", "minLength": 1},
                    "suitable_workloads": {"type": "array", "items": {"type": "string", "minLength": 1}},
                    "unsuitable_workloads": {"type": "array", "items": {"type": "string", "minLength": 1}},
                    "note": {"type": "string"}
                },
                "additionalProperties": false
            },
            "deprecation": {
                "type": "object",
                "required": ["status"],
                "properties": {
                    "status": {"enum": ["active", "deprecated"]},
                    "note": {"type": "string"},
                    "superseded_by": {"type": "string"}
                },
                "additionalProperties": false
            },
            "variant": {
                "type": "object",
                "required": ["id", "label", "description", "model_id", "provider", "source"],
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "label": {"type": "string", "minLength": 1},
                    "description": {"type": "string"},
                    "model_id": {"type": "string", "minLength": 1},
                    "provider": {"type": "string", "minLength": 1},
                    "source": {"type": "string", "minLength": 1}
                },
                "additionalProperties": false
            },
            "routing_route": {
                "type": "object",
                "required": ["provider", "model"],
                "properties": {
                    "provider": {"type": "string", "minLength": 1},
                    "model": {"type": "string", "minLength": 1},
                    "base_url": {"type": "string", "minLength": 1},
                    "secret_env": {"type": "string", "minLength": 1},
                    "timeout_ms": {"type": "integer", "minimum": 1},
                    "label": {"type": "string", "minLength": 1},
                    "family": {"type": "string", "pattern": "^[a-z0-9][a-z0-9-]*$"},
                    "capabilities": {"type": "array", "items": {"type": "string", "minLength": 1}}
                },
                "additionalProperties": false
            }
        }
    })
}
