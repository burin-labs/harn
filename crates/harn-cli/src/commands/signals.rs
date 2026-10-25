//! `harn signals`: the ambient-signal inbox from the command line, and the
//! hook adapter agent harnesses run at their safe points.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process;
use std::time::Duration as StdDuration;

use harn_signals::{
    Activity, CheckIn, CheckInScope, HarnessKind, IdlePoll, Inbox, ManualEvent, Provenance,
    Reminder, ReminderId, RoutingHints, SessionAnnouncement, Severity, Signal, SignalInput,
    SignatureStatus, TriageEvent,
};
use harn_vm::stdlib::signals::MAX_REMINDERS_PER_DELIVERY;
use serde::Deserialize;
use time::OffsetDateTime;

use crate::cli::{
    SignalSeverityArg, SignalSignatureArg, SignalsArgs, SignalsCommand, SignalsFireArgs,
    SignalsHookArgs, SignalsHookEvent, SignalsHookFormat,
};
use crate::json_envelope::{to_string_pretty, JsonEnvelope};

pub(crate) const SIGNALS_SCHEMA_VERSION: u32 = 1;
const ROLE_ENV: &str = "HARN_EXT_SIGNALS_ROLE";
const REWAKE_POLL: StdDuration = StdDuration::from_secs(2);
/// Claude Code's exit code for "wake the session with my stderr".
const REWAKE_EXIT: i32 = 2;

pub(crate) fn run(args: SignalsArgs) -> Result<(), String> {
    let is_hook = matches!(args.command, SignalsCommand::Hook(_));
    let result = run_command(args);
    if is_hook {
        // The CLI's usual error exit is 2, which Claude Code reads as "block
        // the stop" or "show Claude this". A broken hook must never steer a
        // session, so it reports and exits 1, which harnesses treat as a
        // non-blocking hook error.
        if let Err(error) = result {
            eprintln!("harn signals hook: {error}");
            process::exit(1);
        }
    }
    result
}

fn run_command(args: SignalsArgs) -> Result<(), String> {
    let dir = match args.dir {
        Some(dir) => dir,
        None => Inbox::default_dir().map_err(|error| error.to_string())?,
    };
    let inbox = Inbox::open(dir).map_err(|error| error.to_string())?;
    let now = harn_vm::stdlib::signals::vm_now();
    match args.command {
        SignalsCommand::List(output) => {
            let signals: Vec<Signal> = snapshot(&inbox, now)?.signals.into_values().collect();
            print_signals(&inbox, &signals, now, output.json)
        }
        SignalsCommand::Pending(pending) => {
            let signals: Vec<Signal> = snapshot(&inbox, now)?
                .signals
                .into_values()
                .filter(|signal| {
                    signal.is_pending()
                        && pending.session.as_ref().is_none_or(|session| {
                            signal
                                .delivery
                                .as_ref()
                                .is_some_and(|delivery| &delivery.session_id == session)
                        })
                })
                .collect();
            print_signals(&inbox, &signals, now, pending.json)
        }
        SignalsCommand::Sessions(output) => {
            let sessions: Vec<_> = snapshot(&inbox, now)?.sessions.into_values().collect();
            if output.json {
                return print_json(&sessions);
            }
            for session in sessions {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    session.id,
                    session.harness,
                    match session.activity {
                        Activity::Busy => "busy",
                        Activity::Idle => "idle",
                    },
                    session.repo.as_deref().unwrap_or("-"),
                    session.role.as_deref().unwrap_or("-"),
                    session.cwd.display(),
                );
            }
            Ok(())
        }
        SignalsCommand::Fire(fire) => {
            let json = fire.json;
            let transition = inbox
                .fire(fire_input(*fire, now)?, now)
                .map_err(|error| error.to_string())?;
            report(&transition, json)
        }
        SignalsCommand::Resolve(resolve) => {
            let event = match &resolve.summary {
                Some(summary) => {
                    let current = snapshot(&inbox, now)?
                        .signals
                        .remove(&resolve.key)
                        .ok_or_else(|| format!("no signal has key `{}`", resolve.key))?;
                    let mut event = current.event;
                    event.summary = summary.clone();
                    Some(event)
                }
                None => None,
            };
            let transition = inbox
                .resolve(&resolve.key, event, now)
                .map_err(|error| error.to_string())?;
            report(&transition, resolve.json)
        }
        SignalsCommand::Ack(ack) => {
            let generation = match ack.generation {
                Some(generation) => generation,
                None => {
                    snapshot(&inbox, now)?
                        .signals
                        .get(&ack.key)
                        .ok_or_else(|| format!("no signal has key `{}`", ack.key))?
                        .generation
                }
            };
            let id = ReminderId {
                key: ack.key.clone(),
                generation,
            };
            let acked = inbox
                .ack(ack.session.as_deref(), std::slice::from_ref(&id), now)
                .map_err(|error| error.to_string())?;
            if acked.is_empty() {
                return Err(format!(
                    "`{id}` has no leased, unacknowledged reminder{}",
                    ack.session
                        .map(|session| format!(" for session {session}"))
                        .unwrap_or_default()
                ));
            }
            println!("acked {id}");
            Ok(())
        }
        SignalsCommand::Hook(hook) => run_hook(&inbox, hook, now),
    }
}

fn snapshot(inbox: &Inbox, now: OffsetDateTime) -> Result<harn_signals::InboxSnapshot, String> {
    inbox.snapshot(now).map_err(|error| error.to_string())
}

fn fire_input(args: SignalsFireArgs, now: OffsetDateTime) -> Result<SignalInput, String> {
    let event = match &args.event {
        Some(path) => read_event(path)?,
        None => {
            let source_url = args.source_url.clone().unwrap_or_default();
            let key = args
                .key
                .clone()
                .unwrap_or_else(|| format!("{}:{source_url}", args.provider));
            TriageEvent::manual(
                ManualEvent {
                    provider: args.provider.clone(),
                    source_kind: args.kind.clone(),
                    source_url,
                    summary: args.summary.clone().unwrap_or_default(),
                    why_it_matters: args.why.clone(),
                    proposed_action: args.action.clone(),
                    urgency: String::new(),
                },
                &key,
            )
            .map_err(|error| error.to_string())?
        }
    };
    let expires_at = args
        .expires_in
        .map(|duration| {
            time::Duration::try_from(duration)
                .map(|duration| now + duration)
                .map_err(|error| format!("--expires-in: {error}"))
        })
        .transpose()?;
    Ok(SignalInput {
        key: args.key,
        event,
        severity: match args.severity {
            SignalSeverityArg::Info => Severity::Info,
            SignalSeverityArg::Warning => Severity::Warning,
            SignalSeverityArg::Urgent => Severity::Urgent,
        },
        routing: RoutingHints {
            repo: args.repo,
            cwd: args.cwd,
            topic: args.topic,
        },
        provenance: Provenance {
            producer: args.producer,
            signature: match args.signature {
                SignalSignatureArg::Local => SignatureStatus::Local,
                SignalSignatureArg::Verified => SignatureStatus::Verified,
                SignalSignatureArg::Unverified => SignatureStatus::Unverified,
            },
        },
        expires_at,
    })
}

fn read_event(path: &Path) -> Result<TriageEvent, String> {
    let text = if path == Path::new("-") {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|error| format!("reading the event from stdin: {error}"))?;
        text
    } else {
        std::fs::read_to_string(path)
            .map_err(|error| format!("reading {}: {error}", path.display()))?
    };
    serde_json::from_str(&text).map_err(|error| format!("not a harn.triage_event.v1: {error}"))
}

fn report(transition: &harn_signals::Transition, json: bool) -> Result<(), String> {
    if json {
        return print_json(transition);
    }
    match transition {
        harn_signals::Transition::Changed { state, generation } => {
            println!("{} (generation {generation})", state.as_str());
        }
        harn_signals::Transition::Unchanged { state, generation } => {
            println!(
                "unchanged: already {} (generation {generation})",
                state.as_str()
            );
        }
        harn_signals::Transition::Unknown => return Err("no signal has that key".into()),
    }
    Ok(())
}

fn print_signals(
    inbox: &Inbox,
    signals: &[Signal],
    now: OffsetDateTime,
    json: bool,
) -> Result<(), String> {
    if json {
        return print_json(&signals);
    }
    for signal in signals {
        let delivery = match &signal.delivery {
            None if signal.is_pending() => match inbox.config().policy.hold(signal, now) {
                Some(hold) => format!("held ({})", hold.as_str()),
                None => "held (no matching session)".to_string(),
            },
            None => "-".to_string(),
            Some(delivery) if delivery.acked_at.is_some() => {
                format!("acked by {}", delivery.session_id)
            }
            Some(delivery) if delivery.lease_active(now) => format!(
                "leased to {} (attempt {})",
                delivery.session_id, signal.attempts
            ),
            Some(delivery) => format!("routed to {}", delivery.session_id),
        };
        println!(
            "{}\t{}#{}\t{}\t{}\t{}",
            signal.key,
            signal.state.as_str(),
            signal.generation,
            signal.severity.as_str(),
            delivery,
            signal.event.summary,
        );
    }
    Ok(())
}

fn print_json<T: serde::Serialize>(data: &T) -> Result<(), String> {
    println!(
        "{}",
        to_string_pretty(&JsonEnvelope::ok(SIGNALS_SCHEMA_VERSION, data))
    );
    Ok(())
}

/// The fields of a hook's stdin payload this adapter reads. Both Claude Code
/// and Codex send `session_id` and `cwd`; other fields are ignored.
#[derive(Deserialize)]
struct HookInput {
    session_id: String,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    session_title: Option<String>,
}

/// Delivery through a harness hook.
///
/// Every reminder is rendered as context the harness frames as a system or
/// developer note, never as a user prompt: an alert must not carry the user's
/// authority. That rules out Codex's `Stop` hook, whose `decision: block`
/// reason Codex turns into a new user prompt, so a Codex session hears an
/// urgent signal at its next prompt or tool call instead.
///
/// Each check-in acks the leases the session took at its previous hook, since
/// running again means the previous output reached the model, and leases at
/// most [`MAX_REMINDERS_PER_DELIVERY`] reminders so the output stays inside
/// the harness's hook budget (10,000 characters for Claude Code, about 2,500
/// tokens for Codex). The rest wait for the next hook.
fn run_hook(inbox: &Inbox, args: SignalsHookArgs, now: OffsetDateTime) -> Result<(), String> {
    let mut stdin = String::new();
    if !std::io::stdin().is_terminal() {
        std::io::stdin()
            .read_to_string(&mut stdin)
            .map_err(|error| format!("reading the hook payload: {error}"))?;
    }
    let input: HookInput = serde_json::from_str(&stdin)
        .map_err(|error| format!("hook payload is not the expected JSON: {error}"))?;
    if args.event == SignalsHookEvent::SessionEnd {
        return inbox
            .end_session(&input.session_id, now)
            .map_err(|error| error.to_string());
    }
    let cwd = input
        .cwd
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let announce = |activity| SessionAnnouncement {
        id: input.session_id.clone(),
        harness: match args.format {
            SignalsHookFormat::ClaudeCode => HarnessKind::ClaudeCode,
            SignalsHookFormat::Codex => HarnessKind::Codex,
        },
        repo: harn_signals::repo_slug(&cwd),
        cwd: cwd.clone(),
        title: input.session_title.clone(),
        role: args
            .role
            .clone()
            .or_else(|| std::env::var(ROLE_ENV).ok().filter(|role| !role.is_empty())),
        activity,
    };
    let check_in = |activity, scope, limit| {
        inbox
            .check_in(announce(activity), scope, Some(limit), now)
            .map_err(|error| error.to_string())
    };

    if args.event == SignalsHookEvent::Stop {
        if args.rewake {
            if args.format != SignalsHookFormat::ClaudeCode {
                return Err("--rewake is a Claude Code asyncRewake hook".into());
            }
            inbox
                .register_session(announce(Activity::Idle), now)
                .map_err(|error| error.to_string())?;
            return rewake(inbox, &input.session_id, args.max_wait);
        }
        if args.format == SignalsHookFormat::Codex {
            // Ack what the last turn rendered and lease nothing: Codex's Stop
            // output cannot carry context without making it a user prompt.
            check_in(Activity::Idle, CheckInScope::UrgentOnly, 0)?;
            println!("{{}}");
            return Ok(());
        }
        // Stopping is where non-urgent reminders wait for the next prompt;
        // only an urgent one keeps the session going.
        let result = check_in(
            Activity::Idle,
            CheckInScope::UrgentOnly,
            MAX_REMINDERS_PER_DELIVERY,
        )?;
        if result.reminders.is_empty() {
            return Ok(());
        }
        inbox
            .register_session(announce(Activity::Busy), now)
            .map_err(|error| error.to_string())?;
        println!("{}", additional_context("Stop", &render(&result)?));
        return Ok(());
    }

    let result = check_in(
        Activity::Busy,
        CheckInScope::All,
        MAX_REMINDERS_PER_DELIVERY,
    )?;
    if result.reminders.is_empty() {
        return Ok(());
    }
    let event_name = match args.event {
        SignalsHookEvent::SessionStart => "SessionStart",
        SignalsHookEvent::UserPromptSubmit => "UserPromptSubmit",
        SignalsHookEvent::PostToolUse => "PostToolUse",
        SignalsHookEvent::Stop | SignalsHookEvent::SessionEnd => unreachable!(),
    };
    println!("{}", additional_context(event_name, &render(&result)?));
    Ok(())
}

fn additional_context(event_name: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": event_name,
            "additionalContext": text,
        }
    })
}

fn render(check_in: &CheckIn) -> Result<String, String> {
    let mut text = render_reminders(&check_in.reminders)?;
    if check_in.waiting > 0 {
        text.push_str(&format!(
            "\n\n{} more signal{} for this session will follow at a later step.",
            check_in.waiting,
            if check_in.waiting == 1 { "" } else { "s" }
        ));
    }
    Ok(text)
}

fn render_reminders(reminders: &[Reminder]) -> Result<String, String> {
    harn_vm::stdlib::signals::render_reminders(reminders).map_err(|error| error.to_string())
}

/// Wait for an urgent reminder while the session idles. Exiting 2 with the
/// reminder on stderr is Claude Code's `asyncRewake` contract: the text is
/// shown to the session as a system reminder and wakes it. Exiting 0 wakes
/// nothing. One watcher runs per session; a second one for the same session
/// exits at once, so repeated stops never stack watchers.
fn rewake(inbox: &Inbox, session_id: &str, max_wait: StdDuration) -> Result<(), String> {
    let Some(_watcher) = claim_watcher(inbox.dir(), session_id)? else {
        return Ok(());
    };
    let started = harn_vm::stdlib::signals::vm_now();
    loop {
        let now = harn_vm::stdlib::signals::vm_now();
        match inbox
            .poll_idle(session_id, Some(MAX_REMINDERS_PER_DELIVERY), now)
            .map_err(|error| error.to_string())?
        {
            IdlePoll::Reminders(reminders) if !reminders.is_empty() => {
                eprintln!("{}", render_reminders(&reminders)?);
                process::exit(REWAKE_EXIT);
            }
            IdlePoll::Reminders(_) => {}
            IdlePoll::Busy | IdlePoll::Gone => return Ok(()),
        }
        if now - started >= max_wait {
            return Ok(());
        }
        std::thread::sleep(REWAKE_POLL);
    }
}

/// Take this session's watcher lock without waiting, or `None` when another
/// watcher holds it. The lock is released when the returned file drops or the
/// process exits.
fn claim_watcher(dir: &Path, session_id: &str) -> Result<Option<std::fs::File>, String> {
    let watchers = dir.join("watchers");
    std::fs::create_dir_all(&watchers)
        .map_err(|error| format!("{}: {error}", watchers.display()))?;
    // Hex keeps any session id a safe file name.
    let name: String = session_id
        .bytes()
        .take(120)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let path = watchers.join(format!("{name}.lock"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(format!("{}: {error}", path.display())),
    }
}
