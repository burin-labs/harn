use super::Formatter;

impl Formatter {
    /// Emit any comments on the given source line that haven't been emitted yet.
    ///
    /// Non-doc comments are emitted verbatim. Doc comments are coalesced with
    /// any contiguous doc-comment lines that follow (no blank line between
    /// them and no non-doc-comment line interleaved) and rendered as a
    /// canonical `/** */` block.
    pub(crate) fn emit_comments_for_line(&mut self, line: usize) {
        if self.emitted_lines.contains(&line) {
            return;
        }
        let Some(comments) = self.comments.get(&line).cloned() else {
            return;
        };
        let first_is_doc = comments.first().is_some_and(|c| c.is_doc);
        if first_is_doc {
            // Coalesce a contiguous run of doc comments into a single canonical
            // block. A run is contiguous when subsequent commented lines sit
            // immediately below (line+1, line+2, ...) and every one is a doc.
            let mut run_lines: Vec<usize> = vec![line];
            let mut cursor = line + 1;
            while let Some(next_comments) = self.comments.get(&cursor) {
                if self.emitted_lines.contains(&cursor) {
                    break;
                }
                if !next_comments.iter().all(|c| c.is_doc) {
                    break;
                }
                run_lines.push(cursor);
                cursor += 1;
            }
            // Flatten each run element into plain body lines; pre-existing
            // block doc comments may already span multiple lines.
            let mut body_lines: Vec<String> = Vec::new();
            for l in &run_lines {
                if let Some(cs) = self.comments.get(l) {
                    for c in cs {
                        if !c.is_doc {
                            continue;
                        }
                        if c.is_block {
                            // Strip the leading ` *` gutter on interior lines and
                            // any surrounding blank lines left from a previous render.
                            let raw = &c.text;
                            let mut first = true;
                            for raw_line in raw.split('\n') {
                                if first {
                                    first = false;
                                    let t = raw_line.trim();
                                    if t.is_empty() {
                                        continue;
                                    }
                                    body_lines.push(t.to_string());
                                    continue;
                                }
                                let trimmed = raw_line.trim();
                                let stripped = trimmed
                                    .strip_prefix('*')
                                    .map(|s| s.strip_prefix(' ').unwrap_or(s))
                                    .unwrap_or(trimmed);
                                body_lines.push(stripped.to_string());
                            }
                            while body_lines.last().is_some_and(|s| s.is_empty()) {
                                body_lines.pop();
                            }
                        } else {
                            // `///` line doc: trim one leading space for canonical shape.
                            let t = c.text.strip_prefix(' ').unwrap_or(&c.text);
                            body_lines.push(t.trim_end().to_string());
                        }
                    }
                }
            }
            for l in &run_lines {
                self.emitted_lines.insert(*l);
            }
            self.emit_doc_block(&body_lines);
            return;
        }
        self.emitted_lines.insert(line);
        for c in &comments {
            if c.is_block {
                self.writeln(&format!("/*{}*/", c.text));
            } else {
                self.writeln(&format!("//{}", c.text));
            }
        }
    }

    /// Emit a canonical `/** */` doc block from the given body lines.
    /// If the block is a single non-empty line and the compact form fits
    /// within `line_width`, emit `<indent>/** <text> */`. Otherwise emit the
    /// multi-line JSDoc-style form with vertical-aligned stars.
    pub(crate) fn emit_doc_block(&mut self, body_lines: &[String]) {
        let mut start = 0;
        while start < body_lines.len() && body_lines[start].trim().is_empty() {
            start += 1;
        }
        let mut end = body_lines.len();
        while end > start && body_lines[end - 1].trim().is_empty() {
            end -= 1;
        }
        let trimmed: Vec<String> = body_lines[start..end].to_vec();
        if trimmed.is_empty() {
            self.writeln("/** */");
            return;
        }
        if trimmed.len() == 1 {
            let only = trimmed[0].trim();
            let compact = format!("/** {only} */");
            let indent_cols = self.indent * 2;
            if indent_cols + compact.len() <= self.line_width {
                self.writeln(&compact);
                return;
            }
        }
        self.writeln("/**");
        for line in &trimmed {
            if line.trim().is_empty() {
                self.writeln(" *");
            } else {
                self.writeln(&format!(" * {}", line.trim_end()));
            }
        }
        self.writeln(" */");
    }

    /// Emit any standalone comments whose line is between `from` and `to` (exclusive).
    pub(crate) fn emit_comments_in_range(&mut self, from: usize, to: usize) {
        let lines: Vec<usize> = self
            .comments
            .keys()
            .filter(|&&l| l >= from && l < to && !self.emitted_lines.contains(&l))
            .copied()
            .collect();
        for line in lines {
            self.emit_comments_for_line(line);
        }
    }

    /// Emit comments in the given range, recognizing and canonicalizing
    /// section-header separator bars and three-line section blocks. Intended
    /// for use between top-level items (not inside function bodies).
    ///
    /// Returns `true` if at least one section header was emitted in the
    /// range — callers use this to ensure exactly one blank line follows the
    /// last header before the next item.
    pub(crate) fn emit_top_level_comments_in_range(&mut self, from: usize, to: usize) -> bool {
        let lines: Vec<usize> = self
            .comments
            .keys()
            .filter(|&&l| l >= from && l < to && !self.emitted_lines.contains(&l))
            .copied()
            .collect();
        let mut any_section_header = false;
        let mut idx = 0;
        while idx < lines.len() {
            let line = lines[idx];
            if self.emitted_lines.contains(&line) {
                idx += 1;
                continue;
            }
            // Section headers are always single-line standalone `//` comments.
            let here = match self.comments.get(&line).cloned() {
                Some(cs) if cs.len() == 1 && !cs[0].is_block && !cs[0].is_doc => cs,
                _ => {
                    self.emit_comments_for_line(line);
                    idx += 1;
                    continue;
                }
            };
            let here_text = &here[0].text;
            // Three-line section header: `// ----` / `// <title>` / `// ----`
            // with bars of ≥4 dashes surrounding the title.
            if idx + 2 < lines.len()
                && lines[idx + 1] == line + 1
                && lines[idx + 2] == line + 2
                && is_bar_only_line(here_text)
            {
                let mid = self.comments.get(&lines[idx + 1]).cloned();
                let last = self.comments.get(&lines[idx + 2]).cloned();
                if let (Some(mid), Some(last)) = (mid, last) {
                    if mid.len() == 1
                        && !mid[0].is_block
                        && !mid[0].is_doc
                        && last.len() == 1
                        && !last[0].is_block
                        && !last[0].is_doc
                        && is_bar_only_line(&last[0].text)
                    {
                        let title = mid[0].text.trim();
                        if !title.is_empty() && !is_bar_only_line(&mid[0].text) {
                            self.ensure_blank_line_above();
                            self.emit_section_header(title);
                            self.emitted_lines.insert(lines[idx]);
                            self.emitted_lines.insert(lines[idx + 1]);
                            self.emitted_lines.insert(lines[idx + 2]);
                            self.ensure_blank_line_below();
                            any_section_header = true;
                            idx += 3;
                            continue;
                        }
                    }
                }
            }
            // Single-line bar: `// ----` or `// ---- Title ----`.
            if let Some(kind) = classify_bar_line(here_text) {
                self.ensure_blank_line_above();
                match kind {
                    BarKind::PureBar => self.emit_separator_bar(),
                    BarKind::WithTitle(title) => self.emit_section_header(&title),
                }
                self.emitted_lines.insert(line);
                self.ensure_blank_line_below();
                any_section_header = true;
                idx += 1;
                continue;
            }
            self.emit_comments_for_line(line);
            idx += 1;
        }
        any_section_header
    }

    /// Normalize `self.output` to end in exactly `\n\n`, trimming trailing
    /// horizontal whitespace first. Single source of truth for section-header
    /// padding on both sides of a header.
    fn ensure_trailing_blank_line(&mut self) {
        while self.output.ends_with(' ') || self.output.ends_with('\t') {
            self.output.pop();
        }
        if self.output.is_empty() {
            return;
        }
        let trailing = self.output.chars().rev().take_while(|c| *c == '\n').count();
        match trailing {
            0 => self.output.push_str("\n\n"),
            1 => self.output.push('\n'),
            _ => {}
        }
    }

    fn ensure_blank_line_above(&mut self) {
        self.ensure_trailing_blank_line();
    }

    fn ensure_blank_line_below(&mut self) {
        self.ensure_trailing_blank_line();
    }

    fn emit_separator_bar(&mut self) {
        let dashes = self.separator_width.saturating_sub(3);
        let bar: String = "-".repeat(dashes);
        self.writeln(&format!("// {bar}"));
    }

    fn emit_section_header(&mut self, title: &str) {
        let dashes = self.separator_width.saturating_sub(3);
        let bar: String = "-".repeat(dashes);
        self.writeln(&format!("// {bar}"));
        self.writeln(&format!("// {title}"));
        self.writeln(&format!("// {bar}"));
    }
}

/// True iff `text` (raw body after `//`, untrimmed) is a dash-only bar with
/// ≥4 dashes. Delegates to `classify_bar_line` so the two stay in sync.
fn is_bar_only_line(text: &str) -> bool {
    matches!(classify_bar_line(text), Some(BarKind::PureBar))
}

enum BarKind {
    PureBar,
    WithTitle(String),
}

/// Classify a single-line comment body (text after `//`) as a pure bar, a
/// bar-with-inline-title, or neither.
fn classify_bar_line(text: &str) -> Option<BarKind> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    if t.len() >= 4 && t.chars().all(|c| c == '-') {
        return Some(BarKind::PureBar);
    }
    // Inline-titled: ≥4 dashes, title, optional ≥4-dash trailer.
    let chars: Vec<char> = t.chars().collect();
    let lead_dashes = chars.iter().take_while(|c| **c == '-').count();
    if lead_dashes < 4 {
        return None;
    }
    let remaining: String = chars[lead_dashes..].iter().collect();
    let remaining = remaining.trim();
    if remaining.is_empty() {
        return None;
    }
    let rchars: Vec<char> = remaining.chars().collect();
    let trail_dashes = rchars.iter().rev().take_while(|c| **c == '-').count();
    let title = if trail_dashes >= 4 {
        rchars[..rchars.len() - trail_dashes]
            .iter()
            .collect::<String>()
            .trim()
            .to_string()
    } else {
        remaining.to_string()
    };
    if title.is_empty() {
        return None;
    }
    Some(BarKind::WithTitle(title))
}
