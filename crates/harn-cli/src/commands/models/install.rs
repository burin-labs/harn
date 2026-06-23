use std::io::{self, BufRead, Write};
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};

use crate::cli::ModelsInstallArgs;

pub(crate) async fn run(args: ModelsInstallArgs) {
    let resolved = harn_vm::llm_config::resolve_model_info(&args.model);
    if resolved.provider != "ollama" {
        if let Some(plan) = setup_plan_for(&args.model, &resolved.provider, &resolved.id) {
            print_setup_plan(&plan);
            return;
        }
        eprintln!(
            "harn models install currently knows how to pull Ollama models and print setup steps \
             for known local servers; '{}' resolved to provider '{}'.",
            args.model, resolved.provider
        );
        eprintln!("For this provider, start the server yourself and verify it with:");
        eprintln!(
            "  harn provider-ready {} --model {}",
            resolved.provider, args.model
        );
        std::process::exit(1);
    }

    if which::which("ollama").is_err() {
        let hint = if cfg!(target_os = "macos") {
            "macOS: install with `brew install ollama` or download from https://ollama.com"
        } else if cfg!(target_os = "linux") {
            "Linux: install with `curl -fsSL https://ollama.com/install.sh | sh`"
        } else {
            "Install Ollama from https://ollama.com"
        };
        eprintln!("ollama is not installed.");
        eprintln!("{hint}");
        std::process::exit(1);
    }

    if args.model != resolved.id {
        println!(
            "Resolved {} -> {} via provider {}",
            args.model, resolved.id, resolved.provider
        );
    }

    if let Some(size_gb) = estimate_size_gb(&resolved.id).await {
        if size_gb > 10 && !args.yes {
            eprint!(
                "Model {} is approximately {size_gb} GB. Continue? [y/N] ",
                resolved.id
            );
            io::stderr().flush().ok();
            let mut buf = String::new();
            if io::stdin().lock().read_line(&mut buf).is_err()
                || !matches!(buf.trim(), "y" | "Y" | "yes")
            {
                eprintln!("aborted");
                std::process::exit(1);
            }
        }
    }

    let mut command = tokio::process::Command::new("ollama");
    command.arg("pull").arg(&resolved.id);
    if let Some(keep) = &args.keep_alive {
        command.env("OLLAMA_KEEP_ALIVE", keep);
    }
    command.stdout(Stdio::piped()).stderr(Stdio::inherit());
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(error) => {
            eprintln!("failed to spawn ollama: {error}");
            std::process::exit(1);
        }
    };
    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            println!("{line}");
        }
    }
    let status = match child.wait().await {
        Ok(s) => s,
        Err(error) => {
            eprintln!("failed to wait for ollama: {error}");
            std::process::exit(1);
        }
    };
    if !status.success() {
        eprintln!("ollama pull exited {status}");
        std::process::exit(status.code().unwrap_or(1));
    }
    println!("\nPulled {}", resolved.id);

    // Best-effort warm probe through provider readiness. Skip silently if it
    // fails; the pull itself succeeded.
    let api_key = std::env::var("OLLAMA_API_KEY").unwrap_or_default();
    let readiness = harn_vm::llm::readiness::probe_provider_readiness_with_options(
        "ollama",
        harn_vm::llm::readiness::ProviderReadinessOptions {
            requested_model: Some(&resolved.id),
            base_url_override: None,
            api_key_override: Some(&api_key),
        },
    )
    .await;
    if readiness.ok {
        println!("Warm probe: ok");
    } else {
        println!("Warm probe: skipped ({})", readiness.message);
    }
    println!();
    println!("Use with:");
    println!(
        "  HARN_LLM_PROVIDER=ollama HARN_LLM_MODEL={} harn run <file.harn>",
        args.model
    );
    println!("Verify:");
    println!("  harn provider-ready ollama --model {}", args.model);
}

struct SetupPlan {
    title: &'static str,
    steps: Vec<String>,
}

fn setup_plan_for(selector: &str, provider: &str, model_id: &str) -> Option<SetupPlan> {
    match provider {
        "llamacpp" => Some(llamacpp_setup_plan(selector, model_id)),
        "mlx" => Some(mlx_setup_plan(selector, model_id)),
        "local" => Some(local_openai_setup_plan(selector, model_id)),
        _ => None,
    }
}

fn llamacpp_setup_plan(selector: &str, model_id: &str) -> SetupPlan {
    let model_path = if model_id.contains("qwen3.6") {
        "$HOME/models/qwen3.6/Qwen3.6-35B-A3B-UD-Q4_K_XL.gguf"
    } else {
        "$HOME/models/model.gguf"
    };
    SetupPlan {
        title: "llama.cpp setup",
        steps: vec![
            llamacpp_install_step(),
            "Download Qwen3.6 GGUF: `hf download unsloth/Qwen3.6-35B-A3B-GGUF --include \"Qwen3.6-35B-A3B-UD-Q4_K_XL.gguf\" --local-dir ~/models/qwen3.6`".to_string(),
            format!(
                "Launch through Harn: `harn local launch {selector} --provider llamacpp --model-source {model_path} --ctx 65536 --parallel 1 --cache-type-k q8_0 --cache-type-v q8_0 --cache-ram 0 --gpu-layers auto --reasoning off`"
            ),
            "Export endpoint: `export LLAMACPP_BASE_URL=http://127.0.0.1:8001`".to_string(),
            format!("Verify: `harn provider-ready llamacpp --model {selector}`"),
        ],
    }
}

fn llamacpp_install_step() -> String {
    if cfg!(target_os = "macos") {
        "Install runtime tools: `brew install llama.cpp hf`".to_string()
    } else if cfg!(target_os = "linux") {
        "Install runtime tools: install `llama-server` from https://github.com/ggml-org/llama.cpp/releases or build llama.cpp with your CUDA/ROCm/CPU backend, then run `python3 -m pip install -U \"huggingface_hub[cli]\"` for `hf`.".to_string()
    } else if cfg!(target_os = "windows") {
        "Install runtime tools: download a llama.cpp Windows release with `llama-server.exe`, add it to PATH, then run `py -m pip install -U \"huggingface_hub[cli]\"` for `hf`.".to_string()
    } else {
        "Install runtime tools: install `llama-server` from llama.cpp for this platform and install the Hugging Face CLI (`huggingface_hub[cli]`).".to_string()
    }
}

fn mlx_setup_plan(selector: &str, _model_id: &str) -> SetupPlan {
    SetupPlan {
        title: "MLX local coding setup (Qwen3.6-35B-A3B MoE)",
        steps: vec![
            "Create runtime: `python3 -m venv ~/.harn/mlx-lm && ~/.harn/mlx-lm/bin/pip install -U pip mlx-lm huggingface_hub`".to_string(),
            "Download Qwen3.6 MLX: `~/.harn/mlx-lm/bin/hf download unsloth/Qwen3.6-35B-A3B-UD-MLX-4bit --local-dir ~/models/qwen3.6-35b-a3b-mlx/Qwen3.6-35B-A3B-UD-MLX-4bit`".to_string(),
            "Launch through Harn: `harn local launch mlx-qwen3.6 --provider mlx --server-command ~/.harn/mlx-lm/bin/mlx_lm.server --model-source ~/models/qwen3.6-35b-a3b-mlx/Qwen3.6-35B-A3B-UD-MLX-4bit`".to_string(),
            "If `mlx_lm.server` is on PATH, omit `--server-command`; the provider catalog supplies that default.".to_string(),
            "Export endpoint: `export MLX_BASE_URL=http://127.0.0.1:8002`".to_string(),
            format!("Verify: `harn provider-ready mlx --model {selector}`"),
        ],
    }
}

fn local_openai_setup_plan(selector: &str, model_id: &str) -> SetupPlan {
    SetupPlan {
        title: "local OpenAI-compatible setup",
        steps: vec![
            "Start your OpenAI-compatible runtime on a stable host and port, for example vLLM/SGLang on `http://127.0.0.1:8000`.".to_string(),
            format!("Export endpoint and model: `export LOCAL_LLM_BASE_URL=http://127.0.0.1:8000 LOCAL_LLM_MODEL={model_id}`"),
            format!("Verify: `harn provider-ready local --model {selector}`"),
        ],
    }
}

fn print_setup_plan(plan: &SetupPlan) {
    println!("{}", plan.title);
    println!();
    for (idx, step) in plan.steps.iter().enumerate() {
        println!("{}. {step}", idx + 1);
    }
}

async fn estimate_size_gb(model: &str) -> Option<u64> {
    // Best-effort `/api/show` query against the local Ollama daemon.
    let url = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
    let body = serde_json::json!({"name": model});
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()?;
    let resp = client
        .post(format!("{url}/api/show"))
        .json(&body)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    let bytes = v.get("size").and_then(|n| n.as_u64())?;
    Some(bytes / (1024 * 1024 * 1024))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_plan_exists_for_llamacpp_qwen_alias() {
        let resolved = harn_vm::llm_config::resolve_model_info("local-qwen3.6-gguf");
        let plan = setup_plan_for("local-qwen3.6-gguf", &resolved.provider, &resolved.id)
            .expect("llama.cpp setup plan");
        assert_eq!(plan.title, "llama.cpp setup");
        assert!(plan
            .steps
            .iter()
            .any(|step| step.contains("harn provider-ready llamacpp")));
        assert!(plan.steps.iter().any(|step| step.contains("--ctx 65536")));
    }

    #[test]
    fn setup_plan_exists_for_mlx_qwen_alias() {
        let resolved = harn_vm::llm_config::resolve_model_info("mlx-qwen3.6");
        let plan = setup_plan_for("mlx-qwen3.6", &resolved.provider, &resolved.id)
            .expect("MLX setup plan");
        assert_eq!(plan.title, "MLX local coding setup (Qwen3.6-35B-A3B MoE)");
        assert!(plan.steps.iter().any(|step| step.contains("mlx_lm.server")));
    }
}
