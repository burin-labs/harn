use super::*;

impl super::Vm {
    pub fn format_runtime_error(&self, error: &VmError) -> String {
        let source = match &self.source_text {
            Some(s) => s.as_str(),
            None => return format!("error: {error}"),
        };
        let filename = self.source_file.as_deref().unwrap_or("<unknown>");

        let error_msg = format!("{error}");
        let mut out = String::new();

        // Error header
        out.push_str(&format!("error: {error_msg}\n"));

        // Get the error location from the top frame (line, column)
        let frames: Vec<(&str, usize, usize)> = self
            .frames
            .iter()
            .map(|f| {
                let idx = if f.ip > 0 { f.ip - 1 } else { 0 };
                let line = if idx < f.chunk.lines.len() {
                    f.chunk.lines[idx] as usize
                } else {
                    0
                };
                let col = if idx < f.chunk.columns.len() {
                    f.chunk.columns[idx] as usize
                } else {
                    0
                };
                (f.fn_name.as_str(), line, col)
            })
            .collect();

        if let Some((_name, line, col)) = frames.last() {
            let line = *line;
            let col = *col;
            if line > 0 {
                let display_col = if col > 0 { col } else { 1 };
                let gutter_width = line.to_string().len();
                out.push_str(&format!(
                    "{:>width$}--> {filename}:{line}:{display_col}\n",
                    " ",
                    width = gutter_width + 1,
                ));
                // Show source line with caret
                if let Some(source_line) = source.lines().nth(line.saturating_sub(1)) {
                    out.push_str(&format!("{:>width$} |\n", " ", width = gutter_width + 1));
                    out.push_str(&format!(
                        "{:>width$} | {source_line}\n",
                        line,
                        width = gutter_width + 1,
                    ));
                    // Render caret line
                    let caret_col = if col > 0 { col } else { 1 };
                    let trimmed = source_line.trim();
                    let leading = source_line
                        .len()
                        .saturating_sub(source_line.trim_start().len());
                    // Calculate how many carets to show
                    let caret_len = if col > 0 {
                        // Try to find a reasonable token length at this column
                        Self::token_len_at(source_line, col)
                    } else {
                        // No column info: underline the trimmed content
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

        // Show call stack (bottom-up, skipping top frame which is already shown)
        if frames.len() > 1 {
            for (name, line, _col) in frames.iter().rev().skip(1) {
                let display_name = if name.is_empty() { "pipeline" } else { name };
                if *line > 0 {
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
            // Scan forward through identifier/number chars
            let mut end = start + 1;
            while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
                end += 1;
            }
            end - start
        } else {
            // Operator or punctuation: just one caret
            1
        }
    }
}
