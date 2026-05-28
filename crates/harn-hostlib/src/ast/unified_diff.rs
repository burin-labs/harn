//! Unified-diff renderer used by the `ast.dry_run` preview path.
//!
//! Produces `git apply --check`-compatible output: hunk headers
//! (`@@ -a,b +c,d @@`), three lines of leading and trailing context per
//! hunk, and `\ No newline at end of file` markers when either side lacks
//! a trailing newline. The line diff itself is the Myers O(nd) algorithm
//! over `\n`-split lines.

const CONTEXT: usize = 3;

/// Per-file diff with summary counts. `lines_added` and `lines_removed`
/// are raw `+`/`-` line counts (a modified line shows up as both).
pub(super) struct FileDiff {
    pub(super) text: String,
    pub(super) lines_added: usize,
    pub(super) lines_removed: usize,
}

/// Render a unified diff for one file going from `before` to `after`.
///
/// `path` becomes the `a/...` and `b/...` label. Either side being empty
/// is reported as creation (`--- /dev/null`) or deletion (`+++ /dev/null`).
/// Identical inputs produce no body (empty `FileDiff::text`).
pub(super) fn render(path: &str, before: &str, after: &str, kind: ChangeKind) -> FileDiff {
    if before == after {
        return FileDiff {
            text: String::new(),
            lines_added: 0,
            lines_removed: 0,
        };
    }

    let (before_lines, before_has_trailing_nl) = split_lines(before);
    let (after_lines, after_has_trailing_nl) = split_lines(after);
    // Wrap each line as `(text, unterminated_last)` so two textually
    // equal lines compare unequal when only one is the trailing
    // unterminated line of the file. Without this, `"a\nb"` vs
    // `"a\nb\n"` would produce a no-op diff (both yield `["a", "b"]`)
    // and the trailing-newline change would silently disappear.
    let before_keys: Vec<LineKey<'_>> = before_lines
        .iter()
        .enumerate()
        .map(|(i, line)| LineKey {
            text: line,
            unterminated_last: !before_has_trailing_nl && i + 1 == before_lines.len(),
        })
        .collect();
    let after_keys: Vec<LineKey<'_>> = after_lines
        .iter()
        .enumerate()
        .map(|(i, line)| LineKey {
            text: line,
            unterminated_last: !after_has_trailing_nl && i + 1 == after_lines.len(),
        })
        .collect();
    let ops = myers_diff(&before_keys, &after_keys);
    let hunks = group_hunks(&ops, CONTEXT);

    let mut out = String::new();
    match kind {
        ChangeKind::Modify => {
            out.push_str(&format!("--- a/{path}\n+++ b/{path}\n"));
        }
        ChangeKind::Create => {
            out.push_str(&format!("--- /dev/null\n+++ b/{path}\n"));
        }
        ChangeKind::Delete => {
            out.push_str(&format!("--- a/{path}\n+++ /dev/null\n"));
        }
    }

    let mut lines_added = 0usize;
    let mut lines_removed = 0usize;
    for hunk in &hunks {
        let before_count = hunk
            .ops
            .iter()
            .filter(|op| matches!(op.kind, DiffOp::Equal | DiffOp::Delete))
            .count();
        let after_count = hunk
            .ops
            .iter()
            .filter(|op| matches!(op.kind, DiffOp::Equal | DiffOp::Insert))
            .count();
        let before_start = hunk.before_start + 1;
        let after_start = hunk.after_start + 1;
        out.push_str(&format!(
            "@@ -{} +{} @@\n",
            format_range(before_start, before_count),
            format_range(after_start, after_count),
        ));
        for op in &hunk.ops {
            match op.kind {
                DiffOp::Equal => {
                    out.push(' ');
                    out.push_str(before_lines[op.before_idx.unwrap()]);
                    out.push('\n');
                    if op.before_idx.unwrap() + 1 == before_lines.len() && !before_has_trailing_nl {
                        out.push_str("\\ No newline at end of file\n");
                    }
                }
                DiffOp::Delete => {
                    lines_removed += 1;
                    out.push('-');
                    out.push_str(before_lines[op.before_idx.unwrap()]);
                    out.push('\n');
                    if op.before_idx.unwrap() + 1 == before_lines.len() && !before_has_trailing_nl {
                        out.push_str("\\ No newline at end of file\n");
                    }
                }
                DiffOp::Insert => {
                    lines_added += 1;
                    out.push('+');
                    out.push_str(after_lines[op.after_idx.unwrap()]);
                    out.push('\n');
                    if op.after_idx.unwrap() + 1 == after_lines.len() && !after_has_trailing_nl {
                        out.push_str("\\ No newline at end of file\n");
                    }
                }
            }
        }
    }

    FileDiff {
        text: out,
        lines_added,
        lines_removed,
    }
}

/// File-level change kind. Drives the header line shape.
#[derive(Clone, Copy)]
pub(super) enum ChangeKind {
    Modify,
    Create,
    Delete,
}

fn split_lines(text: &str) -> (Vec<&str>, bool) {
    if text.is_empty() {
        return (Vec::new(), true);
    }
    let has_trailing_nl = text.ends_with('\n');
    let body = if has_trailing_nl {
        &text[..text.len() - 1]
    } else {
        text
    };
    (body.split('\n').collect(), has_trailing_nl)
}

fn format_range(start: usize, count: usize) -> String {
    if count == 1 {
        format!("{start}")
    } else if count == 0 {
        // Convention: an empty range renders as `start,0` with `start`
        // set to the line *before* the gap (so 0 for top-of-file).
        format!("{},0", start.saturating_sub(1))
    } else {
        format!("{start},{count}")
    }
}

// ---------------------------------------------------------------------------
// Myers diff (O(nd)) + hunk grouping
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DiffOp {
    Equal,
    Delete,
    Insert,
}

#[derive(Clone, Copy, Debug)]
struct ScriptStep {
    kind: DiffOp,
    before_idx: Option<usize>,
    after_idx: Option<usize>,
}

struct Hunk {
    before_start: usize,
    after_start: usize,
    ops: Vec<ScriptStep>,
}

fn group_hunks(ops: &[ScriptStep], context: usize) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut i = 0;
    while i < ops.len() {
        if matches!(ops[i].kind, DiffOp::Equal) {
            i += 1;
            continue;
        }
        // Found a change. Walk back up to `context` Equal ops as leading
        // context, then forward until we either run out or see more than
        // `2*context` consecutive Equal ops (which is the cue to start a
        // new hunk).
        let mut start = i;
        let mut equals_back = 0;
        while start > 0 && matches!(ops[start - 1].kind, DiffOp::Equal) && equals_back < context {
            start -= 1;
            equals_back += 1;
        }
        let mut end = i;
        let mut equal_run = 0;
        while end < ops.len() {
            match ops[end].kind {
                DiffOp::Equal => {
                    equal_run += 1;
                    if equal_run > 2 * context {
                        break;
                    }
                }
                _ => {
                    equal_run = 0;
                }
            }
            end += 1;
        }
        // Trim trailing Equal run to `context` lines max.
        let mut tail = end;
        let mut trailing_equals = 0;
        while tail > start
            && matches!(ops[tail - 1].kind, DiffOp::Equal)
            && trailing_equals < context * 100
        {
            trailing_equals += 1;
            tail -= 1;
        }
        let trailing = trailing_equals.min(context);
        let final_end = tail + trailing;
        let slice: Vec<ScriptStep> = ops[start..final_end].to_vec();
        let before_start = slice
            .iter()
            .find_map(|op| op.before_idx)
            .or_else(|| {
                ops[..start]
                    .iter()
                    .rev()
                    .find_map(|op| op.before_idx)
                    .map(|x| x + 1)
            })
            .unwrap_or(0);
        let after_start = slice
            .iter()
            .find_map(|op| op.after_idx)
            .or_else(|| {
                ops[..start]
                    .iter()
                    .rev()
                    .find_map(|op| op.after_idx)
                    .map(|x| x + 1)
            })
            .unwrap_or(0);
        hunks.push(Hunk {
            before_start,
            after_start,
            ops: slice,
        });
        i = final_end;
    }
    hunks
}

/// Composite line key. Two lines compare equal only if they have the
/// same text *and* the same "I am the unterminated last line" flag, so
/// trailing-newline changes never silently collapse.
#[derive(Clone, Copy, PartialEq, Eq)]
struct LineKey<'a> {
    text: &'a str,
    unterminated_last: bool,
}

fn myers_diff(a: &[LineKey<'_>], b: &[LineKey<'_>]) -> Vec<ScriptStep> {
    let n = a.len();
    let m = b.len();
    if n == 0 && m == 0 {
        return Vec::new();
    }
    if n == 0 {
        return (0..m)
            .map(|j| ScriptStep {
                kind: DiffOp::Insert,
                before_idx: None,
                after_idx: Some(j),
            })
            .collect();
    }
    if m == 0 {
        return (0..n)
            .map(|i| ScriptStep {
                kind: DiffOp::Delete,
                before_idx: Some(i),
                after_idx: None,
            })
            .collect();
    }

    let n_i = n as isize;
    let m_i = m as isize;
    let max_d = (n + m) as isize;
    let offset = max_d;
    let v_size = (2 * max_d + 1) as usize;
    let mut v = vec![0isize; v_size];
    let mut trace: Vec<Vec<isize>> = Vec::new();

    'outer: for d in 0..=max_d {
        trace.push(v.clone());
        let mut new_v = v.clone();
        for k in (-d..=d).step_by(2) {
            let ki = (k + offset) as usize;
            #[allow(clippy::suspicious_operation_groupings)]
            let mut x = if k == -d || (k != d && v[ki - 1] < v[ki + 1]) {
                v[ki + 1]
            } else {
                v[ki - 1] + 1
            };
            let mut y = x - k;
            #[allow(clippy::suspicious_operation_groupings)]
            while x < n_i && y < m_i && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            new_v[ki] = x;
            if x >= n_i && y >= m_i {
                break 'outer;
            }
        }
        v = new_v;
    }

    let mut ops: Vec<ScriptStep> = Vec::new();
    let mut x = n_i;
    let mut y = m_i;
    for d in (1..trace.len() as isize).rev() {
        let k = x - y;
        let v_prev = &trace[d as usize];
        let prev_k = if k == -d
            || (k != d && v_prev[(k - 1 + offset) as usize] < v_prev[(k + 1 + offset) as usize])
        {
            k + 1
        } else {
            k - 1
        };
        let prev_x = v_prev[(prev_k + offset) as usize];
        let prev_y = prev_x - prev_k;

        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            ops.push(ScriptStep {
                kind: DiffOp::Equal,
                before_idx: Some(x as usize),
                after_idx: Some(y as usize),
            });
        }
        if prev_k < k {
            x -= 1;
            ops.push(ScriptStep {
                kind: DiffOp::Delete,
                before_idx: Some(x as usize),
                after_idx: None,
            });
        } else {
            y -= 1;
            ops.push(ScriptStep {
                kind: DiffOp::Insert,
                before_idx: None,
                after_idx: Some(y as usize),
            });
        }
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        ops.push(ScriptStep {
            kind: DiffOp::Equal,
            before_idx: Some(x as usize),
            after_idx: Some(y as usize),
        });
    }
    ops.reverse();
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_change_yields_empty_diff() {
        let out = render(
            "foo.txt",
            "alpha\nbeta\n",
            "alpha\nbeta\n",
            ChangeKind::Modify,
        );
        assert!(out.text.is_empty());
        assert_eq!(out.lines_added, 0);
        assert_eq!(out.lines_removed, 0);
    }

    #[test]
    fn single_line_modification_emits_hunk_header() {
        let out = render(
            "src/lib.rs",
            "fn alpha() { 1 }\nfn beta() { 2 }\n",
            "fn alpha() { 1 }\nfn beta() { 42 }\n",
            ChangeKind::Modify,
        );
        assert!(out.text.contains("--- a/src/lib.rs"));
        assert!(out.text.contains("+++ b/src/lib.rs"));
        assert!(out.text.contains("@@ -"));
        assert!(out.text.contains("-fn beta() { 2 }"));
        assert!(out.text.contains("+fn beta() { 42 }"));
        assert_eq!(out.lines_added, 1);
        assert_eq!(out.lines_removed, 1);
    }

    #[test]
    fn file_creation_uses_dev_null_a() {
        let out = render("new.txt", "", "hello\n", ChangeKind::Create);
        assert!(out.text.starts_with("--- /dev/null\n+++ b/new.txt\n"));
        assert!(out.text.contains("+hello"));
        assert_eq!(out.lines_added, 1);
        assert_eq!(out.lines_removed, 0);
    }

    #[test]
    fn file_deletion_uses_dev_null_b() {
        let out = render("old.txt", "bye\n", "", ChangeKind::Delete);
        assert!(out.text.starts_with("--- a/old.txt\n+++ /dev/null\n"));
        assert!(out.text.contains("-bye"));
        assert_eq!(out.lines_removed, 1);
        assert_eq!(out.lines_added, 0);
    }

    #[test]
    fn missing_trailing_newline_emits_marker() {
        let out = render("f.txt", "alpha\nbeta", "alpha\nbeta\n", ChangeKind::Modify);
        // The before side lacks a trailing newline; the marker must
        // appear on the `-beta` removal so `git apply` recovers the
        // original byte sequence.
        assert!(out.text.contains("-beta\n\\ No newline at end of file\n"));
    }

    #[test]
    fn hunk_with_context_marks_three_surrounding_lines() {
        let before = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let after = "a\nb\nc\nD\ne\nf\ng\nh\ni\nj\n";
        let out = render("f.txt", before, after, ChangeKind::Modify);
        // Three lines before the change and three after — ten total
        // lines but only seven appear in the hunk plus the modified pair.
        let body = out.text.lines().collect::<Vec<_>>();
        assert!(body.iter().any(|l| l.starts_with(" a")));
        assert!(body.iter().any(|l| l.starts_with(" g")));
        assert!(!body.iter().any(|l| l.starts_with(" j")));
        assert!(body.contains(&"-d"));
        assert!(body.contains(&"+D"));
    }

    #[test]
    fn distant_changes_produce_separate_hunks() {
        let before = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n";
        let after = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\nL\n";
        let out = render("f.txt", before, after, ChangeKind::Modify);
        let hunk_headers = out.text.matches("@@ -").count();
        assert_eq!(hunk_headers, 2);
    }
}
