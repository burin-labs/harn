use serde::Serialize;

use crate::cli::{
    OrchestratorTenantArgs, OrchestratorTenantCommand, OrchestratorTenantCreateArgs,
    OrchestratorTenantDeleteArgs, OrchestratorTenantLsArgs, OrchestratorTenantSuspendArgs,
};

use super::common::{absolutize_from_cwd, print_json};

#[derive(Serialize)]
struct TenantCreateOutput {
    tenant: harn_vm::TenantRecord,
    api_key: String,
}

pub(crate) async fn run(args: OrchestratorTenantArgs) -> Result<(), String> {
    let state_dir = absolutize_from_cwd(&args.local.state_dir)?;
    std::fs::create_dir_all(&state_dir).map_err(|error| {
        format!(
            "failed to create state dir {}: {error}",
            state_dir.display()
        )
    })?;
    let mut store = harn_vm::TenantStore::load(&state_dir)?;
    match args.command {
        OrchestratorTenantCommand::Create(create) => create_tenant(&mut store, create),
        OrchestratorTenantCommand::Ls(ls) => list_tenants(&store, ls),
        OrchestratorTenantCommand::Suspend(suspend) => suspend_tenant(&mut store, suspend),
        OrchestratorTenantCommand::Delete(delete) => delete_tenant(&mut store, delete),
    }
}

fn create_tenant(
    store: &mut harn_vm::TenantStore,
    args: OrchestratorTenantCreateArgs,
) -> Result<(), String> {
    if let Some(value) = args.daily_cost_usd {
        if value < 0.0 {
            return Err("--daily-cost-usd must be greater than or equal to 0".to_string());
        }
    }
    if let Some(value) = args.hourly_cost_usd {
        if value < 0.0 {
            return Err("--hourly-cost-usd must be greater than or equal to 0".to_string());
        }
    }
    let budget = harn_vm::TenantBudget {
        daily_cost_usd: args.daily_cost_usd,
        hourly_cost_usd: args.hourly_cost_usd,
        ingest_per_minute: args.ingest_per_minute,
        ..harn_vm::TenantBudget::default()
    };
    let (tenant, api_key) = store.create_tenant(args.id, budget)?;
    if args.json {
        return print_json(&TenantCreateOutput { tenant, api_key });
    }
    println!("created tenant {}", tenant.scope.id.0);
    println!("state: {}", tenant.scope.state_root.display());
    println!("secret namespace: {}", tenant.scope.secret_namespace);
    println!(
        "event topic prefix: {}",
        tenant.scope.event_log_topic_prefix
    );
    println!("api key: {api_key}");
    Ok(())
}

fn list_tenants(
    store: &harn_vm::TenantStore,
    args: OrchestratorTenantLsArgs,
) -> Result<(), String> {
    let tenants = store.list();
    if args.json {
        return print_json(&tenants);
    }
    if tenants.is_empty() {
        println!("no tenants");
        return Ok(());
    }
    for tenant in tenants {
        println!(
            "{}\t{:?}\t{}\tkeys={}",
            tenant.scope.id.0,
            tenant.status,
            tenant.scope.state_root.display(),
            tenant.api_keys.len()
        );
    }
    Ok(())
}

fn suspend_tenant(
    store: &mut harn_vm::TenantStore,
    args: OrchestratorTenantSuspendArgs,
) -> Result<(), String> {
    let tenant = store.suspend(&args.id)?;
    if args.json {
        return print_json(&tenant);
    }
    println!("suspended tenant {}", tenant.scope.id.0);
    Ok(())
}

fn delete_tenant(
    store: &mut harn_vm::TenantStore,
    args: OrchestratorTenantDeleteArgs,
) -> Result<(), String> {
    if !args.confirm {
        return Err("tenant delete is destructive; pass --confirm".to_string());
    }
    let tenant = store.delete(&args.id)?;
    if args.json {
        return print_json(&tenant);
    }
    println!("deleted tenant {}", tenant.scope.id.0);
    Ok(())
}
