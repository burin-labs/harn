//! Argument and rendering projection of the VM-owned decision evaluator.
use crate::cli::LlmEvaluateArgs;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    site_id: String,
    state: Value,
    questions: Value,
    policy: Value,
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))
}

fn read_json(path: &Path) -> Result<Value, String> {
    serde_json::from_str(&read(path)?)
        .map_err(|error| format!("invalid JSON in {}: {error}", path.display()))
}

fn request(args: &LlmEvaluateArgs) -> Result<Request, String> {
    if let Some(path) = &args.request {
        return serde_json::from_value(read_json(path)?).map_err(|error| error.to_string());
    }
    let model = args.model.as_deref().ok_or("--model is required")?;
    let resolved = harn_vm::llm_config::resolve_model_info(model);
    let state = read(
        args.state_file
            .as_deref()
            .ok_or("--state-file is required")?,
    )?;
    let policy = if let Some(path) = &args.policy {
        let policy = read_json(path)?;
        if policy.get("model").and_then(Value::as_str) != Some(model) {
            return Err("--policy model must match --model".into());
        }
        policy
    } else {
        json!({"backend":"native_decision", "provider":resolved.provider, "model":model,
            "threshold":0.5, "evaluation_cost_limit":0.01, "run_cost_limit":0.01})
    };
    Ok(Request {
        site_id: args.site_id.clone(),
        state: serde_json::from_str(&state).unwrap_or(Value::String(state)),
        questions: read_json(args.questions.as_deref().ok_or("--questions is required")?)?,
        policy,
    })
}

pub(crate) async fn run(args: LlmEvaluateArgs) -> i32 {
    let request = match request(&args) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("error: {error}");
            return 2;
        }
    };
    let mut vm = harn_vm::Vm::new();
    let result = harn_vm::orchestration::scope_fresh_run_runtime(vm.evaluate_decision(
        &request.site_id,
        request.state,
        request.questions,
        request.policy,
    ))
    .await;
    match result {
        Ok(result) => {
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string(&result).expect("evaluation result is serializable")
                );
            } else {
                println!("{}", result.outcome);
                println!(
                    "model={} requests={} input_tokens={} output_tokens={} cost_usd={} elapsed_ms={} receipt={}",
                    result.receipt.served_model.as_deref().unwrap_or("unknown"),
                    result.receipt.physical_attempts,
                    result.receipt.input_tokens.map_or_else(|| "unknown".into(), |value| value.to_string()),
                    result.receipt.output_tokens.map_or_else(|| "unknown".into(), |value| value.to_string()),
                    result
                        .receipt
                        .cost_usd
                        .map_or_else(|| "unknown".into(), |value| value.to_string()),
                    result.receipt.elapsed_ms,
                    result.receipt.reference()
                );
            }
            0
        }
        Err(error) => {
            eprintln!("error: {error}");
            2
        }
    }
}
