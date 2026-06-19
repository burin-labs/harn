use crate::value::VmDictExt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::event_log::{
    active_event_log, install_memory_for_current_thread, EventLog, LogEvent, Topic,
};
use crate::runtime_limits::RuntimeLimits;
use crate::trust_graph::{AutonomyTier, TrustOutcome, TrustRecord};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const GIT_RECEIPTS_TOPIC: &str = "stdlib.git.receipts";
const EVENT_LOG_QUEUE_DEPTH: usize = RuntimeLimits::DEFAULT.default_event_log_queue_depth;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GitMutation {
    Read,
    Mutating,
    Risky,
}

#[derive(Clone, Debug)]
struct GitCommand {
    operation: &'static str,
    action: &'static str,
    cwd: PathBuf,
    argv: Vec<String>,
    mutation: GitMutation,
    affected_paths: Vec<String>,
    data_parser: GitDataParser,
}

#[derive(Clone, Debug)]
enum GitDataParser {
    None,
    Discover { input: String },
    Status,
    Conflicts,
    MergeBase,
    Diff,
    WorktreeCreate { branch: String, path: String },
    WorktreeRemove { path: String },
    Push { refspec: String },
    Fetch,
    Rebase,
}

pub(crate) fn register_git_builtins(vm: &mut Vm) {
    register_git_namespace(vm);

    vm.register_async_builtin("git.repo.discover", |ctx, args| async move {
        let path = required_path_arg(&args, 0, "git.repo.discover")?;
        run_git_command(
            Some(&ctx),
            GitCommand {
                operation: "git.repo.discover",
                action: "git.repo.discover",
                cwd: path.clone(),
                argv: vec![
                    "git".to_string(),
                    "rev-parse".to_string(),
                    "--show-toplevel".to_string(),
                    "--git-dir".to_string(),
                    "--is-bare-repository".to_string(),
                    "--is-inside-work-tree".to_string(),
                ],
                mutation: GitMutation::Read,
                affected_paths: vec![display_path(&path)],
                data_parser: GitDataParser::Discover {
                    input: display_path(&path),
                },
            },
        )
        .await
    });

    vm.register_async_builtin("git.worktree.create", |ctx, args| async move {
        let repo = repo_path_arg(&args, 0, "git.worktree.create")?;
        let branch = required_string_arg(&args, 1, "git.worktree.create", "branch")?;
        let path = required_string_arg(&args, 2, "git.worktree.create", "path")?;
        let options = optional_dict_arg(&args, 3);
        let force = bool_option(options, "force").unwrap_or(false);
        let detach = bool_option(options, "detach").unwrap_or(false);
        let base_ref = string_option(options, "base_ref")
            .or_else(|| string_option(options, "base"))
            .or_else(|| string_option(options, "start_point"));
        let mut argv = vec!["git".to_string(), "worktree".to_string(), "add".to_string()];
        if force {
            argv.push("--force".to_string());
        }
        if detach {
            argv.push("--detach".to_string());
        } else {
            argv.push("-B".to_string());
            argv.push(branch.clone());
        }
        argv.push(path.clone());
        if let Some(base_ref) = base_ref {
            argv.push(base_ref);
        }
        run_git_command(
            Some(&ctx),
            GitCommand {
                operation: "git.worktree.create",
                action: "git.worktree.create",
                cwd: repo,
                argv,
                mutation: GitMutation::Mutating,
                affected_paths: vec![path.clone()],
                data_parser: GitDataParser::WorktreeCreate { branch, path },
            },
        )
        .await
    });

    vm.register_async_builtin("git.worktree.remove", |ctx, args| async move {
        let path = required_string_arg(&args, 0, "git.worktree.remove", "path")?;
        let options = optional_dict_arg(&args, 1);
        let force = bool_option(options, "force").unwrap_or(false);
        let path_buf = PathBuf::from(&path);
        if !path_buf.exists() {
            return planned_or_noop_receipt(
                "git.worktree.remove",
                "git.worktree.remove",
                GitMutation::Mutating,
                path_buf
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .to_path_buf(),
                vec![
                    "git".to_string(),
                    "worktree".to_string(),
                    "remove".to_string(),
                    path.clone(),
                ],
                vec![path.clone()],
                json!({"path": path, "removed": false, "idempotent": true}),
                "no_op",
            )
            .await;
        }
        let mut argv = vec![
            "git".to_string(),
            "worktree".to_string(),
            "remove".to_string(),
        ];
        if force {
            argv.push("--force".to_string());
        }
        argv.push(path.clone());
        let cwd = path_buf
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        run_git_command(
            Some(&ctx),
            GitCommand {
                operation: "git.worktree.remove",
                action: "git.worktree.remove",
                cwd,
                argv,
                mutation: GitMutation::Mutating,
                affected_paths: vec![path.clone()],
                data_parser: GitDataParser::WorktreeRemove { path },
            },
        )
        .await
    });

    vm.register_async_builtin("git.fetch", |ctx, args| async move {
        let repo = repo_path_arg(&args, 0, "git.fetch")?;
        let remote = required_string_arg(&args, 1, "git.fetch", "remote")?;
        let refspecs = string_list_arg(&args, 2, "git.fetch", "refspecs")?.unwrap_or_default();
        let mut argv = vec!["git".to_string(), "fetch".to_string(), remote.clone()];
        argv.extend(refspecs);
        run_git_command(
            Some(&ctx),
            GitCommand {
                operation: "git.fetch",
                action: "git.fetch",
                cwd: repo,
                argv,
                mutation: GitMutation::Mutating,
                affected_paths: Vec::new(),
                data_parser: GitDataParser::Fetch,
            },
        )
        .await
    });

    vm.register_async_builtin("git.rebase", |ctx, args| async move {
        let repo = repo_path_arg(&args, 0, "git.rebase")?;
        let base_ref = required_string_arg(&args, 1, "git.rebase", "base_ref")?;
        run_git_command(
            Some(&ctx),
            GitCommand {
                operation: "git.rebase",
                action: "git.rebase",
                cwd: repo,
                argv: vec!["git".to_string(), "rebase".to_string(), base_ref],
                mutation: GitMutation::Risky,
                affected_paths: Vec::new(),
                data_parser: GitDataParser::Rebase,
            },
        )
        .await
    });

    vm.register_async_builtin("git.status", |ctx, args| async move {
        let repo = repo_path_arg(&args, 0, "git.status")?;
        run_git_command(
            Some(&ctx),
            GitCommand {
                operation: "git.status",
                action: "git.status",
                cwd: repo,
                argv: vec![
                    "git".to_string(),
                    "status".to_string(),
                    "--porcelain=v1".to_string(),
                    "--branch".to_string(),
                ],
                mutation: GitMutation::Read,
                affected_paths: Vec::new(),
                data_parser: GitDataParser::Status,
            },
        )
        .await
    });

    vm.register_async_builtin("git.conflicts", |ctx, args| async move {
        let repo = repo_path_arg(&args, 0, "git.conflicts")?;
        run_git_command(
            Some(&ctx),
            GitCommand {
                operation: "git.conflicts",
                action: "git.conflicts",
                cwd: repo,
                argv: vec![
                    "git".to_string(),
                    "status".to_string(),
                    "--porcelain=v1".to_string(),
                ],
                mutation: GitMutation::Read,
                affected_paths: Vec::new(),
                data_parser: GitDataParser::Conflicts,
            },
        )
        .await
    });

    vm.register_async_builtin("git.push", |ctx, args| async move {
        let repo = repo_path_arg(&args, 0, "git.push")?;
        let remote = required_string_arg(&args, 1, "git.push", "remote")?;
        let refspec = required_string_arg(&args, 2, "git.push", "refspec")?;
        let lease = args.get(3).filter(|value| !matches!(value, VmValue::Nil));
        let mut argv = vec!["git".to_string(), "push".to_string()];
        let mut mutation = GitMutation::Mutating;
        if let Some(lease) = lease {
            let lease = parse_lease(lease, &refspec)?;
            if let Some(actual_oid) =
                verify_force_with_lease(&repo, &remote, &lease.ref_name, &lease.expected_oid)
                    .await?
            {
                let message = format!(
                    "git.push: lease_mismatch for {}; expected {}, found {}",
                    lease.ref_name, lease.expected_oid, actual_oid
                );
                return synthetic_receipt(
                    GitCommand {
                        operation: "git.push",
                        action: "git.push",
                        cwd: repo,
                        argv: vec![
                            "git".to_string(),
                            "push".to_string(),
                            format!(
                                "--force-with-lease={}:{}",
                                lease.ref_name, lease.expected_oid
                            ),
                            remote,
                            refspec.clone(),
                        ],
                        mutation: GitMutation::Risky,
                        affected_paths: Vec::new(),
                        data_parser: GitDataParser::Push {
                            refspec: refspec.clone(),
                        },
                    },
                    false,
                    "lease_mismatch",
                    json!({
                        "ref": lease.ref_name,
                        "expected_oid": lease.expected_oid,
                        "actual_oid": actual_oid,
                        "pushed": false,
                    }),
                    message,
                )
                .await;
            }
            argv.push(format!(
                "--force-with-lease={}:{}",
                lease.ref_name, lease.expected_oid
            ));
            mutation = GitMutation::Risky;
        }
        argv.push(remote);
        argv.push(refspec.clone());
        run_git_command(
            Some(&ctx),
            GitCommand {
                operation: "git.push",
                action: "git.push",
                cwd: repo,
                argv,
                mutation,
                affected_paths: Vec::new(),
                data_parser: GitDataParser::Push { refspec },
            },
        )
        .await
    });

    vm.register_async_builtin("git.diff", |ctx, args| async move {
        let repo = repo_path_arg(&args, 0, "git.diff")?;
        let selector = args.get(1);
        let mut argv = vec!["git".to_string(), "diff".to_string()];
        match selector {
            Some(VmValue::String(range)) if !range.is_empty() => argv.push(range.to_string()),
            Some(VmValue::List(paths)) => {
                argv.push("--".to_string());
                argv.extend(string_list_value(paths, "git.diff", "paths")?);
            }
            Some(VmValue::Dict(options)) => {
                if let Some(range) = string_option(Some(options), "range") {
                    argv.push(range);
                }
                if let Some(paths) = string_list_option(Some(options), "paths")? {
                    argv.push("--".to_string());
                    argv.extend(paths);
                }
            }
            _ => {}
        }
        run_git_command(
            Some(&ctx),
            GitCommand {
                operation: "git.diff",
                action: "git.diff",
                cwd: repo,
                argv,
                mutation: GitMutation::Read,
                affected_paths: Vec::new(),
                data_parser: GitDataParser::Diff,
            },
        )
        .await
    });

    vm.register_async_builtin("git.merge_base", |ctx, args| async move {
        let repo = repo_path_arg(&args, 0, "git.merge_base")?;
        let left = required_string_arg(&args, 1, "git.merge_base", "left")?;
        let right = required_string_arg(&args, 2, "git.merge_base", "right")?;
        run_git_command(
            Some(&ctx),
            GitCommand {
                operation: "git.merge_base",
                action: "git.merge_base",
                cwd: repo,
                argv: vec!["git".to_string(), "merge-base".to_string(), left, right],
                mutation: GitMutation::Read,
                affected_paths: Vec::new(),
                data_parser: GitDataParser::MergeBase,
            },
        )
        .await
    });
}

fn register_git_namespace(vm: &mut Vm) {
    let repo = namespace(&[("discover", "git.repo.discover")]);
    let worktree = namespace(&[
        ("create", "git.worktree.create"),
        ("remove", "git.worktree.remove"),
    ]);
    let mut root = crate::value::DictMap::new();
    root.put_str("_namespace", "git");
    root.insert("repo".to_string(), repo);
    root.insert("worktree".to_string(), worktree);
    for (name, builtin) in [
        ("fetch", "git.fetch"),
        ("rebase", "git.rebase"),
        ("status", "git.status"),
        ("conflicts", "git.conflicts"),
        ("push", "git.push"),
        ("diff", "git.diff"),
        ("merge_base", "git.merge_base"),
        ("repo_discover", "git.repo.discover"),
        ("worktree_create", "git.worktree.create"),
        ("worktree_remove", "git.worktree.remove"),
    ] {
        root.insert(
            name.to_string(),
            VmValue::BuiltinRef(arcstr::ArcStr::from(builtin)),
        );
    }
    vm.set_global("git", VmValue::dict(root));
}

fn namespace(entries: &[(&str, &str)]) -> VmValue {
    VmValue::dict(
        entries
            .iter()
            .map(|(name, builtin)| {
                (
                    name.to_string(),
                    VmValue::BuiltinRef(arcstr::ArcStr::from(*builtin)),
                )
            })
            .collect::<crate::value::DictMap>(),
    )
}

async fn run_git_command(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    command: GitCommand,
) -> Result<VmValue, VmError> {
    if should_plan(&command) {
        return planned_receipt(command).await;
    }

    let approval = enforce_git_approval(ctx, &command).await?;
    let result = exec_argv(&command).await?;
    let result_json = crate::llm::vm_value_to_json(&result);
    let status = command_status(&result_json);
    let success = result_json
        .get("success")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let data = parse_git_data(&command.data_parser, &result_json, success)?;
    let receipt = build_receipt(&command, &result_json, status, data, approval);
    persist_receipt_and_trust(&receipt, &command, success).await?;
    Ok(crate::stdlib::json_to_vm_value(&receipt))
}

async fn planned_or_noop_receipt(
    operation: &'static str,
    action: &'static str,
    mutation: GitMutation,
    cwd: PathBuf,
    argv: Vec<String>,
    affected_paths: Vec<String>,
    data: JsonValue,
    status: &str,
) -> Result<VmValue, VmError> {
    let command = GitCommand {
        operation,
        action,
        cwd,
        argv,
        mutation,
        affected_paths,
        data_parser: GitDataParser::None,
    };
    if should_plan(&command) {
        return planned_receipt(command).await;
    }
    let result = json!({
        "status": status,
        "success": true,
        "stdout": "",
        "stderr": "",
        "exit_code": 0,
    });
    let receipt = build_receipt(&command, &result, status.to_string(), data, None);
    persist_receipt_and_trust(&receipt, &command, true).await?;
    Ok(crate::stdlib::json_to_vm_value(&receipt))
}

async fn synthetic_receipt(
    command: GitCommand,
    success: bool,
    status: &str,
    data: JsonValue,
    stderr: String,
) -> Result<VmValue, VmError> {
    let result = json!({
        "status": status,
        "success": success,
        "stdout": "",
        "stderr": stderr,
        "exit_code": if success { 0 } else { -1 },
    });
    let receipt = build_receipt(&command, &result, status.to_string(), data, None);
    persist_receipt_and_trust(&receipt, &command, success).await?;
    Ok(crate::stdlib::json_to_vm_value(&receipt))
}

fn should_plan(command: &GitCommand) -> bool {
    if command.mutation == GitMutation::Read {
        return false;
    }
    crate::triggers::dispatcher::current_dispatch_context().is_some_and(|context| {
        matches!(
            context.autonomy_tier,
            AutonomyTier::Shadow | AutonomyTier::Suggest
        )
    })
}

async fn planned_receipt(command: GitCommand) -> Result<VmValue, VmError> {
    let result = json!({
        "status": "planned",
        "success": true,
        "stdout": "",
        "stderr": "",
        "exit_code": 0,
    });
    let data = json!({
        "planned": true,
        "argv": command.argv.clone(),
        "cwd": display_path(&command.cwd),
    });
    let receipt = build_receipt(&command, &result, "planned".to_string(), data, None);
    persist_receipt_and_trust(&receipt, &command, true).await?;
    Ok(crate::stdlib::json_to_vm_value(&receipt))
}

/// Environment variables that, when set on the calling process, would
/// override the `cwd`/`argv`-based repo selection that the Harn git
/// stdlib relies on. Strip them from every subprocess invocation so
/// behavior is identical whether Harn is launched from a clean shell
/// or from inside a git hook (which sets `GIT_DIR` etc. for the hook
/// process and any of its descendants).
///
/// Reference: <https://git-scm.com/docs/git#_environment_variables>.
const GIT_ENV_OVERRIDES: &[&str] = &[
    "GIT_DIR",
    "GIT_INDEX_FILE",
    "GIT_WORK_TREE",
    "GIT_NAMESPACE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_PREFIX",
];

/// Non-interactive guard applied to every git subprocess Harn spawns.
///
/// Harn runs git from TTY-less contexts (`harn serve`, `@job`, CI). A
/// fetch/push/clone that needs credentials or an SSH host-key decision
/// would otherwise block on an interactive prompt and hang the runtime
/// indefinitely. These variables tell git (and the ssh transport) to
/// fail fast instead of prompting:
///
/// * `GIT_TERMINAL_PROMPT=0` — disable git's own terminal prompting
///   (HTTPS username/password, missing-credential prompts).
/// * `GIT_ASKPASS` / `SSH_ASKPASS` empty — refuse to shell out to a GUI
///   or external askpass helper for a passphrase.
/// * `GIT_SSH_COMMAND` with `BatchMode=yes` — make the ssh transport
///   non-interactive (no passphrase / host-key prompts).
///
/// This is a *prompt* guard only: it never removes credentials. Auth via
/// environment, `.netrc`, credential helpers, or pre-loaded ssh-agent
/// keys continues to work because those paths are non-interactive. The
/// env is merged (`env_mode: "merge"`) so inherited PATH/HOME and any
/// caller-supplied credentials survive.
const GIT_NONINTERACTIVE_ENV: &[(&str, &str)] = &[
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_ASKPASS", ""),
    ("SSH_ASKPASS", ""),
    ("GIT_SSH_COMMAND", "ssh -oBatchMode=yes"),
];

async fn exec_argv(command: &GitCommand) -> Result<VmValue, VmError> {
    let mut params = crate::value::DictMap::new();
    params.put_str("mode", "argv");
    params.insert(
        "argv".to_string(),
        VmValue::List(std::sync::Arc::new(
            command
                .argv
                .iter()
                .map(|arg| VmValue::String(arcstr::ArcStr::from(arg.as_str())))
                .collect(),
        )),
    );
    params.put_str("cwd", display_path(&command.cwd));
    params.insert("timeout_ms".to_string(), VmValue::Int(120_000));
    params.insert(
        "env_remove".to_string(),
        VmValue::List(std::sync::Arc::new(
            GIT_ENV_OVERRIDES
                .iter()
                .map(|name| VmValue::String(arcstr::ArcStr::from(*name)))
                .collect(),
        )),
    );
    // Make every git subprocess non-interactive by default so a
    // credential / host-key prompt fails fast instead of hanging a
    // TTY-less runtime. `env_mode: "merge"` keeps inherited PATH/HOME
    // and any credentials already present in the environment.
    let mut env = crate::value::DictMap::new();
    for (key, value) in GIT_NONINTERACTIVE_ENV {
        env.insert(
            (*key).to_string(),
            VmValue::String(arcstr::ArcStr::from(*value)),
        );
    }
    params.insert("env".to_string(), VmValue::dict(env));
    params.put_str("env_mode", "merge");
    let caller = json!({
        "surface": "stdlib.git",
        "operation": command.operation,
        "session_id": crate::llm::current_agent_session_id(),
    });
    crate::stdlib::host::dispatch_process_exec(&params, caller).await
}

async fn enforce_git_approval(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    command: &GitCommand,
) -> Result<Option<JsonValue>, VmError> {
    let args = json!({
        "operation": command.operation,
        "argv": command.argv,
        "cwd": display_path(&command.cwd),
        "affected_paths": command.affected_paths,
    });
    if command.mutation == GitMutation::Risky {
        let approval = request_permission(ctx, command.operation, &args, None).await?;
        return Ok(Some(approval));
    }
    let Some(policy) = crate::orchestration::current_approval_policy() else {
        return Ok(None);
    };
    let decision = policy.evaluate_detailed(command.operation, &args);
    if decision.is_allow() {
        Ok(None)
    } else if decision.is_deny() {
        Err(VmError::CategorizedError {
            message: decision.reason,
            category: crate::value::ErrorCategory::ToolRejected,
        })
    } else {
        request_permission(ctx, command.operation, &args, Some(decision.receipt))
            .await
            .map(Some)
    }
}

async fn request_permission(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    operation: &str,
    args: &JsonValue,
    policy_decision: Option<JsonValue>,
) -> Result<JsonValue, VmError> {
    let Some(bridge) = ctx.and_then(|ctx| ctx.child_vm().bridge.clone()) else {
        return Err(VmError::CategorizedError {
            message: format!("{operation}: approval required but no host bridge is attached"),
            category: crate::value::ErrorCategory::ToolRejected,
        });
    };
    let approval_id = format!("git-{}", Uuid::now_v7());
    let approval_request = crate::stdlib::hitl::approval_request_for_host_permission(
        approval_id.clone(),
        operation.to_string(),
        args.clone(),
        crate::llm::current_agent_session_id().unwrap_or_else(|| "harn".to_string()),
        Vec::new(),
        policy_decision
            .as_ref()
            .map(|decision| json!({"policy_decision": decision}))
            .unwrap_or(JsonValue::Null),
        vec![format!("stdlib.{operation}")],
    );
    let approval_request_json = serde_json::to_value(&approval_request).unwrap_or(JsonValue::Null);
    let response = bridge
        .call(
            "session/request_permission",
            crate::llm::acp_permission::request_params(
                crate::llm::current_agent_session_id().as_deref(),
                &approval_id,
                operation,
                args,
                approval_request_json,
                &policy_decision.clone().unwrap_or(JsonValue::Null),
                None,
            ),
        )
        .await?;
    match crate::llm::acp_permission::parse_response(&response) {
        crate::llm::acp_permission::WireOutcome::Allowed => Ok(response),
        crate::llm::acp_permission::WireOutcome::Rejected { reason } => {
            Err(VmError::CategorizedError {
                message: format!("{operation}: approval denied: {reason}"),
                category: crate::value::ErrorCategory::ToolRejected,
            })
        }
    }
}

async fn verify_force_with_lease(
    repo: &Path,
    remote: &str,
    ref_name: &str,
    expected_oid: &str,
) -> Result<Option<String>, VmError> {
    let command = GitCommand {
        operation: "git.push.lease_check",
        action: "git.push.lease_check",
        cwd: repo.to_path_buf(),
        argv: vec![
            "git".to_string(),
            "ls-remote".to_string(),
            remote.to_string(),
            ref_name.to_string(),
        ],
        mutation: GitMutation::Read,
        affected_paths: Vec::new(),
        data_parser: GitDataParser::None,
    };
    let output = exec_argv(&command).await?;
    let output_json = crate::llm::vm_value_to_json(&output);
    let actual = output_json
        .get("stdout")
        .and_then(|value| value.as_str())
        .and_then(|stdout| stdout.split_whitespace().next())
        .unwrap_or("");
    if actual == expected_oid {
        return Ok(None);
    }
    Ok(Some(
        if actual.is_empty() {
            "<missing>"
        } else {
            actual
        }
        .to_string(),
    ))
}

#[derive(Clone, Debug)]
struct Lease {
    ref_name: String,
    expected_oid: String,
}

fn parse_lease(value: &VmValue, refspec: &str) -> Result<Lease, VmError> {
    let default_ref = refspec
        .split(':')
        .next_back()
        .filter(|value| !value.is_empty())
        .unwrap_or(refspec)
        .to_string();
    match value {
        VmValue::Dict(map) => {
            let expected_oid = string_option(Some(map), "expected_oid")
                .or_else(|| string_option(Some(map), "oid"))
                .ok_or_else(|| {
                    VmError::Runtime("git.push: force_with_lease requires expected_oid".to_string())
                })?;
            let ref_name = string_option(Some(map), "ref")
                .or_else(|| string_option(Some(map), "ref_name"))
                .unwrap_or(default_ref);
            Ok(Lease {
                ref_name,
                expected_oid,
            })
        }
        VmValue::String(expected_oid) if !expected_oid.is_empty() => Ok(Lease {
            ref_name: default_ref,
            expected_oid: expected_oid.to_string(),
        }),
        _ => Err(VmError::Runtime(
            "git.push: lease must be {ref, expected_oid} or expected_oid string".to_string(),
        )),
    }
}

fn build_receipt(
    command: &GitCommand,
    result: &JsonValue,
    status: String,
    data: JsonValue,
    approval: Option<JsonValue>,
) -> JsonValue {
    let context = identity_context();
    let stdout = result
        .get("stdout")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let stderr = result
        .get("stderr")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let exit_code = result
        .get("exit_code")
        .and_then(|value| value.as_i64())
        .unwrap_or(-1);
    let success = result
        .get("success")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    json!({
        "schema": "harn-stdlib-git-receipt-v1",
        "receipt_id": format!("git-receipt-{}", Uuid::now_v7()),
        "operation": command.operation,
        "action": command.action,
        "status": status,
        "success": success,
        "exit_category": exit_category(status_from_result(result), exit_code, success),
        "exit_code": exit_code,
        "command_args": command.argv.clone(),
        "argv": command.argv.clone(),
        "working_dir": display_path(&command.cwd),
        "cwd": display_path(&command.cwd),
        "affected_paths": command.affected_paths.clone(),
        "agent": context.agent,
        "trace_id": context.trace_id,
        "autonomy_tier": context.autonomy_tier,
        "stdout": output_ref("stdout", stdout),
        "stderr": output_ref("stderr", stderr),
        "command_policy": result.get("command_policy").cloned().unwrap_or(JsonValue::Null),
        "approval": approval,
        "data": data,
        "repo": data.get("repo").cloned().unwrap_or(JsonValue::Null),
    })
}

async fn persist_receipt_and_trust(
    receipt: &JsonValue,
    command: &GitCommand,
    success: bool,
) -> Result<(), VmError> {
    let log = active_event_log()
        .unwrap_or_else(|| install_memory_for_current_thread(EVENT_LOG_QUEUE_DEPTH));
    let topic = Topic::new(GIT_RECEIPTS_TOPIC)
        .map_err(|error| VmError::Runtime(format!("git receipt topic: {error}")))?;
    let mut headers = std::collections::BTreeMap::new();
    if let Some(trace_id) = receipt.get("trace_id").and_then(|value| value.as_str()) {
        headers.insert("trace_id".to_string(), trace_id.to_string());
    }
    if let Some(agent) = receipt.get("agent").and_then(|value| value.as_str()) {
        headers.insert("agent".to_string(), agent.to_string());
    }
    log.append(
        &topic,
        LogEvent::new("stdlib.git.receipt", receipt.clone()).with_headers(headers),
    )
    .await
    .map_err(|error| VmError::Runtime(format!("git receipt append: {error}")))?;

    let context = identity_context();
    let mut record = TrustRecord::new(
        context.agent,
        command.action,
        approval_approver(receipt),
        if success {
            TrustOutcome::Success
        } else {
            TrustOutcome::Failure
        },
        context.trace_id,
        context.tier,
    );
    record
        .metadata
        .insert("receipt".to_string(), receipt.clone());
    record.metadata.insert(
        "weight".to_string(),
        json!(if command.mutation == GitMutation::Risky {
            "high"
        } else {
            "normal"
        }),
    );
    crate::trust_graph::append_trust_record(&log, &record)
        .await
        .map_err(|error| VmError::Runtime(format!("git trust graph append: {error}")))?;
    Ok(())
}

fn approval_approver(receipt: &JsonValue) -> Option<String> {
    receipt
        .get("approval")
        .and_then(|value| value.get("approver").or_else(|| value.get("reviewer")))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

struct IdentityContext {
    agent: String,
    trace_id: String,
    autonomy_tier: String,
    tier: AutonomyTier,
}

fn identity_context() -> IdentityContext {
    if let Some(context) = crate::triggers::dispatcher::current_dispatch_context() {
        return IdentityContext {
            agent: context.agent_id,
            trace_id: context.trigger_event.trace_id.0,
            autonomy_tier: context.autonomy_tier.as_str().to_string(),
            tier: context.autonomy_tier,
        };
    }
    IdentityContext {
        agent: crate::llm::current_agent_session_id().unwrap_or_else(|| "harn".to_string()),
        trace_id: format!("trace-{}", Uuid::now_v7()),
        autonomy_tier: AutonomyTier::ActAuto.as_str().to_string(),
        tier: AutonomyTier::ActAuto,
    }
}

fn output_ref(kind: &str, text: &str) -> JsonValue {
    const INLINE_LIMIT: usize = 8192;
    let inline_end = if text.len() > INLINE_LIMIT {
        text.char_indices()
            .map(|(idx, _)| idx)
            .take_while(|idx| *idx <= INLINE_LIMIT)
            .last()
            .unwrap_or(0)
    } else {
        text.len()
    };
    json!({
        "kind": kind,
        "bytes": text.len(),
        "sha256": sha256_hex(text.as_bytes()),
        "inline": &text[..inline_end],
        "truncated": text.len() > INLINE_LIMIT,
    })
}

fn parse_git_data(
    parser: &GitDataParser,
    result: &JsonValue,
    success: bool,
) -> Result<JsonValue, VmError> {
    let stdout = result
        .get("stdout")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    Ok(match parser {
        GitDataParser::None => JsonValue::Null,
        GitDataParser::Discover { input } => parse_discover(stdout, input),
        GitDataParser::Status => parse_status(stdout),
        GitDataParser::Conflicts => parse_conflicts(stdout),
        GitDataParser::MergeBase => json!({"oid": stdout.trim()}),
        GitDataParser::Diff => json!({"diff": stdout}),
        GitDataParser::WorktreeCreate { branch, path } => {
            json!({"branch": branch, "path": path, "created": success})
        }
        GitDataParser::WorktreeRemove { path } => json!({"path": path, "removed": success}),
        GitDataParser::Push { refspec } => json!({"refspec": refspec, "pushed": success}),
        GitDataParser::Fetch => json!({"fetched": success}),
        GitDataParser::Rebase => json!({"rebased": success}),
    })
}

fn parse_discover(stdout: &str, input: &str) -> JsonValue {
    let mut lines = stdout.lines();
    let root = lines.next().unwrap_or("").to_string();
    let git_dir = lines.next().unwrap_or("").to_string();
    let bare = lines.next().unwrap_or("false") == "true";
    let inside_work_tree = lines.next().unwrap_or("false") == "true";
    json!({
        "repo": {
            "path": root,
            "root": root,
            "git_dir": git_dir,
            "bare": bare,
            "inside_work_tree": inside_work_tree,
            "input": input,
        }
    })
}

fn parse_status(stdout: &str) -> JsonValue {
    let mut branch = JsonValue::Null;
    let mut entries = Vec::new();
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            branch = json!(rest);
            continue;
        }
        if line.len() < 3 {
            continue;
        }
        let xy = &line[0..2];
        let path = line[3..].to_string();
        entries.push(json!({
            "xy": xy,
            "index": &xy[0..1],
            "worktree": &xy[1..2],
            "path": path,
            "conflict_kind": conflict_kind(xy),
        }));
    }
    json!({
        "branch": branch,
        "entries": entries,
        "dirty": !entries.is_empty(),
    })
}

fn parse_conflicts(stdout: &str) -> JsonValue {
    let conflicts = stdout
        .lines()
        .filter_map(|line| {
            if line.len() < 3 {
                return None;
            }
            let xy = &line[0..2];
            conflict_kind(xy).map(|kind| {
                json!({
                    "path": line[3..].to_string(),
                    "xy": xy,
                    "kind": kind,
                })
            })
        })
        .collect::<Vec<_>>();
    json!({
        "conflicts": conflicts,
        "has_conflicts": !conflicts.is_empty(),
    })
}

fn conflict_kind(xy: &str) -> Option<&'static str> {
    match xy {
        "DD" => Some("both_deleted"),
        "AU" => Some("added_by_us"),
        "UD" => Some("deleted_by_them"),
        "UA" => Some("added_by_them"),
        "DU" => Some("deleted_by_us"),
        "AA" => Some("both_added"),
        "UU" => Some("content"),
        _ => None,
    }
}

fn command_status(result: &JsonValue) -> String {
    status_from_result(result).to_string()
}

fn status_from_result(result: &JsonValue) -> &str {
    result
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or_else(|| {
            if result
                .get("success")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            {
                "completed"
            } else {
                "failed"
            }
        })
}

fn exit_category(status: &str, exit_code: i64, success: bool) -> &'static str {
    if status == "planned" {
        "planned"
    } else if status == "blocked" {
        "policy_blocked"
    } else if status == "timed_out" {
        "timeout"
    } else if success {
        "success"
    } else if exit_code == 128 {
        "git_fatal"
    } else {
        "nonzero_exit"
    }
}

fn repo_path_arg(args: &[VmValue], index: usize, builtin: &str) -> Result<PathBuf, VmError> {
    let value = args
        .get(index)
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: missing repo")))?;
    match value {
        VmValue::String(path) if !path.is_empty() => Ok(PathBuf::from(path.as_str())),
        VmValue::Dict(map) => {
            if let Some(repo) = map.get("repo").and_then(|value| value.as_dict()) {
                return repo_path_from_map(repo, builtin);
            }
            if let Some(data) = map.get("data").and_then(|value| value.as_dict()) {
                if let Some(repo) = data.get("repo").and_then(|value| value.as_dict()) {
                    return repo_path_from_map(repo, builtin);
                }
            }
            repo_path_from_map(map, builtin)
        }
        other => Err(VmError::TypeError(format!(
            "{builtin}: repo must be a path string or repo dict, got {}",
            other.type_name()
        ))),
    }
}

fn repo_path_from_map(map: &crate::value::DictMap, builtin: &str) -> Result<PathBuf, VmError> {
    for key in ["root", "path", "cwd"] {
        if let Some(VmValue::String(path)) = map.get(key) {
            if !path.is_empty() {
                return Ok(PathBuf::from(path.as_str()));
            }
        }
    }
    Err(VmError::Runtime(format!(
        "{builtin}: repo dict must include root or path"
    )))
}

fn required_path_arg(args: &[VmValue], index: usize, builtin: &str) -> Result<PathBuf, VmError> {
    required_string_arg(args, index, builtin, "path").map(PathBuf::from)
}

fn required_string_arg(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    name: &str,
) -> Result<String, VmError> {
    match args.get(index) {
        Some(VmValue::String(value)) if !value.is_empty() => Ok(value.to_string()),
        Some(other) => Err(VmError::TypeError(format!(
            "{builtin}: {name} must be a non-empty string, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!("{builtin}: missing {name}"))),
    }
}

fn optional_dict_arg(args: &[VmValue], index: usize) -> Option<&crate::value::DictMap> {
    args.get(index).and_then(|value| match value {
        VmValue::Dict(map) => Some(map.as_ref()),
        _ => None,
    })
}

fn string_option(map: Option<&crate::value::DictMap>, key: &str) -> Option<String> {
    map.and_then(|map| map.get(key))
        .and_then(|value| match value {
            VmValue::String(value) if !value.is_empty() => Some(value.to_string()),
            _ => None,
        })
}

fn bool_option(map: Option<&crate::value::DictMap>, key: &str) -> Option<bool> {
    map.and_then(|map| map.get(key))
        .and_then(|value| match value {
            VmValue::Bool(value) => Some(*value),
            _ => None,
        })
}

fn string_list_arg(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    name: &str,
) -> Result<Option<Vec<String>>, VmError> {
    match args.get(index) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::List(values)) => string_list_value(values, builtin, name).map(Some),
        Some(other) => Err(VmError::TypeError(format!(
            "{builtin}: {name} must be a list<string>, got {}",
            other.type_name()
        ))),
    }
}

fn string_list_option(
    map: Option<&crate::value::DictMap>,
    key: &str,
) -> Result<Option<Vec<String>>, VmError> {
    let Some(value) = map.and_then(|map| map.get(key)) else {
        return Ok(None);
    };
    match value {
        VmValue::List(values) => string_list_value(values, "git.diff", "paths").map(Some),
        other => Err(VmError::TypeError(format!(
            "git.diff: paths must be a list<string>, got {}",
            other.type_name()
        ))),
    }
}

fn string_list_value(
    values: &[VmValue],
    builtin: &str,
    name: &str,
) -> Result<Vec<String>, VmError> {
    values
        .iter()
        .map(|value| match value {
            VmValue::String(value) => Ok(value.to_string()),
            other => Err(VmError::TypeError(format!(
                "{builtin}: {name} entries must be strings, got {}",
                other.type_name()
            ))),
        })
        .collect()
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust_graph::{query_trust_records, TrustQueryFilters};
    use std::fs;
    use std::process::Command;

    fn require_git() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn git(cwd: &Path, args: &[&str]) {
        let mut command = Command::new("git");
        command.args(args).current_dir(cwd);
        // Mirror the production `exec_argv` env scrub so these tests
        // also pass when invoked from inside a git hook (which sets
        // `GIT_DIR=.git` for the hook process and its descendants —
        // without this, a child `git init` in a temp dir reuses the
        // outer repo's index and fails).
        for name in super::GIT_ENV_OVERRIDES {
            command.env_remove(name);
        }
        let output = command.output().expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        git(dir.path(), &["init", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "harn@example.test"]);
        git(dir.path(), &["config", "user.name", "Harn Test"]);
        fs::write(dir.path().join("README.md"), "initial\n").expect("write readme");
        git(dir.path(), &["add", "README.md"]);
        git(dir.path(), &["commit", "-m", "initial"]);
        dir
    }

    async fn run_on_local<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::task::LocalSet::new().run_until(future).await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn git_status_returns_receipt_and_trust_record() {
        if !require_git() {
            return;
        }
        crate::event_log::reset_active_event_log();
        crate::stdlib::reset_stdlib_state();
        let repo = init_repo();
        fs::write(repo.path().join("README.md"), "changed\n").expect("modify readme");

        run_on_local(async {
            let receipt = run_git_command(
                None,
                GitCommand {
                    operation: "git.status",
                    action: "git.status",
                    cwd: repo.path().to_path_buf(),
                    argv: vec![
                        "git".to_string(),
                        "status".to_string(),
                        "--porcelain=v1".to_string(),
                        "--branch".to_string(),
                    ],
                    mutation: GitMutation::Read,
                    affected_paths: Vec::new(),
                    data_parser: GitDataParser::Status,
                },
            )
            .await
            .expect("git status receipt");
            let json = crate::llm::vm_value_to_json(&receipt);
            assert_eq!(json["schema"], "harn-stdlib-git-receipt-v1");
            assert_eq!(json["operation"], "git.status");
            assert_eq!(json["success"], true);
            assert_eq!(json["data"]["dirty"], true);
            assert_eq!(json["data"]["entries"][0]["path"], "README.md");

            let log = active_event_log().expect("git receipt installed event log");
            let records = query_trust_records(
                &log,
                &TrustQueryFilters {
                    action: Some("git.status".to_string()),
                    ..TrustQueryFilters::default()
                },
            )
            .await
            .expect("query trust records");
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].outcome, TrustOutcome::Success);
            assert_eq!(
                records[0].metadata["receipt"]["operation"],
                JsonValue::String("git.status".to_string())
            );
        })
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn git_subprocess_carries_noninteractive_prompt_guard() {
        // Every git subprocess Harn spawns must be non-interactive by
        // default so a credential / host-key prompt fails fast instead
        // of hanging a TTY-less runtime (`harn serve`, `@job`, CI). We
        // run a probe through the same `exec_argv` path the receipt git
        // builtins use and assert the guard env reached the child while
        // an inherited env var survived the merge (proving `env_mode:
        // "merge"`, not a clobbering replace). The inherited var uses a
        // unique name so it can't race other tests in a shared process.
        crate::stdlib::reset_stdlib_state();
        std::env::set_var("HARN_GUARD_PROBE_INHERIT", "kept");
        run_on_local(async {
            let result = exec_argv(&GitCommand {
                operation: "git.status",
                action: "git.status",
                cwd: std::env::temp_dir(),
                argv: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "printf '%s|%s' \"$GIT_TERMINAL_PROMPT\" \"$HARN_GUARD_PROBE_INHERIT\""
                        .to_string(),
                ],
                mutation: GitMutation::Read,
                affected_paths: Vec::new(),
                data_parser: GitDataParser::None,
            })
            .await
            .expect("exec_argv probe");
            let json = crate::llm::vm_value_to_json(&result);
            let stdout = json["stdout"].as_str().unwrap_or_default();
            let (prompt, inherited) = stdout.split_once('|').unwrap_or_default();
            assert_eq!(
                prompt, "0",
                "git subprocess must inherit GIT_TERMINAL_PROMPT=0; stdout was {stdout:?}"
            );
            assert_eq!(
                inherited, "kept",
                "env_mode=merge must preserve inherited env; stdout was {stdout:?}"
            );
        })
        .await;
        std::env::remove_var("HARN_GUARD_PROBE_INHERIT");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn force_with_lease_detects_advanced_remote_before_push() {
        if !require_git() {
            return;
        }
        crate::event_log::reset_active_event_log();
        crate::stdlib::reset_stdlib_state();
        let remote = tempfile::tempdir().expect("remote");
        git(remote.path(), &["init", "--bare"]);
        let one = tempfile::tempdir().expect("clone one");
        let two = tempfile::tempdir().expect("clone two");
        git(
            one.path(),
            &["clone", remote.path().to_str().unwrap(), "repo"],
        );
        git(
            two.path(),
            &["clone", remote.path().to_str().unwrap(), "repo"],
        );
        let one_repo = one.path().join("repo");
        let two_repo = two.path().join("repo");
        git(&one_repo, &["config", "user.email", "harn@example.test"]);
        git(&one_repo, &["config", "user.name", "Harn Test"]);
        git(&two_repo, &["config", "user.email", "harn@example.test"]);
        git(&two_repo, &["config", "user.name", "Harn Test"]);
        fs::write(one_repo.join("file.txt"), "one\n").expect("write one");
        git(&one_repo, &["add", "file.txt"]);
        git(&one_repo, &["commit", "-m", "one"]);
        git(&one_repo, &["push", "origin", "HEAD:refs/heads/main"]);
        let expected_oid = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "refs/remotes/origin/main"])
                .current_dir(&one_repo)
                .output()
                .expect("rev-parse")
                .stdout,
        )
        .expect("utf8")
        .trim()
        .to_string();

        git(&two_repo, &["fetch", "origin", "main"]);
        git(&two_repo, &["checkout", "-B", "main", "origin/main"]);
        fs::write(two_repo.join("file.txt"), "two\n").expect("write two");
        git(&two_repo, &["commit", "-am", "two"]);
        git(&two_repo, &["push", "origin", "HEAD:refs/heads/main"]);

        fs::write(one_repo.join("file.txt"), "one advanced\n").expect("write one advanced");
        git(&one_repo, &["commit", "-am", "one advanced"]);

        run_on_local(async {
            let mismatch =
                verify_force_with_lease(&one_repo, "origin", "refs/heads/main", &expected_oid)
                    .await
                    .expect("lease check should complete");
            assert!(
                mismatch.is_some(),
                "expected advanced remote to produce a lease mismatch"
            );
        })
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn absent_worktree_remove_is_idempotent_receipt() {
        if !require_git() {
            return;
        }
        crate::event_log::reset_active_event_log();
        crate::stdlib::reset_stdlib_state();
        let root = tempfile::tempdir().expect("root");
        let missing = root.path().join("missing-worktree");
        run_on_local(async {
            let receipt = planned_or_noop_receipt(
                "git.worktree.remove",
                "git.worktree.remove",
                GitMutation::Mutating,
                root.path().to_path_buf(),
                vec![
                    "git".to_string(),
                    "worktree".to_string(),
                    "remove".to_string(),
                    display_path(&missing),
                ],
                vec![display_path(&missing)],
                json!({"path": display_path(&missing), "removed": false, "idempotent": true}),
                "no_op",
            )
            .await
            .expect("receipt");
            let json = crate::llm::vm_value_to_json(&receipt);
            assert_eq!(json["status"], "no_op");
            assert_eq!(json["success"], true);
            assert_eq!(json["data"]["idempotent"], true);
        })
        .await;
    }

    #[test]
    fn string_list_value_rejects_non_string_entries() {
        let err = string_list_value(&[VmValue::Int(1)], "git.diff", "paths")
            .expect_err("non-string path should be rejected");
        assert!(
            matches!(err, VmError::TypeError(message) if message.contains("paths entries must be strings"))
        );
    }
}
