//! Canonical mappings between typed `harness.<sub>.*` methods and the
//! ambient builtin names that still back capability classification,
//! lint migrations, and runtime dispatch.

pub fn harness_stdio_ambient(method: &str) -> Option<&'static str> {
    match method {
        "print" => Some("print"),
        "println" => Some("println"),
        "eprint" => Some("eprint"),
        "eprintln" => Some("eprintln"),
        "read_line" => Some("read_line"),
        "prompt" => Some("prompt_user"),
        _ => None,
    }
}

pub fn harness_term_ambient(method: &str) -> Option<&'static str> {
    match method {
        "width" => Some("term_width"),
        "height" => Some("term_height"),
        "read_password" => Some("read_password"),
        _ => None,
    }
}

pub fn harness_clock_ambient(method: &str) -> Option<&'static str> {
    match method {
        "now_ms" => Some("now_ms"),
        "monotonic_ms" | "elapsed" => Some("monotonic_ms"),
        "sleep_ms" => Some("sleep_ms"),
        "timestamp" => Some("timestamp"),
        _ => None,
    }
}

pub fn harness_fs_ambient(method: &str) -> Option<&'static str> {
    match method {
        "read_text" | "read" | "read_file" => Some("read_file"),
        "read_text_result" | "read_result" => Some("read_file_result"),
        "read_bytes" | "read_file_bytes" => Some("read_file_bytes"),
        "write_text" | "write" | "write_file" => Some("write_file"),
        "write_bytes" | "write_file_bytes" => Some("write_file_bytes"),
        "replace_text" => Some("replace_file"),
        "replace_text_result" => Some("replace_file_result"),
        "replace_bytes" => Some("replace_file_bytes"),
        "replace_bytes_result" => Some("replace_file_bytes_result"),
        "exists" | "file_exists" => Some("file_exists"),
        "status" | "path_status" => Some("path_status"),
        "delete" | "delete_file" | "remove" => Some("delete_file"),
        "append" | "append_file" => Some("append_file"),
        "append_locked" | "append_file_locked" => Some("append_file_locked"),
        "list_dir" | "list" => Some("list_dir"),
        "mkdir" | "create_dir" => Some("mkdir"),
        "path_join" => Some("path_join"),
        "copy" | "copy_file" => Some("copy_file"),
        "temp_dir" => Some("temp_dir"),
        "workspace_temp_dir" => Some("workspace_temp_dir"),
        "mkdtemp" => Some("mkdtemp"),
        "mkdtemp_in_workspace" => Some("mkdtemp_in_workspace"),
        "stat" => Some("stat"),
        "rename" | "move" | "move_file" => Some("move_file"),
        "read_lines" => Some("read_lines"),
        "read_lines_page_result" => Some("read_lines_page_result"),
        "walk" | "walk_dir" => Some("walk_dir"),
        "glob" => Some("glob"),
        "find_text" => Some("find_text"),
        "find_evidence" => Some("find_evidence"),
        _ => None,
    }
}

pub fn harness_env_ambient(method: &str) -> Option<&'static str> {
    match method {
        "get" => Some("env"),
        "get_or" => Some("env_or"),
        _ => None,
    }
}

pub fn harness_random_ambient(method: &str) -> Option<&'static str> {
    match method {
        "gen_f64" | "f64" | "random" => Some("random"),
        "gen_u64" | "u64" => Some("random_int"),
        "gen_range" | "range" | "random_int" | "int" => Some("random_int"),
        "choice" | "random_choice" => Some("random_choice"),
        "shuffle" | "random_shuffle" => Some("random_shuffle"),
        _ => None,
    }
}

pub fn harness_net_ambient(method: &str) -> Option<&'static str> {
    match method {
        "get" | "http_get" => Some("http_get"),
        "post" | "http_post" => Some("http_post"),
        "put" | "http_put" => Some("http_put"),
        "patch" | "http_patch" => Some("http_patch"),
        "delete" | "http_delete" => Some("http_delete"),
        "request" | "http_request" => Some("http_request"),
        "download" | "http_download" => Some("http_download"),
        _ => None,
    }
}

pub fn harness_process_ambient(method: &str) -> Option<&'static str> {
    match method {
        "spawn_captured" => Some("spawn_captured"),
        _ => None,
    }
}

pub fn harness_crypto_ambient(method: &str) -> Option<&'static str> {
    match method {
        "sha256" => Some("sha256_hex"),
        _ => None,
    }
}

pub fn harness_system_ambient(method: &str) -> Option<&'static str> {
    match method {
        "platform" => Some("system_platform"),
        "cpu" => Some("system_cpu"),
        "memory" => Some("system_memory"),
        "gpus" => Some("system_gpus"),
        "temperature" => Some("system_temperature"),
        "processes" => Some("system_processes"),
        _ => None,
    }
}

pub fn harness_llm_ambient(method: &str) -> Option<&'static str> {
    match method {
        "catalog" => Some("llm_catalog"),
        "catalog_refresh" => Some("llm_catalog_refresh"),
        "providers" => Some("llm_provider_status"),
        _ => None,
    }
}

pub fn harness_secrets_ambient(method: &str) -> Option<&'static str> {
    match method {
        "read" | "read_bytes" | "write" | "delete" | "rotate" | "lease" | "lease_bytes" => None,
        _ => None,
    }
}

pub fn harness_tenant_ambient(method: &str) -> Option<&'static str> {
    match method {
        "id" => Some("harness_tenant_id"),
        "try_id" => Some("harness_tenant_try_id"),
        _ => None,
    }
}

pub fn harness_auth_ambient(method: &str) -> Option<&'static str> {
    match method {
        "is_authenticated" => Some("harness_auth_is_authenticated"),
        "subject" => Some("harness_auth_subject"),
        "try_subject" => Some("harness_auth_try_subject"),
        "scheme" => Some("harness_auth_scheme"),
        "try_scheme" => Some("harness_auth_try_scheme"),
        "kind" => Some("harness_auth_kind"),
        "scopes" => Some("harness_auth_scopes"),
        "has_scope" => Some("harness_auth_has_scope"),
        _ => None,
    }
}

pub fn harness_obs_ambient(method: &str) -> Option<&'static str> {
    match method {
        "span" | "start_span" => Some("__obs_start_span"),
        "end_span" => Some("__obs_end_span"),
        "log" => Some("__obs_emit"),
        "counter" => Some("__obs_counter"),
        "histogram" => Some("__obs_histogram"),
        "gauge" => Some("__obs_gauge"),
        "request_id" => Some("__obs_request_id"),
        _ => None,
    }
}

/// `harness.verdict.*` — the verdict issuance authority. `issue(result_handle)`
/// resolves the host-owned record of a real `run_test` execution and mints an
/// opaque proof-of-execution receipt; there is no caller-constructible positive
/// path and no caller-authored evidence can earn one.
pub fn harness_verdict_ambient(method: &str) -> Option<&'static str> {
    match method {
        "issue" => Some("__harness_verdict_issue"),
        // Handled VM-side (operates on the opaque receipt, which cannot cross the
        // hostlib boundary); the mapped name is never dispatched to a builtin,
        // it only marks the method as known to the typechecker.
        "same_run" => Some("__harness_verdict_same_run"),
        _ => None,
    }
}

pub fn harness_sub_handle_ambient(sub_handle: &str, method: &str) -> Option<&'static str> {
    match sub_handle {
        "stdio" => harness_stdio_ambient(method),
        "term" => harness_term_ambient(method),
        "clock" => harness_clock_ambient(method),
        "fs" => harness_fs_ambient(method),
        "env" => harness_env_ambient(method),
        "random" => harness_random_ambient(method),
        "net" => harness_net_ambient(method),
        "process" => harness_process_ambient(method),
        "crypto" => harness_crypto_ambient(method),
        "system" => harness_system_ambient(method),
        "secrets" => harness_secrets_ambient(method),
        "llm" => harness_llm_ambient(method),
        "tenant" => harness_tenant_ambient(method),
        "auth" => harness_auth_ambient(method),
        "obs" => harness_obs_ambient(method),
        "verdict" => harness_verdict_ambient(method),
        _ => None,
    }
}

pub fn harness_type_sub_handle(type_name: &str) -> Option<&'static str> {
    match type_name {
        "HarnessStdio" => Some("stdio"),
        "HarnessTerm" => Some("term"),
        "HarnessClock" => Some("clock"),
        "HarnessFs" => Some("fs"),
        "HarnessEnv" => Some("env"),
        "HarnessRandom" => Some("random"),
        "HarnessNet" => Some("net"),
        "HarnessProcess" => Some("process"),
        "HarnessCrypto" => Some("crypto"),
        "HarnessSystem" => Some("system"),
        "HarnessSecrets" => Some("secrets"),
        "HarnessLlm" => Some("llm"),
        "HarnessTenant" => Some("tenant"),
        "HarnessAuth" => Some("auth"),
        "HarnessObs" => Some("obs"),
        "HarnessVerdict" => Some("verdict"),
        _ => None,
    }
}

pub fn harness_fs_replacement(name: &str) -> Option<&'static str> {
    match name {
        "read_file" => Some("harness.fs.read_text"),
        "read_file_result" => Some("harness.fs.read_text_result"),
        "read_file_bytes" => Some("harness.fs.read_bytes"),
        "write_file" => Some("harness.fs.write_text"),
        "write_file_bytes" => Some("harness.fs.write_bytes"),
        "replace_file" => Some("harness.fs.replace_text"),
        "replace_file_result" => Some("harness.fs.replace_text_result"),
        "replace_file_bytes" => Some("harness.fs.replace_bytes"),
        "replace_file_bytes_result" => Some("harness.fs.replace_bytes_result"),
        "file_exists" => Some("harness.fs.exists"),
        "path_status" => Some("harness.fs.status"),
        "delete_file" => Some("harness.fs.delete"),
        "append_file" => Some("harness.fs.append"),
        "append_file_locked" => Some("harness.fs.append_locked"),
        "list_dir" => Some("harness.fs.list_dir"),
        "mkdir" => Some("harness.fs.mkdir"),
        "copy_file" => Some("harness.fs.copy"),
        "temp_dir" => Some("harness.fs.temp_dir"),
        "workspace_temp_dir" => Some("harness.fs.workspace_temp_dir"),
        "mkdtemp" => Some("harness.fs.mkdtemp"),
        "mkdtemp_in_workspace" => Some("harness.fs.mkdtemp_in_workspace"),
        "stat" => Some("harness.fs.stat"),
        "move_file" => Some("harness.fs.rename"),
        "read_lines" => Some("harness.fs.read_lines"),
        "read_lines_page_result" => Some("harness.fs.read_lines_page_result"),
        "walk_dir" => Some("harness.fs.walk"),
        "glob" => Some("harness.fs.glob"),
        "find_text" => Some("harness.fs.find_text"),
        "find_evidence" => Some("harness.fs.find_evidence"),
        _ => None,
    }
}
