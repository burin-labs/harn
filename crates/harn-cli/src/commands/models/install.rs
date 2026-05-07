use std::io::{self, BufRead, Write};
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};

use crate::cli::ModelsInstallArgs;

pub(crate) async fn run(args: ModelsInstallArgs) {
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

    if let Some(size_gb) = estimate_size_gb(&args.model).await {
        if size_gb > 10 && !args.yes {
            eprint!(
                "Model {} is approximately {size_gb} GB. Continue? [y/N] ",
                args.model
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
    command.arg("pull").arg(&args.model);
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
    println!("\nPulled {}", args.model);

    // Best-effort warm probe through the OpenAI-compatible interface. Skip
    // silently if it fails — the pull itself succeeded.
    let api_key = std::env::var("OLLAMA_API_KEY").unwrap_or_default();
    let readiness =
        harn_vm::llm::probe_openai_compatible_model("ollama", &args.model, &api_key).await;
    if readiness.valid {
        println!("Warm probe: ok");
    } else {
        println!("Warm probe: skipped ({})", readiness.message);
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
