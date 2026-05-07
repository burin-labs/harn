use std::process;

use crate::cli::ModelsTestArgs;

pub(crate) async fn run(args: &ModelsTestArgs) {
    let result = harn_vm::llm::run_model_smoke_test(harn_vm::llm::ModelSmokeTestOptions {
        model: args.model.clone(),
        provider: args.provider.clone(),
        prompt: args.prompt.clone(),
    })
    .await;

    match result {
        Ok(result) if args.json => match serde_json::to_string_pretty(&result) {
            Ok(payload) => println!("{payload}"),
            Err(error) => {
                crate::command_error(&format!("failed to serialize model test result: {error}"))
            }
        },
        Ok(result) => {
            let first_token = result
                .first_token_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string());
            println!(
                "model_id={} provider={} latency_ms={} first_token_ms={} input_tokens={} output_tokens={} estimated_cost_usd={:.6}",
                result.model_id,
                result.provider,
                result.latency_ms,
                first_token,
                result.input_tokens,
                result.output_tokens,
                result.estimated_cost_usd
            );
        }
        Err(error) if args.json => {
            println!("{}", serde_json::json!({ "ok": false, "error": error }));
            process::exit(1);
        }
        Err(error) => {
            eprintln!("{error}");
            process::exit(1);
        }
    }
}
