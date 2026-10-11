use std::path::{Path, PathBuf};

use super::*;

const BASE: &str = "(version 1)\n(deny default)\n(allow process*)\n";

fn seatbelt(profile: &str, path: &str) -> ChildWriteDisposition {
    seatbelt_write_disposition(&format!("{BASE}{profile}"), Path::new(path))
}

#[test]
fn seatbelt_default_deny_is_read_only() {
    assert_eq!(
        seatbelt("(allow file-read* (subpath \"/ro\"))\n", "/ro/f"),
        ChildWriteDisposition::EnforcedReadOnly
    );
}

#[test]
fn seatbelt_last_matching_write_rule_wins() {
    let nested_deny =
        "(allow file-write* (subpath \"/ws\"))\n(deny file-write* (subpath \"/ws/ro\"))\n";
    assert_eq!(
        seatbelt(nested_deny, "/ws/ro/f"),
        ChildWriteDisposition::EnforcedReadOnly
    );
    assert_eq!(
        seatbelt(nested_deny, "/ws/f"),
        ChildWriteDisposition::Writable
    );
    let reallowed = format!("{nested_deny}(allow file-write* (subpath \"/ws/ro/out\"))\n");
    assert_eq!(
        seatbelt(&reallowed, "/ws/ro/out/f"),
        ChildWriteDisposition::Writable
    );
    assert_eq!(
        seatbelt(&reallowed, "/ws/ro/f"),
        ChildWriteDisposition::EnforcedReadOnly
    );
}

#[test]
fn seatbelt_aggregate_and_literal_filters_match() {
    let profile = "(allow file-write* (subpath \"/a\") (subpath \"/b\"))\n\
                   (allow file-write* (literal \"/dev/null\"))\n";
    assert_eq!(seatbelt(profile, "/b/x"), ChildWriteDisposition::Writable);
    assert_eq!(
        seatbelt(profile, "/dev/null"),
        ChildWriteDisposition::Writable
    );
    assert_eq!(
        seatbelt(profile, "/c/x"),
        ChildWriteDisposition::EnforcedReadOnly
    );
}

/// A socket-only create grant still makes an entry, so it counts as a write.
#[test]
fn seatbelt_socket_create_grant_counts_as_writable() {
    let profile = "(allow file-write-create file-write-unlink \
                   (require-all (subpath \"/ro/sock\") (vnode-type SOCKET)))\n";
    assert_eq!(
        seatbelt(profile, "/ro/sock/s"),
        ChildWriteDisposition::Writable
    );
}

/// A rule form this module does not read never reads as read-only.
#[test]
fn seatbelt_unread_forms_are_unknown() {
    for profile in [
        "(allow file-write* (regex #\"^/ro/\"))\n",
        "(allow file*)\n",
        "(allow default)\n",
        "(allow file-write* (subpath \"/ro\")\n",
    ] {
        assert_eq!(
            seatbelt_write_disposition(&format!("{BASE}{profile}"), Path::new("/ro/f")),
            if profile.starts_with("(allow file*)") || profile.starts_with("(allow default)") {
                ChildWriteDisposition::Writable
            } else {
                ChildWriteDisposition::Unknown
            },
            "{profile}"
        );
    }
}

#[test]
fn seatbelt_private_aliases_match_either_spelling() {
    let profile = "(allow file-write* (subpath \"/private/tmp/w\"))\n";
    assert_eq!(
        seatbelt(profile, "/tmp/w/f"),
        ChildWriteDisposition::Writable
    );
    let deny = "(allow file-write* (subpath \"/private/tmp\"))\n(deny file-write* (subpath \"/tmp/ro\"))\n";
    assert_eq!(
        seatbelt(deny, "/private/tmp/ro/f"),
        ChildWriteDisposition::EnforcedReadOnly
    );
}

const WRITE: u64 = 1 << 1;
const TRUNCATE: u64 = 1 << 14;
const READ: u64 = 1 << 2;

fn grants(rules: &[(&str, u64)], handled: u64) -> LandlockWriteGrants {
    LandlockWriteGrants {
        rules: rules
            .iter()
            .map(|(path, access)| (PathBuf::from(path), *access))
            .collect(),
        handled_access: handled,
        write_access: WRITE | TRUNCATE,
    }
}

#[test]
fn landlock_inherits_an_ancestors_write_grant() {
    let set = grants(
        &[("/ws", READ | WRITE), ("/ws/ro", READ)],
        READ | WRITE | TRUNCATE,
    );
    assert_eq!(
        landlock_write_disposition(&set, Path::new("/ws/ro/f")),
        ChildWriteDisposition::Writable
    );
    let disjoint = grants(
        &[("/ws", READ | WRITE), ("/ro", READ)],
        READ | WRITE | TRUNCATE,
    );
    assert_eq!(
        landlock_write_disposition(&disjoint, Path::new("/ro/f")),
        ChildWriteDisposition::EnforcedReadOnly
    );
}

/// An ABI that does not handle a write right leaves it unrestricted.
#[test]
fn landlock_unhandled_write_right_is_writable() {
    let set = grants(&[("/ro", READ)], READ | WRITE);
    assert_eq!(
        landlock_write_disposition(&set, Path::new("/ro/f")),
        ChildWriteDisposition::Writable
    );
}

#[test]
fn a_dot_dot_path_that_does_not_resolve_is_unknown() {
    assert_eq!(resolved_target(Path::new("relative/f")), None);
    assert_eq!(
        resolved_target(Path::new("/definitely-missing-harn-root/../x")),
        None
    );
}
