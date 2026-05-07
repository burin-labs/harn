use std::collections::{BTreeMap, BTreeSet};

use harn_vm::llm_config;
use serde_json::json;

use crate::cli::ModelsListArgs;

pub(crate) async fn run(args: ModelsListArgs) {
    let installed_ollama = detect_installed_ollama_models().await;

    // Build provider -> Vec<(id, tags, installed)>.
    let mut by_provider: BTreeMap<String, Vec<(String, Vec<String>, bool)>> = BTreeMap::new();

    for (id, model) in llm_config::model_catalog_entries() {
        let provider = model.provider.clone();
        let installed = provider == "ollama" && installed_ollama.contains(&id);
        if args.installed_only && !installed {
            continue;
        }
        if let Some(filter) = &args.provider {
            if &provider != filter {
                continue;
            }
        }
        by_provider
            .entry(provider)
            .or_default()
            .push((id, model.capabilities, installed));
    }

    // Synthesize installed Ollama models the catalog hasn't listed.
    if args
        .provider
        .as_deref()
        .map(|p| p == "ollama")
        .unwrap_or(true)
    {
        let known: BTreeSet<String> = by_provider
            .get("ollama")
            .into_iter()
            .flat_map(|v| v.iter().map(|(id, _, _)| id.clone()))
            .collect();
        for id in &installed_ollama {
            if !known.contains(id) {
                by_provider.entry("ollama".to_string()).or_default().push((
                    id.clone(),
                    vec!["local".to_string()],
                    true,
                ));
            }
        }
    }

    if args.json {
        let providers: Vec<serde_json::Value> = by_provider
            .iter()
            .map(|(name, models)| {
                let model_list: Vec<serde_json::Value> = models
                    .iter()
                    .map(|(id, tags, installed)| {
                        if name == "ollama" {
                            json!({"id": id, "tags": tags, "installed": installed})
                        } else {
                            json!({"id": id, "tags": tags})
                        }
                    })
                    .collect();
                json!({"name": name, "models": model_list})
            })
            .collect();
        let payload = json!({"providers": providers});
        match serde_json::to_string_pretty(&payload) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("failed to render JSON: {e}"),
        }
        return;
    }

    if by_provider.is_empty() {
        println!("(no models match)");
        return;
    }
    for (provider, mut models) in by_provider {
        println!("{provider}");
        models.sort_by(|a, b| a.0.cmp(&b.0));
        for (id, tags, installed) in models {
            let suffix = if installed { " [installed]" } else { "" };
            let tag_text = if tags.is_empty() {
                String::new()
            } else {
                format!("  ({})", tags.join(", "))
            };
            println!("  {id}{suffix}{tag_text}");
        }
        println!();
    }
}

async fn detect_installed_ollama_models() -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    if which::which("ollama").is_err() {
        return set;
    }
    let Ok(output) = tokio::process::Command::new("ollama")
        .arg("list")
        .output()
        .await
    else {
        return set;
    };
    if !output.status.success() {
        return set;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines().skip(1) {
        if let Some(name) = line.split_whitespace().next() {
            set.insert(name.to_string());
        }
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn empty_payload_serializes() {
        let payload = json!({"providers": []});
        let parsed: Value = serde_json::from_str(&payload.to_string()).unwrap();
        assert_eq!(parsed["providers"].as_array().unwrap().len(), 0);
    }
}
