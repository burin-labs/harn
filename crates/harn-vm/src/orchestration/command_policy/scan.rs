use super::*;

pub(super) fn scan_command_risk_scan_json(
    ctx: &JsonValue,
    policy: Option<&CommandPolicy>,
) -> JsonValue {
    let command_text = command_text(ctx);
    let lower = command_text.to_ascii_lowercase();
    let mut labels = BTreeSet::new();
    let mut rationale = Vec::new();

    // Never-approvable command floor. A catastrophic classification is a
    // hard-deny that is enforced before any consent/approval routing (see
    // `run_command_policy_preflight_with_ctx`), so it is surfaced both as a
    // distinct `catastrophic` label and as a `catastrophic_reason` string the
    // preflight reads to block the command with burin's verbatim rationale.
    // The floor is fed `floor_command_text` (argv boundaries preserved via
    // shell-quoting), NOT the lossy `command_text` space-join — otherwise the
    // canonical agent shape `argv:["sh","-c","<script>"]` would evade every
    // token-based rule. See `floor_command_text`.
    let catastrophe =
        super::catastrophic::reason(&floor_command_text(ctx), &scan_workspace_roots(ctx));
    if catastrophe.is_some() {
        labels.insert("catastrophic".to_string());
        rationale.push("catastrophic (never-approvable) command detected");
    }

    if has_destructive_tokens(&command_text) {
        labels.insert("destructive".to_string());
        rationale.push("destructive shell token or command detected");
    }
    if has_write_intent(&lower) {
        labels.insert("write_intent".to_string());
        rationale.push("output redirection or write-intent command detected");
    }
    if has_curl_pipe_shell(&lower) {
        labels.insert("curl_pipe_shell".to_string());
        rationale.push("download piped into shell detected");
    }
    if has_credential_file_read(&lower) {
        labels.insert("credential_file_read".to_string());
        rationale.push("credential-like file read detected");
    }
    if has_network_exfil(&lower) {
        labels.insert("network_exfil".to_string());
        rationale.push("network transfer primitive detected");
    }
    if lower.contains("sudo ") || lower.starts_with("sudo") {
        labels.insert("sudo".to_string());
        rationale.push("privilege escalation via sudo detected");
    }
    if has_package_install(&lower) {
        labels.insert("package_install".to_string());
        rationale.push("package installation command detected");
    }
    if lower.contains("git push") && (lower.contains("--force") || lower.contains("-f")) {
        labels.insert("git_force_push".to_string());
        rationale.push("git force-push detected");
    }
    if has_process_kill(&lower) {
        labels.insert("process_kill".to_string());
        rationale.push("process kill command detected");
    }
    if path_outside_workspace(ctx) {
        labels.insert("outside_workspace".to_string());
        rationale.push("cwd or absolute path is outside workspace roots");
    }
    if let Some(policy) = policy {
        if first_deny_pattern(policy, ctx).is_some() {
            labels.insert("deny_pattern".to_string());
            rationale.push("command matched a configured deny pattern");
        }
    }

    let labels = labels.into_iter().collect::<Vec<_>>();
    let recommended = if labels.is_empty() {
        "allow"
    } else if labels.iter().any(|label| {
        matches!(
            label.as_str(),
            "catastrophic"
                | "destructive"
                | "curl_pipe_shell"
                | "credential_file_read"
                | "network_exfil"
        )
    }) {
        "deny"
    } else {
        "require_approval"
    };
    let mut scan = serde_json::json!({
        "action": recommended,
        "recommended_action": recommended,
        "risk_labels": labels,
        "confidence": if recommended == "allow" { 0.45 } else { 0.86 },
        "rationale": if rationale.is_empty() {
            "no high-risk command patterns detected".to_string()
        } else {
            rationale.join("; ")
        },
    });
    if let Some(reason) = catastrophe {
        scan["catastrophic_reason"] = JsonValue::String(reason);
        // Kept for existing scan consumers; the floor is now single-tier and
        // every catastrophic classification is universal/never-approvable.
        scan["catastrophic_category"] = JsonValue::String("universal".to_string());
    }
    scan
}

/// Workspace roots the catastrophic floor uses to judge whether an `rm -rf`
/// target escapes the workspace. Mirrors the roots the context builder already
/// populates (`command_context_json`): the policy roots when set, otherwise the
/// execution root. An empty result makes the floor treat every absolute path as
/// an escape (conservative), matching burin's `project_root: None` behavior.
pub(super) fn scan_workspace_roots(ctx: &JsonValue) -> Vec<String> {
    ctx.get("workspace_roots")
        .and_then(|value| value.as_array())
        .map(|roots| {
            roots
                .iter()
                .filter_map(|root| root.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A never-approvable hard-deny for the catastrophic floor or a policy
/// `deny_labels` entry. Returns the blocking decision to append, if any. This is
/// evaluated before the consent/approval routing and is NEVER sent through the
/// consent gate: a catastrophic command is never approvable, and `deny_labels`
/// promotes the named scanner labels to the same never-approvable tier. The
/// `catastrophic` label is always enforced regardless of policy configuration.
pub(super) fn hard_deny_decision(
    scan: &JsonValue,
    policy: &CommandPolicy,
    risk_labels: &[String],
) -> Option<CommandPolicyDecision> {
    if let Some(reason) = scan
        .get("catastrophic_reason")
        .and_then(|value| value.as_str())
    {
        return Some(decision(
            "deny",
            Some(reason.to_string()),
            "catastrophic_floor",
            risk_labels.to_vec(),
            1.0,
        ));
    }
    if let Some(label) = risk_labels
        .iter()
        .find(|label| policy.deny_labels.contains(label.as_str()))
    {
        let msg = format!("command hard-denied by policy deny_labels for risk class {label}");
        return Some(decision(
            "deny",
            Some(msg),
            "deny_labels",
            risk_labels.to_vec(),
            1.0,
        ));
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DenyPatternMatch {
    pub(super) pattern: String,
    pub(super) candidate: String,
}

pub(super) fn first_deny_pattern(
    policy: &CommandPolicy,
    ctx: &JsonValue,
) -> Option<DenyPatternMatch> {
    let candidates = deny_pattern_candidates(ctx);
    policy.deny_patterns.iter().find_map(|pattern| {
        candidates
            .iter()
            .filter(|candidate| deny_pattern_matches(pattern, candidate))
            .min_by_key(|candidate| candidate.len())
            .map(|candidate| DenyPatternMatch {
                pattern: pattern.clone(),
                candidate: candidate.clone(),
            })
    })
}

pub(super) fn deny_pattern_candidates(ctx: &JsonValue) -> Vec<String> {
    let mut candidates = Vec::new();
    let command = floor_command_text(ctx);
    for candidate in super::catastrophic::command_segments(&command) {
        push_deny_pattern_candidate(&mut candidates, candidate);
    }
    push_deny_pattern_candidate(&mut candidates, command.clone());
    let legacy_command = command_text(ctx);
    if legacy_command != command {
        for candidate in super::catastrophic::command_segments(&legacy_command) {
            push_deny_pattern_candidate(&mut candidates, candidate);
        }
        push_deny_pattern_candidate(&mut candidates, legacy_command);
    }
    candidates
}

pub(super) fn push_deny_pattern_candidate(candidates: &mut Vec<String>, candidate: String) {
    let candidate = candidate.trim();
    if !candidate.is_empty() && !candidates.iter().any(|existing| existing == candidate) {
        candidates.push(candidate.to_string());
    }
}

pub(super) fn deny_pattern_matches(pattern: &str, candidate: &str) -> bool {
    if pattern.contains('*') {
        super::super::glob_match(pattern, candidate)
    } else {
        candidate.contains(pattern)
    }
}

pub(super) fn command_text(ctx: &JsonValue) -> String {
    if let Some(argv) = ctx
        .pointer("/request/argv")
        .and_then(|value| value.as_array())
    {
        let joined = argv
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        if !joined.is_empty() {
            return joined;
        }
    }
    ctx.pointer("/request/command")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Reconstruct the command line the catastrophic floor classifies, PRESERVING
/// argv argument boundaries. `command_text` space-joins argv with no quoting,
/// which is fine for the substring / verb-scan detectors that scan for a verb
/// anywhere, but silently destroys the boundaries the token-based floor rules
/// depend on. The canonical agent shape `argv:["sh","-c","git reset --hard"]`
/// would otherwise flatten to `sh -c git reset --hard`, and `shell_c_script`
/// would then recover only the bare token `git` after `-c` — evading every
/// token-based rule (`git reset --hard`, `rm -rf /`, `dd of=`, `mkfs`,
/// `chmod -R 000`, `git push --force`, `truncate -s 0`). Shell-quoting each argv
/// element makes argv-mode classify identically to the equivalent shell-mode
/// string (the `-c` payload survives as a single token and recurses correctly).
pub(super) fn floor_command_text(ctx: &JsonValue) -> String {
    if let Some(argv) = ctx
        .pointer("/request/argv")
        .and_then(|value| value.as_array())
    {
        let parts = argv
            .iter()
            .filter_map(|value| value.as_str())
            .map(shell_quote_arg)
            .collect::<Vec<_>>();
        if !parts.is_empty() {
            return parts.join(" ");
        }
    }
    ctx.pointer("/request/command")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Universal catastrophic-command floor for embedders and the hostlib
/// command-execution chokepoint. Returns a blocking reason iff the given argv
/// classifies as a catastrophe that is never legitimate as arbitrary process
/// execution: fork bomb, `mkfs`, `dd of=<device>`, `rm -rf` escaping
/// `workspace_roots`, `chmod -R 000`, `truncate -s 0` of a source file,
/// redirect-over-source, project-root delete, or textual git-destructive
/// commands (`git reset --hard`, `git clean -fd`, force-push).
///
/// This is the load-bearing UNIVERSAL backstop. It is meant to be enforced
/// UNCONDITIONALLY (no `command_policy` on the stack required) at the point a
/// process is about to be spawned, so a bare `run_command` / `process.exec` —
/// on standalone Harn or under any embedder — can never spawn one of these.
/// Embedders therefore do not (and must not) re-plumb the floor themselves.
///
/// Structured git builtins remain the reviewed path for legitimate
/// force-with-lease flows; this chokepoint protects arbitrary textual process
/// execution before any child can spawn.
///
/// `program` is argv[0] and `args` the remaining argv elements. A shell-mode
/// command reaches this as its resolved argv (e.g. `["sh", "-c", "<script>"]`),
/// which the classifier unwraps and recurses into — the same
/// `floor_command_text` argv-shell-quoting the policy path uses, so the
/// `sh -c "<script>"` wrapper bypass stays closed here too. `workspace_roots`
/// are the absolute roots an `rm -rf` target must stay inside; an empty slice
/// conservatively treats every absolute path as an escape.
pub(super) fn scan_universal_catastrophic_reason(
    program: &str,
    args: &[String],
    workspace_roots: &[String],
) -> Option<String> {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(shell_quote_arg(program));
    parts.extend(args.iter().map(|arg| shell_quote_arg(arg)));
    let command = parts.join(" ");
    super::catastrophic::reason(&command, workspace_roots)
}

/// Quote a single argv element so that re-joining the elements with spaces
/// reproduces a shell command line whose tokenization matches the original argv
/// boundaries. A token made only of shell-neutral characters (no whitespace and
/// none of the shell metacharacters the floor's tokenizer/splitter act on) is
/// safe to leave bare; anything else is wrapped in single quotes with embedded
/// single quotes escaped as `'\''`.
pub(super) fn shell_quote_arg(arg: &str) -> String {
    let is_safe = !arg.is_empty()
        && arg.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'_' | b'-' | b'.' | b'/' | b':' | b'=' | b'+' | b',' | b'@' | b'%'
                )
        });
    if is_safe {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('\'');
    for ch in arg.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

pub(super) fn risk_labels_from_scan(scan: &JsonValue) -> Vec<String> {
    scan.get("risk_labels")
        .and_then(|value| value.as_array())
        .map(|labels| {
            labels
                .iter()
                .filter_map(|label| label.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn has_destructive_tokens(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("rm -rf /")
        || lower.contains("rm -fr /")
        || lower.contains("mkfs")
        // A raw-device/file overwrite is `dd of=…`; the input side (`dd if=…`)
        // is a read and is not destructive on its own. (The catastrophic floor
        // independently hard-denies `dd of=…`.)
        || lower.contains("dd of=")
        || lower.contains(":(){")
        || lower.contains("chmod -r 777 /")
        || lower.contains("chown -r ")
        || has_cwd_wipe_tokens(text)
}

/// Detects recursive deletes that destroy the current workspace itself —
/// `rm -rf .` / `rm -rf ./*` / `rm -rf *` and the `find . -delete` /
/// `find . -exec rm ...` family. Root-anchored wipes (`rm -rf /`) are caught by
/// the substring checks above; this covers the cwd-/workspace-relative shapes a
/// prompt injection can use to wipe everything under the working directory
/// without ever naming `/`.
///
/// Boundary (deliberate): a recursive delete of a *named* subdirectory
/// (`rm -rf build/`, `rm -rf node_modules`, `rm -rf ./src`) is a normal clean
/// and is intentionally NOT flagged here. The dangerous set is limited to
/// targets that resolve to the whole working tree: `.`, `./`, `./*`, `*`, and
/// bare `find .` deletes. Flag order (`-rf` / `-fr` / `-r -f`), an optional
/// `--` end-of-options marker, and surrounding whitespace are all normalized.
pub(super) fn has_cwd_wipe_tokens(text: &str) -> bool {
    // Split on shell statement separators so a benign prefix
    // (`cd foo && rm -rf .`) does not hide a wipe in a later clause.
    text.split(['\n', ';', '|', '&'])
        .any(segment_is_workspace_wipe)
}

pub(super) fn segment_is_workspace_wipe(segment: &str) -> bool {
    let tokens: Vec<&str> = segment.split_whitespace().collect();
    // Evaluate every dangerous command word, not just the segment head: the
    // real command text is frequently wrapped (`sh -c rm -rf .`, `cd x && rm
    // -rf .`, `sudo rm -rf .`, `powershell -c "rm -r -fo ."`, `cmd /c "rd /s /q
    // ."`), so the dangerous verb is rarely token 0. We scan for the verb
    // anywhere and judge the tokens that follow it.
    //
    // The PowerShell `Remove-Item` family and the UNIX `rm` deliberately share
    // the `rm_targets_workspace` judge: the aliases (`rm`/`del`/`rmdir`/`ri`/…)
    // overlap with UNIX verbs, and both treat `-r`/`-recurse` as the wipe
    // trigger with the cwd/glob/drive-root target set. `del`/`rmdir`/`rd` route
    // through BOTH the PowerShell-alias path and the cmd.exe path so a `del /s
    // /q .` (cmd) and `del -recurse .` (ps alias) are each caught.
    tokens.iter().enumerate().any(|(idx, raw_token)| {
        let token = command_arg_text(raw_token);
        let rest = &tokens[idx + 1..];
        match token.as_str() {
            "sh" | "bash" | "zsh" => shell_c_payload_is_workspace_wipe(rest),
            "cmd" | "cmd.exe" => cmd_c_payload_is_workspace_wipe(rest),
            "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe" => {
                powershell_c_payload_is_workspace_wipe(rest)
            }
            // UNIX rm + PowerShell `Remove-Item` and its `rm`/`ri` aliases.
            "rm" | "remove-item" | "ri" => rm_targets_workspace(rest),
            "find" => find_deletes_workspace(rest),
            // cmd.exe directory/file deletes use `/s /q` flags + a cwd/drive
            // target. `del`/`rmdir`/`rd`/`erase` are ALSO PowerShell aliases, so
            // try both judges (either matching is dangerous).
            "rmdir" | "rd" | "del" | "erase" => {
                cmd_delete_targets_workspace(rest) || rm_targets_workspace(rest)
            }
            // `format` / `format.com` reformatting a volume is unconditionally
            // destructive once a drive target is present.
            "format" | "format.com" => format_targets_drive(rest),
            _ => false,
        }
    })
}

/// cmd.exe `rmdir`/`rd`/`del`/`erase` wipe judge. The danger signature is a
/// recursive flag (`/s`) together with a target that resolves to the whole
/// working tree or a drive root (`.`, `*`, `*.*`, `c:\`, `\`). Flag order is
/// irrelevant (`/s /q` vs `/q /s`) and `/q` (quiet) does not change the
/// judgment — `/s` alone wipes. cmd flags are case-insensitive (`/S`), but the
/// caller already lowercased the command text.
pub(super) fn cmd_delete_targets_workspace(args: &[&str]) -> bool {
    let mut recursive = false;
    let mut cwd_target = false;
    let mut drive_target = false;
    for raw_arg in args {
        let arg = command_arg_text(raw_arg);
        if let Some(flag) = arg.strip_prefix('/') {
            // `/s`, `/q`, `/f`, and combined forms like `/s/q` (no space).
            if flag.split('/').any(|f| f.starts_with('s')) {
                recursive = true;
            }
            continue;
        }
        if is_drive_root(&arg) {
            // A whole-volume target (`c:\`, `c:\*.*`, `\`) is destructive on its
            // own — `del c:\*.*` clears the drive root without any `/s`.
            drive_target = true;
        } else if is_workspace_wipe_target(raw_arg) {
            cwd_target = true;
        }
    }
    // Drive-root wipe is unconditional; a cwd/glob wipe needs recursion (`/s`)
    // to reach the whole tree (`rmdir /s /q .`, `del /f /s /q *`).
    drive_target || (recursive && cwd_target)
}

/// `format <drive>` reformats a whole volume; a drive-letter or device target
/// (`c:`, `c:\`, `\\.\…`) is the destructive shape. A `format /?` help query or
/// a bare `format` with no volume is not.
pub(super) fn format_targets_drive(args: &[&str]) -> bool {
    args.iter()
        .map(|arg| command_arg_text(arg))
        .any(|arg| !arg.starts_with('/') && (is_drive_root(&arg) || arg.starts_with("\\\\.\\")))
}

/// True when an `rm` invocation is both recursive *and* targets the whole
/// working tree (`.`, `./`, `./*`, or `*`). Named paths are left alone.
pub(super) fn rm_targets_workspace(args: &[&str]) -> bool {
    let mut recursive = false;
    let mut wipe_target = false;
    let mut end_of_options = false;

    for raw_arg in args {
        let arg = command_arg_text(raw_arg);
        if !end_of_options && arg == "--" {
            end_of_options = true;
            continue;
        }
        if !end_of_options && arg.starts_with('-') && arg.len() > 1 {
            // GNU long option (`--recursive`), UNIX short clusters (`-rf`,
            // `-fr`, `-r`), AND PowerShell single-dash options (`-recurse`,
            // `-r`, `-rec`, `-force`, `-fo`, `-literalpath`/`-path`).
            // `-f`/`--force`/`-force`/`-fo` is irrelevant to the wipe judgment:
            // an interactive `rm -r .` (or `Remove-Item -Recurse .`) still
            // destroys the tree. Input is already lowercased by the caller.
            if let Some(long) = arg.strip_prefix("--") {
                if long == "recursive" {
                    recursive = true;
                }
            } else {
                let opt = &arg[1..];
                // PowerShell `-Recurse` (and prefix-abbreviations `-r`, `-rec`,
                // `-recurse`): `recurse` starts with `opt`, so `-r`/`-rec` match.
                // UNIX short clusters (`-rf`, `-fr`) embed `r` as a flag char.
                // A pure PowerShell `-force`/`-fo`/`-path` must NOT set
                // recursion — guard against `force`/`path` etc. matching the
                // `r`-cluster scan by only treating multi-letter tokens that are
                // an abbreviation of a known long option as such.
                if "recurse".starts_with(opt) && !opt.is_empty() {
                    // `-r`, `-re`, `-rec`, `-recu`, … `-recurse`.
                    recursive = true;
                } else if !is_powershell_long_option(opt) {
                    // UNIX short cluster: any `r`/`R` char triggers recursion.
                    for ch in opt.chars() {
                        if ch == 'r' || ch == 'R' {
                            recursive = true;
                        }
                    }
                }
            }
            continue;
        }
        if is_workspace_wipe_target(raw_arg) {
            wipe_target = true;
        }
    }

    // Require recursion: a non-recursive `rm .` cannot remove the tree anyway.
    recursive && wipe_target
}

/// True when `opt` (the text after a single leading `-`, lowercased) is an
/// abbreviation of a PowerShell named parameter other than `-Recurse`. These
/// tokens must NOT be scanned as a UNIX short-flag cluster (otherwise `-force`
/// would falsely set recursion via its `r`, and `-path`/`-literalpath` likewise
/// contain no `r` but should still be treated as PS options, not clusters).
pub(super) fn is_powershell_long_option(opt: &str) -> bool {
    const PS_LONG: &[&str] = &[
        "force",
        "path",
        "literalpath",
        "confirm",
        "whatif",
        "verbose",
    ];
    PS_LONG.iter().any(|long| long.starts_with(opt))
}

pub(super) fn shell_c_payload_is_workspace_wipe(args: &[&str]) -> bool {
    shell_payload_after_flag(args, |arg| {
        arg == "-c" || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains('c'))
    })
}

pub(super) fn cmd_c_payload_is_workspace_wipe(args: &[&str]) -> bool {
    shell_payload_after_flag(args, |arg| arg == "/c")
}

pub(super) fn powershell_c_payload_is_workspace_wipe(args: &[&str]) -> bool {
    if shell_payload_after_flag(args, is_powershell_command_flag) {
        return true;
    }
    for (idx, raw_arg) in args.iter().enumerate() {
        let arg = command_arg_text(raw_arg);
        if is_powershell_encoded_command_flag(&arg) && idx + 1 < args.len() {
            if let Some(decoded) = decode_powershell_encoded_command(args[idx + 1]) {
                if has_cwd_wipe_tokens(&decoded) {
                    return true;
                }
            }
        }
    }
    false
}

pub(super) fn shell_payload_after_flag(
    args: &[&str],
    is_command_flag: impl Fn(&str) -> bool,
) -> bool {
    for (idx, raw_arg) in args.iter().enumerate() {
        let arg = command_arg_text(raw_arg);
        if is_command_flag(&arg) && idx + 1 < args.len() {
            let payload = args[idx + 1..].join(" ");
            let unquoted = strip_outer_shell_quotes(&payload);
            if segment_is_workspace_wipe(&unquoted) {
                return true;
            }
        }
    }
    false
}

pub(super) fn is_powershell_command_flag(arg: &str) -> bool {
    matches!(arg, "/c" | "/command") || (arg.starts_with('-') && "-command".starts_with(arg))
}

pub(super) fn is_powershell_encoded_command_flag(arg: &str) -> bool {
    matches!(arg, "/encodedcommand") || (arg.starts_with('-') && "-encodedcommand".starts_with(arg))
}

pub(super) fn decode_powershell_encoded_command(raw_arg: &str) -> Option<String> {
    let encoded = shell_token(raw_arg).text;
    let bytes = BASE64_STANDARD.decode(encoded.trim()).ok()?;
    if bytes.len() % 2 != 0 {
        return None;
    }
    let utf16 = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    String::from_utf16(&utf16)
        .ok()
        .map(|text| text.trim_start_matches('\u{feff}').to_string())
}

pub(super) fn command_arg_text(token: &str) -> String {
    shell_token(token).text.to_ascii_lowercase()
}

#[derive(Debug)]
pub(super) struct ShellToken {
    text: String,
    single_quoted: Vec<bool>,
}

pub(super) fn shell_token(token: &str) -> ShellToken {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum QuoteMode {
        None,
        Single,
        Double,
    }

    let mut mode = QuoteMode::None;
    let mut text = String::new();
    let mut single_quoted = Vec::new();
    for ch in token.trim().chars() {
        match (mode, ch) {
            (QuoteMode::None, '\'') => mode = QuoteMode::Single,
            (QuoteMode::Single, '\'') => mode = QuoteMode::None,
            (QuoteMode::None, '"') => mode = QuoteMode::Double,
            (QuoteMode::Double, '"') => mode = QuoteMode::None,
            _ => {
                text.push(ch);
                single_quoted.push(mode == QuoteMode::Single);
            }
        }
    }
    ShellToken {
        text,
        single_quoted,
    }
}

pub(super) fn strip_outer_shell_quotes(payload: &str) -> String {
    let trimmed = payload.trim();
    let Some(first) = trimmed.chars().next() else {
        return String::new();
    };
    if !matches!(first, '\'' | '"') || !trimmed.ends_with(first) || trimmed.len() < 2 {
        return trimmed.to_string();
    }
    trimmed[first.len_utf8()..trimmed.len() - first.len_utf8()].to_string()
}

/// A target string that resolves to the entire working directory or a drive
/// root. Covers UNIX cwd/glob shapes, the PowerShell `$pwd`/`.\*` forms, and
/// Windows drive roots (`c:\`, `c:`, `\`, `*.*`).
pub(super) fn is_workspace_wipe_target(arg: &str) -> bool {
    let token = shell_token(arg);
    let arg = token.text.to_ascii_lowercase();
    matches!(
        arg.as_str(),
        "." | "./" | "./*" | "*" | ".*" | "./." | ".\\" | ".\\*" | "*.*" | "\\"
    ) || is_pwd_workspace_target(&token, &arg)
        || is_drive_root(&arg)
}

pub(super) fn is_pwd_workspace_target(token: &ShellToken, arg: &str) -> bool {
    if starts_with_unquoted(token, arg, "$pwd") {
        let rest = &arg["$pwd".len()..];
        return pwd_suffix_wipes_workspace(rest);
    }
    if starts_with_unquoted(token, arg, "$(pwd)") {
        let rest = &arg["$(pwd)".len()..];
        return pwd_suffix_wipes_workspace(rest);
    }
    if starts_with_unquoted(token, arg, "`pwd`") {
        let rest = &arg["`pwd`".len()..];
        return pwd_suffix_wipes_workspace(rest);
    }
    if starts_with_unquoted(token, arg, "${pwd") {
        let rest = &arg["${pwd".len()..];
        if let Some((parameter, suffix)) = rest.split_once('}') {
            if unquoted_prefix(token, "${pwd".len() + parameter.len() + 1)
                && (parameter.is_empty() || parameter.starts_with(':'))
            {
                return pwd_suffix_wipes_workspace(suffix);
            }
        }
    }
    false
}

pub(super) fn starts_with_unquoted(token: &ShellToken, arg: &str, prefix: &str) -> bool {
    arg.starts_with(prefix) && unquoted_prefix(token, prefix.len())
}

pub(super) fn unquoted_prefix(token: &ShellToken, len: usize) -> bool {
    token
        .single_quoted
        .iter()
        .take(len)
        .all(|single_quoted| !*single_quoted)
}

pub(super) fn pwd_suffix_wipes_workspace(suffix: &str) -> bool {
    matches!(
        suffix,
        "" | "/" | "/." | "/*" | "/./" | "/./*" | "\\" | "\\." | "\\*" | "\\.\\" | "\\.\\*"
    )
}

/// Windows drive-root target: `c:`, `c:\`, `c:/`, or `c:\*`. A drive letter
/// followed by only a separator and optional glob wipes the whole volume.
pub(super) fn is_drive_root(arg: &str) -> bool {
    let bytes = arg.as_bytes();
    if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return false;
    }
    // After `x:` the remainder must be empty, a bare separator, or a root glob.
    matches!(&arg[2..], "" | "\\" | "/" | "\\*" | "/*" | "\\*.*" | "*.*")
}

/// True when a `find` invocation roots at the cwd (`.` / `./`) and carries a
/// destructive action (`-delete`, or `-exec`/`-execdir` running `rm`).
pub(super) fn find_deletes_workspace(args: &[&str]) -> bool {
    // The first non-option token is the search root; a cwd root is the
    // dangerous case. `find -delete` (no explicit root) also defaults to cwd.
    let roots_at_cwd = match args
        .iter()
        .find(|arg| !command_arg_text(arg).starts_with('-'))
    {
        Some(&root) => is_workspace_wipe_target(root),
        None => true,
    };
    if !roots_at_cwd {
        return false;
    }
    let has_delete = args.iter().any(|arg| command_arg_text(arg) == "-delete");
    let has_exec_rm = args.windows(2).any(|pair| {
        matches!(command_arg_text(pair[0]).as_str(), "-exec" | "-execdir")
            && command_arg_text(pair[1]) == "rm"
    });
    has_delete || has_exec_rm
}

pub(super) fn has_write_intent(lower: &str) -> bool {
    has_output_redirect_write_intent(lower)
        || lower.contains(" tee ")
        || lower.starts_with("tee ")
        || lower.contains("|tee ")
        || lower.contains(";tee ")
        || lower.contains("sed -i")
        || lower.contains("perl -pi")
        || lower.contains("truncate ")
}

/// Detect unquoted shell output redirects that target files, including compact
/// forms such as `cmd>out`, `1>out`, and `2>err`. POSIX shells define output
/// redirection as `[n]>word`; cmd.exe also treats `>` as output redirection.
/// Do not classify descriptor duplication/close (`2>&1`, `>&-`) or process
/// device sinks (`>/dev/null`, `>nul`) as file write intent.
pub(super) fn has_output_redirect_write_intent(lower: &str) -> bool {
    let mut quote = QuoteMode::None;
    let mut escaped = false;
    let chars = lower.chars().collect::<Vec<_>>();
    let mut idx = 0;
    while idx < chars.len() {
        let ch = chars[idx];
        if escaped {
            escaped = false;
            idx += 1;
            continue;
        }
        if ch == '\\' && quote != QuoteMode::Single {
            escaped = true;
            idx += 1;
            continue;
        }
        quote = update_quote_mode(quote, ch);
        if quote != QuoteMode::None {
            idx += 1;
            continue;
        }

        let amp_redirect = ch == '&' && idx + 1 < chars.len() && chars[idx + 1] == '>';
        let output_redirect = ch == '>';
        if amp_redirect || output_redirect {
            let op_start = idx;
            let mut op_end = idx + 1;
            if amp_redirect {
                op_end += 1;
            }
            if op_end < chars.len() && matches!(chars[op_end], '>' | '|') {
                op_end += 1;
            }
            let after_operator = if !amp_redirect && op_end < chars.len() && chars[op_end] == '&' {
                op_end + 1
            } else {
                op_end
            };
            let target = redirect_target(&chars, after_operator);
            if redirect_target_is_write(target.as_deref()) {
                return true;
            }
            idx = op_end.max(op_start + 1);
            continue;
        }
        idx += 1;
    }
    false
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum QuoteMode {
    None,
    Single,
    Double,
}

pub(super) fn update_quote_mode(mode: QuoteMode, ch: char) -> QuoteMode {
    match (mode, ch) {
        (QuoteMode::None, '\'') => QuoteMode::Single,
        (QuoteMode::Single, '\'') => QuoteMode::None,
        (QuoteMode::None, '"') => QuoteMode::Double,
        (QuoteMode::Double, '"') => QuoteMode::None,
        _ => mode,
    }
}

pub(super) fn redirect_target(chars: &[char], start: usize) -> Option<String> {
    let mut idx = start;
    while idx < chars.len() && chars[idx].is_whitespace() {
        idx += 1;
    }
    if idx >= chars.len() {
        return None;
    }
    let mut quote = QuoteMode::None;
    let mut escaped = false;
    let mut target = String::new();
    while idx < chars.len() {
        let ch = chars[idx];
        if escaped {
            target.push(ch);
            escaped = false;
            idx += 1;
            continue;
        }
        if ch == '\\' && quote != QuoteMode::Single {
            escaped = true;
            idx += 1;
            continue;
        }
        let next_quote = update_quote_mode(quote, ch);
        if next_quote != quote {
            quote = next_quote;
            idx += 1;
            continue;
        }
        if quote == QuoteMode::None
            && (ch.is_whitespace() || matches!(ch, ';' | '|' | '&' | '<' | '>' | '(' | ')'))
        {
            break;
        }
        target.push(ch);
        idx += 1;
    }
    let target = target.trim().to_string();
    (!target.is_empty()).then_some(target)
}

pub(super) fn redirect_target_is_write(target: Option<&str>) -> bool {
    let Some(target) = target else {
        return true;
    };
    if target == "-" || target.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    !is_output_sink_target(target)
}

pub(super) fn is_output_sink_target(target: &str) -> bool {
    matches!(
        target.trim_end_matches(':'),
        "/dev/null" | "/dev/stdout" | "/dev/stderr" | "nul"
    ) || target
        .strip_prefix("/dev/fd/")
        .is_some_and(|fd| !fd.is_empty() && fd.bytes().all(|byte| byte.is_ascii_digit()))
}

pub(super) fn has_curl_pipe_shell(lower: &str) -> bool {
    (lower.contains("curl ") || lower.contains("wget "))
        && lower.contains('|')
        && (lower.contains(" sh") || lower.contains(" bash") || lower.contains(" zsh"))
}

pub(super) fn has_credential_file_read(lower: &str) -> bool {
    let readish = lower.contains("cat ")
        || lower.contains("less ")
        || lower.contains("head ")
        || lower.contains("tail ")
        || lower.contains("grep ");
    readish && contains_secret_like_text(lower)
}

pub(super) fn contains_secret_like_text(lower: &str) -> bool {
    [
        ".env",
        "id_rsa",
        "id_ed25519",
        ".aws/credentials",
        ".npmrc",
        ".netrc",
        "credentials",
        "secret",
        "token",
        "api_key",
        "apikey",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

pub(super) fn has_network_exfil(lower: &str) -> bool {
    lower.contains(" curl ")
        || lower.starts_with("curl ")
        || lower.contains(" wget ")
        || lower.starts_with("wget ")
        || lower.contains(" scp ")
        || lower.starts_with("scp ")
        || lower.contains(" rsync ")
        || lower.starts_with("rsync ")
        || lower.contains(" nc ")
        || lower.starts_with("nc ")
        || lower.contains(" ncat ")
        || lower.starts_with("ncat ")
}

pub(super) fn has_package_install(lower: &str) -> bool {
    lower.contains("npm install")
        || lower.contains("pnpm add")
        || lower.contains("yarn add")
        || lower.contains("pip install")
        || lower.contains("cargo install")
        || lower.contains("brew install")
        || lower.contains("apt install")
        || lower.contains("apt-get install")
}

pub(super) fn has_process_kill(lower: &str) -> bool {
    lower.starts_with("kill ")
        || lower.contains(" kill ")
        || lower.starts_with("pkill ")
        || lower.contains(" pkill ")
        || lower.starts_with("killall ")
        || lower.contains(" killall ")
}

pub(super) fn path_outside_workspace(ctx: &JsonValue) -> bool {
    let roots = ctx
        .get("workspace_roots")
        .and_then(|value| value.as_array())
        .map(|roots| {
            roots
                .iter()
                .filter_map(|root| root.as_str().map(normalize_path))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if roots.is_empty() {
        return false;
    }
    let cwd = ctx
        .pointer("/request/cwd")
        .and_then(|value| value.as_str())
        .map(normalize_path);
    if cwd.as_ref().is_some_and(|cwd| !under_any_root(cwd, &roots)) {
        return true;
    }
    for path in absolute_path_candidates(&command_text(ctx)) {
        if !under_any_root(&normalize_path(&path), &roots) {
            return true;
        }
    }
    false
}

pub(super) fn absolute_path_candidates(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter_map(|part| {
            let trimmed = part.trim_matches(|c| matches!(c, '"' | '\'' | ',' | ';' | ')'));
            trimmed.starts_with('/').then(|| trimmed.to_string())
        })
        .collect()
}

pub(super) fn normalize_path(path: &str) -> PathBuf {
    let path = Path::new(path);
    let raw = if path.is_absolute() {
        path.to_path_buf()
    } else {
        crate::stdlib::process::execution_root_path().join(path)
    };
    normalize_path_components(&raw)
}

pub(super) fn normalize_path_components(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

pub(super) fn under_any_root(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

pub(super) fn redact_json_for_llm(value: &JsonValue) -> JsonValue {
    match value {
        JsonValue::Object(map) => JsonValue::Object(
            map.iter()
                .map(|(key, value)| {
                    let lower = key.to_ascii_lowercase();
                    if contains_secret_like_text(&lower) || lower.contains("auth") {
                        (key.clone(), JsonValue::String("<redacted>".to_string()))
                    } else {
                        (key.clone(), redact_json_for_llm(value))
                    }
                })
                .collect(),
        ),
        JsonValue::Array(items) => {
            JsonValue::Array(items.iter().map(redact_json_for_llm).collect())
        }
        JsonValue::String(text) if text.len() > INLINE_OUTPUT_LIMIT => {
            let prefix: String = text.chars().take(INLINE_OUTPUT_LIMIT).collect();
            JsonValue::String(format!("{prefix}...<truncated>"))
        }
        _ => value.clone(),
    }
}

pub(super) fn inline_output_for_scan(value: Option<&JsonValue>) -> String {
    value
        .and_then(|value| value.as_str())
        .map(|text| text.chars().take(INLINE_OUTPUT_LIMIT).collect())
        .unwrap_or_default()
}

pub(super) fn redacted_vm_request(params: &crate::value::DictMap) -> crate::value::DictMap {
    params
        .iter()
        .map(|(key, value)| {
            if key.as_str() == "env" || key.as_str() == "stdin" {
                (
                    key.clone(),
                    VmValue::String(arcstr::ArcStr::from("<redacted>")),
                )
            } else {
                (key.clone(), value.clone())
            }
        })
        .collect()
}

pub(super) fn string_field(
    map: &crate::value::DictMap,
    key: &str,
) -> Result<Option<String>, VmError> {
    match map.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(value)) => Ok(Some(value.to_string())),
        Some(other) => Err(VmError::Runtime(format!(
            "command_policy.{key} must be a string, got {}",
            other.type_name()
        ))),
    }
}

pub(super) fn string_field_raw(map: &crate::value::DictMap, key: &str) -> Option<String> {
    match map.get(key) {
        Some(VmValue::String(value)) => Some(value.to_string()),
        _ => None,
    }
}

pub(super) fn string_list_field(
    map: &crate::value::DictMap,
    key: &str,
) -> Result<Option<Vec<String>>, VmError> {
    match map.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::List(values)) => values
            .iter()
            .map(|value| match value {
                VmValue::String(value) => Ok(value.to_string()),
                other => Err(VmError::Runtime(format!(
                    "command_policy.{key} entries must be strings, got {}",
                    other.type_name()
                ))),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(other) => Err(VmError::Runtime(format!(
            "command_policy.{key} must be a list, got {}",
            other.type_name()
        ))),
    }
}

pub(super) fn bool_field(map: &crate::value::DictMap, key: &str) -> Result<Option<bool>, VmError> {
    match map.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Bool(value)) => Ok(Some(*value)),
        Some(other) => Err(VmError::Runtime(format!(
            "command_policy.{key} must be a bool, got {}",
            other.type_name()
        ))),
    }
}

pub(super) fn closure_field(
    map: &crate::value::DictMap,
    key: &str,
) -> Result<Option<Arc<VmClosure>>, VmError> {
    match map.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Closure(closure)) => Ok(Some(closure.clone())),
        Some(other) => Err(VmError::Runtime(format!(
            "command_policy.{key} must be a closure, got {}",
            other.type_name()
        ))),
    }
}

pub(super) fn truthy(value: Option<&VmValue>) -> bool {
    match value {
        Some(VmValue::Bool(value)) => *value,
        Some(VmValue::String(value)) => !value.is_empty(),
        Some(VmValue::Int(value)) => *value != 0,
        Some(VmValue::Nil) | None => false,
        Some(_) => true,
    }
}

pub(super) fn vm_i64(value: &VmValue) -> Option<i64> {
    match value {
        VmValue::Int(value) => Some(*value),
        VmValue::Float(value) if value.fract() == 0.0 => Some(*value as i64),
        _ => None,
    }
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}
