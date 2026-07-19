use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;

use crate::cli::{
    HostArgs, HostCommand, HostLeaseAcquireArgs, HostLeaseArgs, HostLeaseCommand,
    HostLeasePriorityArg, HostLeaseReleaseArgs, HostLeaseRenewArgs, HostLeaseResourceClassArg,
    HostLeaseStatusArgs,
};
use crate::json_envelope::{to_string_pretty, JsonEnvelope, JsonError};

pub const HOST_LEASE_CLI_SCHEMA_VERSION: u32 = 3;
const EX_TEMPFAIL: i32 = 75;

mod cargo_run;

pub(crate) async fn run(args: HostArgs) -> i32 {
    match args.command {
        HostCommand::Lease(args) => run_lease(args).await,
    }
}

async fn run_lease(args: HostLeaseArgs) -> i32 {
    let command = match args.command {
        HostLeaseCommand::RunCargoWorker(args) => return cargo_run::run_cargo_worker(args),
        command => command,
    };
    let json = lease_command_json(&command);
    let store = match harn_hostlib::HostLeaseStore::from_env() {
        Ok(store) => store,
        Err(error) => return print_error("host_lease_store", &error.to_string(), json),
    };
    match command {
        HostLeaseCommand::Acquire(args) => acquire(&store, args),
        HostLeaseCommand::Renew(args) => renew(&store, args),
        HostLeaseCommand::Release(args) => release(&store, args),
        HostLeaseCommand::Status(args) => status(&store, args),
        HostLeaseCommand::Run(args) => cargo_run::run_supervised(&store, args).await,
        HostLeaseCommand::RunCargoWorker(_) => unreachable!("worker returned before store setup"),
    }
}

fn lease_command_json(command: &HostLeaseCommand) -> bool {
    match command {
        HostLeaseCommand::Acquire(args) => args.json,
        HostLeaseCommand::Renew(args) => args.json,
        HostLeaseCommand::Release(args) => args.json,
        HostLeaseCommand::Status(args) => args.json,
        HostLeaseCommand::Run(_) | HostLeaseCommand::RunCargoWorker(_) => false,
    }
}

fn acquire(store: &harn_hostlib::HostLeaseStore, args: HostLeaseAcquireArgs) -> i32 {
    let json = args.json;
    let host = args
        .host
        .unwrap_or_else(harn_hostlib::HostLeaseStore::default_host);
    let request = harn_hostlib::HostLeaseRequest {
        host,
        resource_class: resource_class(args.resource_class),
        domain: args.domain,
        execution_context: None,
        owner: args.owner,
        priority_class: priority(args.priority_class),
        ttl_ms: (!args.no_expiry).then_some(args.ttl_ms),
        owner_pid: args.owner_pid,
        reason: args.reason,
        metadata: BTreeMap::new(),
    };
    let result = if args.wait_ms == 0 {
        store.try_acquire(request)
    } else {
        store.acquire_wait(request, Duration::from_millis(args.wait_ms))
    };
    match result {
        Ok(receipt) if receipt.status == harn_hostlib::HostLeaseAcquireStatus::Acquired => {
            print_success(&receipt, json, |receipt| {
                let handle = receipt
                    .handle
                    .as_ref()
                    .expect("acquired receipt has handle");
                format!(
                    "Acquired {} lease on {} as {} ({})",
                    handle.priority_class.as_str(),
                    handle.host,
                    handle.owner,
                    handle.lease_id
                )
            });
            0
        }
        Ok(receipt) => {
            if json {
                let error_code = receipt
                    .defer
                    .as_ref()
                    .expect("deferred receipt has reason")
                    .deferred_reason
                    .as_str();
                let envelope = JsonEnvelope {
                    schema_version: HOST_LEASE_CLI_SCHEMA_VERSION,
                    ok: false,
                    data: Some(receipt),
                    error: Some(JsonError {
                        code: error_code.to_string(),
                        message: "host lease remains held by another owner".to_string(),
                        details: serde_json::Value::Null,
                    }),
                    warnings: Vec::new(),
                };
                println!("{}", to_string_pretty(&envelope));
            } else if let Some(defer) = receipt.defer {
                if let Some(active) = defer.active {
                    eprintln!(
                        "Host {} is leased by {} ({}, lease {})",
                        defer.host,
                        active.owner,
                        active.priority_class.as_str(),
                        active.lease_id
                    );
                } else {
                    eprintln!("Host {} lease registry is busy", defer.host);
                }
            }
            EX_TEMPFAIL
        }
        Err(error) => print_error("host_lease_acquire", &error.to_string(), json),
    }
}

fn renew(store: &harn_hostlib::HostLeaseStore, args: HostLeaseRenewArgs) -> i32 {
    let host = args
        .host
        .unwrap_or_else(harn_hostlib::HostLeaseStore::default_host);
    match store.renew_for_domain(
        &host,
        resource_class(args.resource_class),
        &args.domain,
        &args.lease_id,
        args.ttl_ms,
    ) {
        Ok(receipt) if receipt.renewed => {
            print_success(&receipt, args.json, |_| {
                format!("Renewed lease {}", args.lease_id)
            });
            0
        }
        Ok(receipt) => {
            print_failure_data(
                "host_lease_not_owned",
                "no matching active lease",
                receipt,
                args.json,
            );
            EX_TEMPFAIL
        }
        Err(error) => print_error("host_lease_renew", &error.to_string(), args.json),
    }
}

fn release(store: &harn_hostlib::HostLeaseStore, args: HostLeaseReleaseArgs) -> i32 {
    let host = args
        .host
        .unwrap_or_else(harn_hostlib::HostLeaseStore::default_host);
    match store.release_for_domain(
        &host,
        resource_class(args.resource_class),
        &args.domain,
        &args.lease_id,
    ) {
        Ok(receipt) if receipt.released => {
            print_success(&receipt, args.json, |_| {
                format!("Released lease {}", args.lease_id)
            });
            0
        }
        Ok(receipt) => {
            print_failure_data(
                "host_lease_not_owned",
                "no matching active lease",
                receipt,
                args.json,
            );
            EX_TEMPFAIL
        }
        Err(error) => print_error("host_lease_release", &error.to_string(), args.json),
    }
}

fn status(store: &harn_hostlib::HostLeaseStore, args: HostLeaseStatusArgs) -> i32 {
    let host = args
        .host
        .unwrap_or_else(harn_hostlib::HostLeaseStore::default_host);
    match store.status_for_domain(&host, resource_class(args.resource_class), &args.domain) {
        Ok(state) => {
            print_success(&state, args.json, |state| match state.active.as_ref() {
                Some(active) => format!(
                    "Host {} is leased by {} ({}, lease {})",
                    state.host,
                    active.owner,
                    active.priority_class.as_str(),
                    active.lease_id
                ),
                None => format!("Host {} is available", state.host),
            });
            0
        }
        Err(error) => print_error("host_lease_status", &error.to_string(), args.json),
    }
}

pub(super) fn priority(value: HostLeasePriorityArg) -> harn_hostlib::HostLeasePriorityClass {
    match value {
        HostLeasePriorityArg::Interactive => harn_hostlib::HostLeasePriorityClass::Interactive,
        HostLeasePriorityArg::Measurement => harn_hostlib::HostLeasePriorityClass::Measurement,
        HostLeasePriorityArg::CiVerify => harn_hostlib::HostLeasePriorityClass::CiVerify,
        HostLeasePriorityArg::Deferrable => harn_hostlib::HostLeasePriorityClass::Deferrable,
    }
}

fn resource_class(value: HostLeaseResourceClassArg) -> harn_hostlib::HostLeaseResourceClass {
    match value {
        HostLeaseResourceClassArg::WholeMachine => {
            harn_hostlib::HostLeaseResourceClass::WholeMachine
        }
        HostLeaseResourceClassArg::RustHeavy => harn_hostlib::HostLeaseResourceClass::RustHeavy,
    }
}

fn print_success<T, F>(value: &T, json: bool, human: F)
where
    T: Serialize,
    F: FnOnce(&T) -> String,
{
    if json {
        println!(
            "{}",
            to_string_pretty(&JsonEnvelope::ok(HOST_LEASE_CLI_SCHEMA_VERSION, value))
        );
    } else {
        println!("{}", human(value));
    }
}

fn print_failure_data<T: Serialize>(code: &str, message: &str, value: T, json: bool) {
    if json {
        let envelope = JsonEnvelope {
            schema_version: HOST_LEASE_CLI_SCHEMA_VERSION,
            ok: false,
            data: Some(value),
            error: Some(JsonError {
                code: code.to_string(),
                message: message.to_string(),
                details: serde_json::Value::Null,
            }),
            warnings: Vec::new(),
        };
        println!("{}", to_string_pretty(&envelope));
    } else {
        eprintln!("{message}");
    }
}

fn print_error(code: &str, message: &str, json: bool) -> i32 {
    if json {
        let envelope: JsonEnvelope<serde_json::Value> =
            JsonEnvelope::err(HOST_LEASE_CLI_SCHEMA_VERSION, code, message);
        println!("{}", to_string_pretty(&envelope));
    } else {
        eprintln!("error: {message}");
    }
    1
}
