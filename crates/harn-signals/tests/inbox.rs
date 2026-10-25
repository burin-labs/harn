use std::path::PathBuf;
use std::sync::{Arc, Barrier};

use harn_signals::{
    Activity, CheckInScope, HarnessKind, Inbox, ManualEvent, Provenance, Reminder, ReminderId,
    RoutingHints, SessionAnnouncement, Severity, SignalInput, SignalState, Transition, TriageEvent,
};
use time::{Duration, OffsetDateTime};

const ALL: CheckInScope = CheckInScope::All;

/// Check in with no limit and return what was leased.
fn take(
    inbox: &Inbox,
    session: SessionAnnouncement,
    scope: CheckInScope,
    now: OffsetDateTime,
) -> Vec<Reminder> {
    inbox.check_in(session, scope, None, now).unwrap().reminders
}

fn t0() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap()
}

fn event(key: &str, summary: &str) -> TriageEvent {
    TriageEvent::manual(
        ManualEvent {
            provider: "github".into(),
            source_kind: "ci.default_branch".into(),
            source_url: format!("https://example.test/{key}"),
            summary: summary.into(),
            why_it_matters: "Merges are blocked.".into(),
            proposed_action: "Look at the failing job.".into(),
            urgency: String::new(),
        },
        key,
    )
    .unwrap()
}

fn input(key: &str, repo: Option<&str>, severity: Severity) -> SignalInput {
    SignalInput {
        key: Some(key.into()),
        event: event(key, "CI is red on main"),
        severity,
        routing: RoutingHints {
            repo: repo.map(Into::into),
            ..Default::default()
        },
        provenance: Provenance::local("test"),
        expires_at: None,
    }
}

fn session(id: &str, repo: Option<&str>, role: Option<&str>) -> SessionAnnouncement {
    SessionAnnouncement {
        id: id.into(),
        harness: HarnessKind::ClaudeCode,
        cwd: PathBuf::from(format!("/work/{id}")),
        repo: repo.map(Into::into),
        title: None,
        role: role.map(Into::into),
        activity: Activity::Busy,
    }
}

fn keys(reminders: &[harn_signals::Reminder]) -> Vec<(String, SignalState)> {
    reminders
        .iter()
        .map(|reminder| (reminder.key.clone(), reminder.state))
        .collect()
}

#[test]
fn a_condition_that_stays_red_produces_one_reminder_per_state_change() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    let a = session("a", Some("acme/api"), None);
    take(&inbox, a.clone(), ALL, t0());

    let mut delivered = Vec::new();
    // A monitor polling once a minute for an hour while CI stays red.
    for minute in 0..60 {
        let now = t0() + Duration::minutes(minute);
        inbox
            .fire(
                input("ci:acme/api", Some("acme/api"), Severity::Warning),
                now,
            )
            .unwrap();
        delivered.extend(take(&inbox, a.clone(), ALL, now));
    }
    let later = t0() + Duration::minutes(61);
    for _ in 0..3 {
        inbox.resolve("ci:acme/api", None, later).unwrap();
        delivered.extend(take(&inbox, a.clone(), ALL, later));
    }

    assert_eq!(
        keys(&delivered),
        vec![
            ("ci:acme/api".to_string(), SignalState::Fired),
            ("ci:acme/api".to_string(), SignalState::Resolved),
        ]
    );
    assert_eq!(
        delivered.iter().map(|r| r.generation).collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn concurrent_sessions_never_both_receive_one_generation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    Inbox::open(&path)
        .unwrap()
        .fire(
            input("ci:acme/api", Some("acme/api"), Severity::Warning),
            t0(),
        )
        .unwrap();

    let sessions = 12;
    let barrier = Arc::new(Barrier::new(sessions));
    let handles: Vec<_> = (0..sessions)
        .map(|index| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let inbox = Inbox::open(&path).unwrap();
                let me = session(&format!("s{index}"), Some("acme/api"), None);
                barrier.wait();
                let mut got = 0;
                for round in 0..20 {
                    got += take(&inbox, me.clone(), ALL, t0() + Duration::seconds(round)).len();
                }
                got
            })
        })
        .collect();
    let total: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
    assert_eq!(total, 1, "one generation must reach exactly one session");
}

#[test]
fn routing_prefers_repo_then_fallback_role_and_holds_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    inbox
        .fire(
            input("ci:acme/web", Some("acme/web"), Severity::Warning),
            t0(),
        )
        .unwrap();

    // Nobody matches and nobody holds the fallback role: held, not dropped.
    let other = session("other", Some("acme/api"), None);
    assert!(take(&inbox, other.clone(), ALL, t0()).is_empty());

    let control = session("control", None, Some("control"));
    let got = take(&inbox, control.clone(), ALL, t0());
    assert_eq!(keys(&got), vec![("ci:acme/web".into(), SignalState::Fired)]);

    // A matching session outranks the fallback for a new signal.
    let web = session("web", Some("ACME/web"), None);
    take(&inbox, web.clone(), ALL, t0());
    inbox
        .fire(
            input("prs:acme/web", Some("acme/web"), Severity::Info),
            t0(),
        )
        .unwrap();
    assert!(take(&inbox, control.clone(), ALL, t0()).is_empty());
    assert_eq!(take(&inbox, web, ALL, t0()).len(), 1);

    // The recovery goes to the session that heard the failure.
    inbox.resolve("ci:acme/web", None, t0()).unwrap();
    let got = take(&inbox, control, ALL, t0());
    assert_eq!(
        keys(&got),
        vec![("ci:acme/web".into(), SignalState::Resolved)]
    );
}

#[test]
fn a_recovery_nobody_heard_the_failure_for_is_not_delivered() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    inbox
        .fire(
            input("ci:acme/api", Some("acme/api"), Severity::Warning),
            t0(),
        )
        .unwrap();
    assert_eq!(
        inbox.resolve("ci:acme/api", None, t0()).unwrap(),
        Transition::Changed {
            state: SignalState::Resolved,
            generation: 2
        }
    );
    let a = session("a", Some("acme/api"), None);
    assert!(take(&inbox, a, ALL, t0()).is_empty());
}

#[test]
fn a_stopping_session_leases_only_urgent_reminders() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    let a = session("a", Some("acme/api"), None);
    inbox
        .fire(
            input("prs:acme/api", Some("acme/api"), Severity::Warning),
            t0(),
        )
        .unwrap();
    inbox
        .fire(
            input("alert:acme/api", Some("acme/api"), Severity::Urgent),
            t0(),
        )
        .unwrap();
    let urgent = take(&inbox, a.clone(), CheckInScope::UrgentOnly, t0());
    assert_eq!(
        keys(&urgent),
        vec![("alert:acme/api".into(), SignalState::Fired)]
    );
    let queued = take(&inbox, a, ALL, t0());
    assert_eq!(
        keys(&queued),
        vec![("prs:acme/api".into(), SignalState::Fired)]
    );
}

#[test]
fn a_stale_sessions_undelivered_reminder_moves_to_a_live_one() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    inbox
        .register_session(session("gone", Some("acme/api"), None), t0())
        .unwrap();
    inbox
        .fire(
            input("ci:acme/api", Some("acme/api"), Severity::Warning),
            t0(),
        )
        .unwrap();
    let later = t0() + inbox.config().stale_after + Duration::seconds(1);
    let got = take(&inbox, session("fresh", Some("acme/api"), None), ALL, later);
    assert_eq!(got.len(), 1);
    assert!(!inbox.snapshot(later).unwrap().sessions.contains_key("gone"));
}

#[test]
fn expiry_follows_up_only_when_the_fired_reminder_was_heard() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    let a = session("a", Some("acme/api"), None);
    take(&inbox, a.clone(), ALL, t0());
    let mut heard = input("alert:heard", Some("acme/api"), Severity::Warning);
    heard.expires_at = Some(t0() + Duration::minutes(5));
    inbox.fire(heard, t0()).unwrap();
    assert_eq!(take(&inbox, a.clone(), ALL, t0()).len(), 1);

    let mut unheard = input("alert:unheard", Some("acme/other"), Severity::Warning);
    unheard.expires_at = Some(t0() + Duration::minutes(5));
    inbox.fire(unheard, t0()).unwrap();

    let later = t0() + Duration::minutes(6);
    let got = take(&inbox, a, ALL, later);
    assert_eq!(
        keys(&got),
        vec![("alert:heard".into(), SignalState::Expired)]
    );
    let snapshot = inbox.snapshot(later).unwrap();
    assert_eq!(
        snapshot.signals["alert:unheard"].state,
        SignalState::Expired
    );
    assert!(!snapshot.signals["alert:unheard"].is_pending());
}

#[test]
fn the_inbox_refuses_a_document_from_another_schema() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("inbox.json"),
        r#"{"schema":"harn.signals.inbox.v0","signals":{},"sessions":{}}"#,
    )
    .unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    let error = inbox.snapshot(t0()).unwrap_err().to_string();
    assert!(error.contains("harn.signals.inbox.v1"), "{error}");
}

#[test]
fn an_unacked_lease_is_redelivered_after_it_lapses_and_the_next_check_in_acks_it() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    let lease_for = inbox.config().lease_for;
    let a = session("a", Some("acme/api"), None);
    inbox
        .fire(
            input("ci:acme/api", Some("acme/api"), Severity::Warning),
            t0(),
        )
        .unwrap();

    let first = take(&inbox, a.clone(), ALL, t0());
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].attempt, 1);

    // The next check-in acks the lease instead of leasing it twice.
    assert!(take(&inbox, a.clone(), ALL, t0() + Duration::seconds(1)).is_empty());
    let after_lapse = t0() + lease_for + Duration::seconds(2);
    // Acked, so a lapse redelivers nothing.
    assert!(take(&inbox, a.clone(), ALL, after_lapse).is_empty());

    // Next generation: leased, then the harness crashed before the reminder
    // reached the model, so no check-in follows within the lease.
    inbox.resolve("ci:acme/api", None, after_lapse).unwrap();
    let resolved = take(&inbox, a.clone(), ALL, after_lapse);
    assert_eq!(
        keys(&resolved),
        vec![("ci:acme/api".into(), SignalState::Resolved)]
    );
    let lapsed = after_lapse + lease_for + Duration::seconds(1);
    let again = inbox.check_in(a.clone(), ALL, None, lapsed).unwrap();
    assert!(
        again.acked.is_empty(),
        "a lapsed lease is not evidence of delivery"
    );
    assert_eq!(keys(&again.reminders), keys(&resolved));
    assert_eq!(again.reminders[0].attempt, 2);
    assert_eq!(again.reminders[0].id(), resolved[0].id());

    // The next check-in acks the redelivery; nothing comes a third time.
    let settle = inbox
        .check_in(a.clone(), ALL, None, lapsed + Duration::seconds(30))
        .unwrap();
    assert_eq!(settle.acked, vec![resolved[0].id()]);
    assert!(settle.reminders.is_empty());
    assert!(take(&inbox, a, ALL, lapsed + lease_for * 3).is_empty());
}

#[test]
fn a_lease_held_by_a_session_that_ends_goes_to_another_session() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    inbox
        .fire(
            input("ci:acme/api", Some("acme/api"), Severity::Warning),
            t0(),
        )
        .unwrap();
    let a = session("a", Some("acme/api"), None);
    assert_eq!(take(&inbox, a, ALL, t0()).len(), 1);
    inbox.end_session("a", t0()).unwrap();
    let b = take(&inbox, session("b", Some("acme/api"), None), ALL, t0());
    assert_eq!(keys(&b), vec![("ci:acme/api".into(), SignalState::Fired)]);
    assert_eq!(b[0].attempt, 2);
}

#[test]
fn explicit_ack_settles_only_the_named_generation_for_its_recipient() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    inbox
        .fire(
            input("ci:acme/api", Some("acme/api"), Severity::Warning),
            t0(),
        )
        .unwrap();
    let id = ReminderId {
        key: "ci:acme/api".into(),
        generation: 1,
    };
    // Not leased yet: nothing to ack.
    assert!(inbox.ack(None, &[id.clone()], t0()).unwrap().is_empty());
    let a = session("a", Some("acme/api"), None);
    take(&inbox, a.clone(), ALL, t0());
    let stale = ReminderId {
        generation: 7,
        ..id.clone()
    };
    assert!(inbox.ack(Some("a"), &[stale], t0()).unwrap().is_empty());
    assert!(inbox
        .ack(Some("b"), &[id.clone()], t0())
        .unwrap()
        .is_empty());
    assert_eq!(inbox.ack(Some("a"), &[id.clone()], t0()).unwrap(), vec![id]);
    let later = t0() + inbox.config().lease_for * 2;
    assert!(take(&inbox, a, ALL, later).is_empty());
}

#[test]
fn a_limit_leases_the_most_severe_first_and_counts_the_rest_as_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    for (key, severity) in [
        ("a:info", Severity::Info),
        ("b:urgent", Severity::Urgent),
        ("c:warning", Severity::Warning),
    ] {
        inbox
            .fire(input(key, Some("acme/api"), severity), t0())
            .unwrap();
    }
    let a = session("a", Some("acme/api"), None);
    let first = inbox.check_in(a.clone(), ALL, Some(2), t0()).unwrap();
    assert_eq!(
        first
            .reminders
            .iter()
            .map(|r| r.key.as_str())
            .collect::<Vec<_>>(),
        vec!["b:urgent", "c:warning"]
    );
    assert_eq!(first.waiting, 1);
    let second = inbox
        .check_in(a, ALL, Some(2), t0() + Duration::seconds(1))
        .unwrap();
    assert_eq!(second.acked.len(), 2);
    assert_eq!(
        keys(&second.reminders),
        vec![("a:info".into(), SignalState::Fired)]
    );
    assert_eq!(second.waiting, 0);
}

#[test]
fn the_inbox_policy_holds_muted_low_and_quiet_hours_signals_for_every_consumer() {
    let dir = tempfile::tempdir().unwrap();
    // t0 is 2027-01-15T08:00:00Z, which is 01:00 at UTC-07:00.
    std::fs::write(
        dir.path().join("config.toml"),
        r#"
min_severity = "warning"
muted = ["github.open_prs:*", "exact:key"]

[quiet_hours]
start = "22:00"
end = "07:00"
utc_offset = "-07:00"
"#,
    )
    .unwrap();
    let inbox = Inbox::open(dir.path()).unwrap();
    for (key, severity) in [
        ("github.open_prs:acme/api", Severity::Urgent),
        ("exact:key", Severity::Urgent),
        ("exact:key2", Severity::Urgent),
        ("low:info", Severity::Info),
        ("ci:warning", Severity::Warning),
    ] {
        inbox
            .fire(input(key, Some("acme/api"), severity), t0())
            .unwrap();
    }
    let a = session("a", Some("acme/api"), None);
    // Quiet hours: only urgent and unmuted.
    let night = take(&inbox, a.clone(), ALL, t0());
    assert_eq!(
        keys(&night),
        vec![("exact:key2".into(), SignalState::Fired)]
    );
    let snapshot = inbox.snapshot(t0()).unwrap();
    let policy = &inbox.config().policy;
    let hold = |key: &str| {
        policy
            .hold(&snapshot.signals[key], t0())
            .map(|h| h.as_str())
    };
    assert_eq!(hold("github.open_prs:acme/api"), Some("muted"));
    assert_eq!(hold("low:info"), Some("below minimum severity"));
    assert_eq!(hold("ci:warning"), Some("quiet hours"));

    // The next check-in acks the night's lease.
    assert!(take(&inbox, a.clone(), ALL, t0() + Duration::seconds(1)).is_empty());
    // 07:30 local: quiet hours are over, the warning routes; muted and info
    // stay held.
    let morning = t0() + Duration::minutes(390);
    let day = take(&inbox, a, ALL, morning);
    assert_eq!(keys(&day), vec![("ci:warning".into(), SignalState::Fired)]);
}

#[test]
fn a_config_with_an_unknown_key_or_bad_clock_is_refused() {
    assert!(harn_signals::Config::parse("mute = []").is_err());
    let error = harn_signals::Config::parse("[quiet_hours]\nstart = \"25:00\"\nend = \"07:00\"")
        .unwrap_err();
    assert!(error.contains("quiet_hours.start"), "{error}");
}
