//! Which GitHub CLI invocations only read.
//!
//! This is also the only gate on which gh invocations receive a command-bound
//! credential grant, so it is an allowlist: a read names a known subcommand and
//! every flag is one that subcommand is known to read with. Arguments are parsed
//! the way gh parses them. Short flags can share an argument (`-cw`), and a value
//! can follow its letter, with or without `=`. A flag this table does not know,
//! including one a later gh adds, makes the invocation a non-read. The flag
//! lists come from `gh <group> <verb> --help` at gh 2.103.3.

/// One flag of a gh subcommand.
#[derive(Clone, Copy)]
struct GhFlag {
    short: Option<char>,
    long: &'static str,
    takes_value: bool,
}

const fn switch(long: &'static str) -> GhFlag {
    GhFlag {
        short: None,
        long,
        takes_value: false,
    }
}

const fn short_switch(short: char, long: &'static str) -> GhFlag {
    GhFlag {
        short: Some(short),
        long,
        takes_value: false,
    }
}

const fn value(long: &'static str) -> GhFlag {
    GhFlag {
        short: None,
        long,
        takes_value: true,
    }
}

const fn short_value(short: char, long: &'static str) -> GhFlag {
    GhFlag {
        short: Some(short),
        long,
        takes_value: true,
    }
}

// The output flags parse as reads so that the grant can refuse them by name.
const JQ: GhFlag = short_value('q', "jq");
const JSON: GhFlag = value("json");
const TEMPLATE: GhFlag = short_value('t', "template");

const API: &[GhFlag] = &[
    short_switch('i', "include"),
    switch("paginate"),
    switch("silent"),
    switch("slurp"),
    value("cache"),
    short_value('H', "header"),
    short_value('X', "method"),
    short_value('p', "preview"),
    JQ,
    TEMPLATE,
];
const GIST_LIST: &[GhFlag] = &[
    value("filter"),
    switch("include-content"),
    short_value('L', "limit"),
    switch("public"),
    switch("secret"),
];
const GIST_VIEW: &[GhFlag] = &[
    short_value('f', "filename"),
    switch("files"),
    short_switch('r', "raw"),
];
const ISSUE_LIST: &[GhFlag] = &[
    value("app"),
    short_value('a', "assignee"),
    short_value('A', "author"),
    JQ,
    JSON,
    short_value('l', "label"),
    short_value('L', "limit"),
    value("mention"),
    short_value('m', "milestone"),
    short_value('S', "search"),
    short_value('s', "state"),
    TEMPLATE,
    value("type"),
];
const OUTPUT_ONLY: &[GhFlag] = &[JQ, JSON, TEMPLATE];
const COMMENTS_VIEW: &[GhFlag] = &[short_switch('c', "comments"), JQ, JSON, TEMPLATE];
const LABEL_LIST: &[GhFlag] = &[
    JQ,
    JSON,
    short_value('L', "limit"),
    value("order"),
    short_value('S', "search"),
    value("sort"),
    TEMPLATE,
];
const PR_CHECKS: &[GhFlag] = &[
    switch("fail-fast"),
    short_value('i', "interval"),
    JQ,
    JSON,
    switch("required"),
    TEMPLATE,
    switch("watch"),
];
const PR_DIFF: &[GhFlag] = &[
    value("color"),
    short_value('e', "exclude"),
    switch("name-only"),
    switch("patch"),
];
const PR_LIST: &[GhFlag] = &[
    value("app"),
    short_value('a', "assignee"),
    short_value('A', "author"),
    short_value('B', "base"),
    short_switch('d', "draft"),
    short_value('H', "head"),
    JQ,
    JSON,
    short_value('l', "label"),
    short_value('L', "limit"),
    short_value('S', "search"),
    short_value('s', "state"),
    TEMPLATE,
];
const PR_STATUS: &[GhFlag] = &[short_switch('c', "conflict-status"), JQ, JSON, TEMPLATE];
const REPO_LIST: &[GhFlag] = &[
    switch("archived"),
    switch("fork"),
    JQ,
    JSON,
    short_value('l', "language"),
    short_value('L', "limit"),
    switch("no-archived"),
    switch("source"),
    TEMPLATE,
    value("topic"),
    value("visibility"),
];
const REPO_VIEW: &[GhFlag] = &[short_value('b', "branch"), JQ, JSON, TEMPLATE];
const RUN_LIST: &[GhFlag] = &[
    short_switch('a', "all"),
    short_value('b', "branch"),
    short_value('c', "commit"),
    value("created"),
    short_value('e', "event"),
    JQ,
    JSON,
    short_value('L', "limit"),
    short_value('s', "status"),
    TEMPLATE,
    short_value('u', "user"),
    // On this subcommand `-w` names a workflow rather than opening a browser.
    short_value('w', "workflow"),
];
const RUN_VIEW: &[GhFlag] = &[
    short_value('a', "attempt"),
    switch("exit-status"),
    short_value('j', "job"),
    JQ,
    JSON,
    switch("log"),
    switch("log-failed"),
    TEMPLATE,
    short_switch('v', "verbose"),
];
const WORKFLOW_LIST: &[GhFlag] = &[
    short_switch('a', "all"),
    JQ,
    JSON,
    short_value('L', "limit"),
    TEMPLATE,
];
const WORKFLOW_VIEW: &[GhFlag] = &[short_value('r', "ref"), short_switch('y', "yaml")];

/// The flags of a subcommand that only reads. `release` is absent on purpose:
/// every release subcommand is treated as an effect.
fn read_subcommand_flags(group: &str, verb: &str) -> Option<&'static [GhFlag]> {
    Some(match (group, verb) {
        ("gist", "list") => GIST_LIST,
        ("gist", "view") => GIST_VIEW,
        ("issue", "list") => ISSUE_LIST,
        ("issue", "status") => OUTPUT_ONLY,
        ("issue" | "pr", "view") => COMMENTS_VIEW,
        ("label", "list") => LABEL_LIST,
        ("pr", "checks") => PR_CHECKS,
        ("pr", "diff") => PR_DIFF,
        ("pr", "list") => PR_LIST,
        ("pr", "status") => PR_STATUS,
        ("repo", "list") => REPO_LIST,
        ("repo", "view") => REPO_VIEW,
        ("run", "list") => RUN_LIST,
        ("run", "view") => RUN_VIEW,
        ("workflow", "list") => WORKFLOW_LIST,
        ("workflow", "view") => WORKFLOW_VIEW,
        _ => return None,
    })
}

/// `gh pr list` and `gh api repos/o/r` read; `gh pr merge`, `gh issue comment`,
/// `gh pr view -cw`, and `gh api -X POST ...` do not. Global `-R`/`--repo`/
/// `--hostname` flags are skipped before the subcommand is read.
///
/// Traced with `GIT_TRACE`, the reads run git only for `config`, `remote -v`,
/// `rev-parse` and `symbolic-ref`, none of which runs a filter driver, hook or
/// external diff.
pub(crate) fn gh_invocation_is_read_only(args: &[String]) -> bool {
    let mut words = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-R" | "--repo" | "--hostname" => {
                iter.next();
            }
            flag if flag.starts_with("--repo=")
                || flag.starts_with("--hostname=")
                || (flag.starts_with("-R") && flag.len() > 2) => {}
            _ => words.push(arg.clone()),
        }
    }
    match words.as_slice() {
        [api, rest @ ..] if api == "api" => {
            positionals_when_read(rest, API, api_value_reads) == Some(1)
        }
        [status] if status == "status" => true,
        [group, verb, rest @ ..] => read_subcommand_flags(group, verb)
            .is_some_and(|flags| positionals_when_read(rest, flags, |_, _| true).is_some()),
        _ => false,
    }
}

/// How many positional arguments `args` holds, or `None` when any flag is not
/// known to read. A bare `--` ends the flags, and the grant refuses it by name.
fn positionals_when_read(
    args: &[String],
    flags: &[GhFlag],
    value_reads: fn(&str, &str) -> bool,
) -> Option<usize> {
    let mut positionals = 0;
    let mut words = args.iter();
    while let Some(arg) = words.next() {
        if arg == "--" {
            return Some(positionals + words.len());
        }
        let reads = if let Some(long) = arg.strip_prefix("--") {
            long_flag_reads(long, &mut words, flags, value_reads)
        } else if let Some(cluster) = arg.strip_prefix('-') {
            short_flags_read(cluster, &mut words, flags, value_reads)
        } else {
            positionals += 1;
            true
        };
        if !reads {
            return None;
        }
    }
    Some(positionals)
}

/// Whether `--name` or `--name=value` is a known flag that keeps this a read.
fn long_flag_reads(
    long: &str,
    words: &mut std::slice::Iter<'_, String>,
    flags: &[GhFlag],
    value_reads: fn(&str, &str) -> bool,
) -> bool {
    let (name, inline) = match long.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (long, None),
    };
    let Some(flag) = flags.iter().find(|flag| flag.long == name) else {
        return false;
    };
    if !flag.takes_value {
        return inline.is_none();
    }
    inline
        .or_else(|| words.next().map(String::as_str))
        .is_some_and(|value| value_reads(flag.long, value))
}

/// Whether one argument of short flags keeps this a read. The first
/// value-taking letter ends the flags: the rest of the argument, or else the
/// next one, is its value. An empty cluster is a lone `-`, which is not a flag.
fn short_flags_read(
    cluster: &str,
    words: &mut std::slice::Iter<'_, String>,
    flags: &[GhFlag],
    value_reads: fn(&str, &str) -> bool,
) -> bool {
    let mut letters = cluster.chars();
    while let Some(letter) = letters.next() {
        let Some(flag) = flags.iter().find(|flag| flag.short == Some(letter)) else {
            return false;
        };
        if !flag.takes_value {
            continue;
        }
        let rest = letters.as_str();
        let rest = rest.strip_prefix('=').unwrap_or(rest);
        return Some(rest)
            .filter(|rest| !rest.is_empty())
            .or_else(|| words.next().map(String::as_str))
            .is_some_and(|value| value_reads(flag.long, value));
    }
    !cluster.is_empty()
}

/// Whether a `gh api` flag's value keeps the call a GET without a body. A header
/// can ask a server for a different method, so only content negotiation passes.
fn api_value_reads(name: &str, value: &str) -> bool {
    match name {
        "method" => value.eq_ignore_ascii_case("GET"),
        "header" => value.split_once(':').is_some_and(|(header, _)| {
            let header = header.trim();
            header.eq_ignore_ascii_case("Accept")
                || header.eq_ignore_ascii_case("X-GitHub-Api-Version")
        }),
        _ => true,
    }
}
