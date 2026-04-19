use crate::value::VmError;

impl super::Vm {
    pub fn format_runtime_error(&self, error: &VmError) -> String {
        let entry_file = self.source_file.as_deref().unwrap_or("<unknown>");
        let entry_source = self.source_text.as_deref();

        let error_msg = format!("{error}");
        let mut out = String::new();

        out.push_str(&format!("error: {error_msg}\n"));

        // Prefer captured stack trace (taken before unwinding); fall back to live frames.
        let frames: Vec<(String, usize, usize, Option<String>)> =
            if !self.error_stack_trace.is_empty() {
                self.error_stack_trace
                    .iter()
                    .map(|(name, line, col, src)| (name.clone(), *line, *col, src.clone()))
                    .collect()
            } else {
                self.frames
                    .iter()
                    .map(|f| {
                        let idx = if f.ip > 0 { f.ip - 1 } else { 0 };
                        let line = f.chunk.lines.get(idx).copied().unwrap_or(0) as usize;
                        let col = f.chunk.columns.get(idx).copied().unwrap_or(0) as usize;
                        (f.fn_name.clone(), line, col, f.chunk.source_file.clone())
                    })
                    .collect()
            };

        if let Some((_name, line, col, frame_file)) = frames.last() {
            let line = *line;
            let col = *col;
            let filename = frame_file.as_deref().unwrap_or(entry_file);
            // Read the frame's own source so the caret line is meaningful;
            // fall back to entry-point source (e.g. for stdlib modules).
            let owned_source: Option<String> = frame_file
                .as_deref()
                .and_then(|p| std::fs::read_to_string(p).ok());
            let source_for_line: Option<&str> =
                owned_source.as_deref().or(if frame_file.is_none() {
                    entry_source
                } else {
                    None
                });
            if line > 0 {
                let display_col = if col > 0 { col } else { 1 };
                let gutter_width = line.to_string().len();
                out.push_str(&format!(
                    "{:>width$}--> {filename}:{line}:{display_col}\n",
                    " ",
                    width = gutter_width + 1,
                ));
                if let Some(source_line) =
                    source_for_line.and_then(|s| s.lines().nth(line.saturating_sub(1)))
                {
                    out.push_str(&format!("{:>width$} |\n", " ", width = gutter_width + 1));
                    out.push_str(&format!(
                        "{:>width$} | {source_line}\n",
                        line,
                        width = gutter_width + 1,
                    ));
                    let caret_col = if col > 0 { col } else { 1 };
                    let trimmed = source_line.trim();
                    let leading = source_line
                        .len()
                        .saturating_sub(source_line.trim_start().len());
                    let caret_len = if col > 0 {
                        Self::token_len_at(source_line, col)
                    } else {
                        trimmed.len().max(1)
                    };
                    let padding = if col > 0 {
                        " ".repeat(caret_col.saturating_sub(1))
                    } else {
                        " ".repeat(leading)
                    };
                    let carets = "^".repeat(caret_len);
                    out.push_str(&format!(
                        "{:>width$} | {padding}{carets}\n",
                        " ",
                        width = gutter_width + 1,
                    ));
                }
            }
        }

        // Call stack, bottom-up, skipping the top frame (already shown).
        if frames.len() > 1 {
            for (name, line, _col, frame_file) in frames.iter().rev().skip(1) {
                let display_name = if name.is_empty() { "pipeline" } else { name };
                if *line > 0 {
                    let filename = frame_file.as_deref().unwrap_or(entry_file);
                    out.push_str(&format!(
                        "  = note: called from {display_name} at {filename}:{line}\n"
                    ));
                }
            }
        }

        out
    }

    /// Estimate the length of the token at the given 1-based column position
    /// in a source line. Scans forward from that position to find a word/operator
    /// boundary.
    fn token_len_at(source_line: &str, col: usize) -> usize {
        let chars: Vec<char> = source_line.chars().collect();
        let start = col.saturating_sub(1);
        if start >= chars.len() {
            return 1;
        }
        let first = chars[start];
        if first.is_alphanumeric() || first == '_' {
            let mut end = start + 1;
            while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
                end += 1;
            }
            end - start
        } else {
            1
        }
    }
}
