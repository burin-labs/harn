//! `harn signals`: the ambient-signal inbox from the command line, and the
//! hook adapter agent harnesses run at their safe points.

use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::time::Duration as StdDuration;

use harn_signals::{
    Activity, CheckIn, CheckInScope, HarnessKind, IdlePoll, Inbox, ManualEvent, Provenance,
    Reminder, ReminderId, RoutingHints, SessionAnnouncement, Severity, Signal, SignalInput,
    SignatureStatus, TriageEvent,
};

/// Polls a reminder may sit pending with no lease out before the watcher
/// wakes for it even though the `Stop` hook that started the watcher saw it.
/// That hook leases what was pending when it ran; one routed after it ran but
/// before the first poll is still unleased a poll or two later.
const BASELINE_GRACE_POLLS: u32 = 2;
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
/// The only text a rewake prints. Claude Code shows a rewake hook's stderr as
/// a user-role turn, so no signal field may appear in it; the alert itself
/// arrives through the `Stop` hook's `additionalContext` when the woken turn
/// ends.
const REWAKE_NOTICE: &str =
    "A signal is pending; it arrives as a system reminder when this turn ends. No reply is needed until then.";

pub(crate) fn run(args: SignalsArgs) -> Result<(), String> {
    let is_hook = matches!(args.command, SignalsCommand::Hook(_));
    let result = run_command(args);
    if is_hook {
        // The CLI's usual error exit is 2, which Claude Code reads as "block
        // the stop" or "show Claude this". A broken hook must never steer a
        // session, so it reports and exits 1, which harnesses treat as a
        // non-blocking hook error.
        if let Err(error) = result {
            cli_diagnostic!("harn signals hook: {error}");
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
                    terminal_safe(&session.id),
                    session.harness,
                    match session.activity {
                        Activity::Busy => "busy",
                        Activity::Idle => "idle",
                    },
                    terminal_safe(session.repo.as_deref().unwrap_or("-")),
                    terminal_safe(session.role.as_deref().unwrap_or("-")),
                    terminal_safe(&session.cwd.display().to_string()),
                );
            }
            Ok(())
        }
        SignalsCommand::Fire(fire) => {
            let json = fire.json;
            let event_id = fire.event_id.clone();
            let session = fire.session.clone();
            let input = fire_input(*fire, now)?;
            let transition = match (session, event_id) {
                (Some(session), Some(event_id)) => inbox.capture(&session, &event_id, input, now),
                (None, None) => inbox.fire(input, now),
                _ => return Err("--event-id and --session must be supplied together".into()),
            }
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
                .resolve(&resolve.key, event, signature(resolve.signature), now)
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
            signature: signature(args.signature),
        },
        expires_at,
    })
}

fn signature(arg: SignalSignatureArg) -> SignatureStatus {
    match arg {
        SignalSignatureArg::Local => SignatureStatus::Local,
        SignalSignatureArg::Verified => SignatureStatus::Verified,
        SignalSignatureArg::Unverified => SignatureStatus::Unverified,
    }
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
            Some(delivery) if delivery.lease.is_some() => format!(
                "lease lapsed for {} (attempt {})",
                delivery.session_id, signal.attempts
            ),
            Some(delivery) => format!("routed to {}", delivery.session_id),
        };
        println!(
            "{}\t{}#{}\t{}\t{}\t{}",
            terminal_safe(&signal.key),
            signal.state.as_str(),
            signal.generation,
            signal.severity.as_str(),
            terminal_safe(&delivery),
            terminal_safe(&signal.event.summary),
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
/// A hook leases at most [`MAX_REMINDERS_PER_DELIVERY`] reminders, so the
/// output stays inside the harness's hook budget (10,000 characters for Claude
/// Code, about 2,500 tokens for Codex), and acks exactly those leases by token
/// once its output is flushed. A hook killed before that leaves its leases to
/// lapse and be leased again; a hook running beside it acks nothing of its.
/// The rest wait for the next hook.
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
        inbox
            .end_session(&input.session_id, now)
            .map_err(|error| error.to_string())?;
        // A running watcher sees the session gone at its next poll and removes
        // its own lock; this clears one a killed watcher left behind.
        drop(
            inbox
                .claim_watcher(&input.session_id)
                .map_err(|error| error.to_string())?,
        );
        return Ok(());
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
            // Lease nothing: Codex's Stop output cannot carry context without
            // making it a user prompt.
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
        return deliver(inbox, &input.session_id, "Stop", &result);
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
    deliver(inbox, &input.session_id, event_name, &result)
}

/// Print the hook output, then ack the leases it carried. The harness reads
/// stdout only after the hook exits, so a flushed write is as far as this
/// process can see the reminder travel.
fn deliver(
    inbox: &Inbox,
    session_id: &str,
    event_name: &str,
    result: &CheckIn,
) -> Result<(), String> {
    let output = additional_context(event_name, &render(result)?).to_string();
    let tokens: Vec<String> = result
        .reminders
        .iter()
        .map(|reminder| reminder.lease_token.clone())
        .collect();
    finish_delivery(
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
        &output,
        || {
            inbox
                .ack_leases(session_id, &tokens, harn_vm::stdlib::signals::vm_now())
                .map(drop)
                .map_err(|error| error.to_string())
        },
    )
}

/// Once the output is flushed, a failed ack must not fail the hook: a
/// harness discards the output of a hook that exits non-zero, which would
/// drop a reminder already handed over. The unacked lease lapses and is
/// leased again, so the worst case is a repeat.
fn finish_delivery(
    out: &mut impl Write,
    err: &mut impl Write,
    output: &str,
    ack: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    writeln!(out, "{output}")
        .and_then(|()| out.flush())
        .map_err(|error| format!("writing the hook output: {error}"))?;
    if let Err(error) = ack() {
        let _ = writeln!(
            err,
            "harn signals hook: the delivered reminders were not acked and will be sent again: {error}"
        );
    }
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

/// Wait for an urgent reminder while the session idles. Exiting 2 is Claude
/// Code's `asyncRewake` contract: it wakes the session and shows stderr as a
/// user-role `Stop hook feedback` turn. The watcher therefore prints only
/// [`REWAKE_NOTICE`] and leases nothing; the woken turn's `Stop` hook leases
/// the reminder and delivers it as `additionalContext`. Exiting 0 wakes
/// nothing. One watcher runs per session; a second one for the same session
/// exits at once, so repeated stops never stack watchers.
fn rewake(inbox: &Inbox, session_id: &str, max_wait: StdDuration) -> Result<(), String> {
    let Some(watcher) = inbox
        .claim_watcher(session_id)
        .map_err(|error| error.to_string())?
    else {
        return Ok(());
    };
    let started = harn_vm::stdlib::signals::vm_now();
    // The `Stop` hook that started this watcher runs beside it and leases
    // what was pending, so the first poll's ids are given that hook's chance
    // before they wake the session; waking at once would announce them twice.
    let mut baseline: Option<Vec<ReminderId>> = None;
    let mut polls: u32 = 0;
    loop {
        let now = harn_vm::stdlib::signals::vm_now();
        let poll = inbox
            .poll_idle(session_id, now)
            .map_err(|error| error.to_string())?;
        polls += 1;
        match poll {
            IdlePoll::Pending(pending) => {
                let baseline = baseline.get_or_insert_with(|| pending.clone());
                let wake = pending
                    .iter()
                    .any(|id| !baseline.contains(id) || polls > BASELINE_GRACE_POLLS);
                if wake {
                    // An unrecorded wake would not count against the cap, so
                    // stop without waking; the hook error exit is not a wake.
                    inbox
                        .record_wake(session_id, &pending, now)
                        .map_err(|error| format!("could not record the wake: {error}"))?;
                    eprintln!("{REWAKE_NOTICE}");
                    // `process::exit` skips destructors; release the lock first.
                    drop(watcher);
                    process::exit(REWAKE_EXIT);
                }
            }
            IdlePoll::Busy | IdlePoll::Gone => return Ok(()),
        }
        if now - started >= max_wait {
            return Ok(());
        }
        std::thread::sleep(REWAKE_POLL);
    }
}

/// Inbox text comes from monitors and webhooks; never let it drive the
/// terminal.
fn terminal_safe(text: &str) -> String {
    harn_signals::display_safe(text)
}

#[cfg(test)]
mod tests {
    use super::finish_delivery;

    #[test]
    fn a_failed_ack_after_a_flushed_delivery_still_succeeds() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let result = finish_delivery(&mut out, &mut err, "{\"reminder\":1}", || {
            Err("inbox is locked".into())
        });
        assert_eq!(result, Ok(()));
        assert_eq!(String::from_utf8(out).unwrap(), "{\"reminder\":1}\n");
        assert!(String::from_utf8(err).unwrap().contains("inbox is locked"));
    }
}
