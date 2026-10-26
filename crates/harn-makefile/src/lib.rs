//! Static Make declarations shared by native and source-facing projections.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::Serialize;

pub const FILENAMES: [&str; 3] = ["GNUmakefile", "makefile", "Makefile"];
const PREVIEW_LINES: usize = 3;
const PREVIEW_CHARACTERS: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MakefileTarget {
    pub name: String,
    pub command: String,
    pub recipe_preview: Vec<String>,
    pub recipe_line_count: usize,
}

/// Read the first default Makefile, falling back only when a file is missing.
pub fn read_default(root: &Path) -> io::Result<Option<(&'static str, String)>> {
    for filename in FILENAMES {
        match fs::read_to_string(root.join(filename)) {
            Ok(source) => return Ok(Some((filename, source))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

/// Read literal target declarations without expanding variables or executing Make.
pub fn parse(source: &str) -> Vec<MakefileTarget> {
    let mut targets: Vec<MakefileTarget> = Vec::new();
    let mut indices = BTreeMap::new();
    let mut current = Vec::new();
    let mut recipe_started = false;
    let mut prefix = '\t';
    let mut pending = String::new();
    let mut define_depth = 0usize;

    for raw in source.lines() {
        if define_depth == 0 && pending.is_empty() && raw.starts_with(prefix) {
            #[expect(
                clippy::string_slice,
                reason = "starts_with matched the complete recipe-prefix character"
            )]
            let recipe = raw[prefix.len_utf8()..].trim();
            append_recipe(&mut targets, &current, &mut recipe_started, recipe);
            continue;
        }
        if !pending.is_empty() {
            pending.push(' ');
        }
        pending.push_str(raw.trim_start());
        if pending.chars().rev().take_while(|c| *c == '\\').count() % 2 == 1 {
            pending.pop();
            continue;
        }
        let logical = std::mem::take(&mut pending);
        let line = without_comment(&logical).trim();
        if line.is_empty() {
            continue;
        }
        if line == "endef" {
            define_depth = define_depth.saturating_sub(1);
            continue;
        }
        let mut words = line.split_whitespace();
        let first = words.next();
        if first == Some("define") || (first == Some("override") && words.next() == Some("define"))
        {
            define_depth += 1;
        }
        if define_depth > 0 {
            current.clear();
            continue;
        }
        current.clear();
        recipe_started = false;
        if let Some(value) = line.strip_prefix(".RECIPEPREFIX") {
            if let Some((_, value)) = value.split_once('=') {
                prefix = value.trim_start().chars().next().unwrap_or('\t');
            }
            continue;
        }
        let Some(colon) = unescaped(line, ':') else {
            continue;
        };
        let (head, tail) = line.split_at(colon);
        #[expect(
            clippy::string_slice,
            reason = "tail begins at the matched ASCII colon"
        )]
        let tail = &tail[1..];
        // Assignments can contain colons in either their operator or value.
        #[expect(
            clippy::string_slice,
            reason = "unescaped returns a char_indices UTF-8 boundary"
        )]
        let prerequisites = unescaped(tail, ';').map_or(tail, |index| &tail[..index]);
        if head.contains('=')
            || head.contains('$')
            || tail.trim_start_matches(':').starts_with('=')
            || unescaped(prerequisites, '=').is_some()
        {
            continue;
        }
        let names = target_names(head);
        for name in names {
            let index = *indices.entry(name.clone()).or_insert_with(|| {
                let index = targets.len();
                let terminator = if name.starts_with('-') { " --" } else { "" };
                targets.push(MakefileTarget {
                    command: format!("make{terminator} {}", shell_words::quote(&name)),
                    name,
                    recipe_preview: Vec::new(),
                    recipe_line_count: 0,
                });
                index
            });
            if !current.contains(&index) {
                current.push(index);
            }
        }
        if let Some(semicolon) = unescaped(tail, ';') {
            #[expect(
                clippy::string_slice,
                reason = "unescaped returns a UTF-8 boundary and semicolon is ASCII"
            )]
            let recipe = &tail[semicolon + 1..];
            append_recipe(&mut targets, &current, &mut recipe_started, recipe);
        }
    }
    targets
}

fn append_recipe(
    targets: &mut [MakefileTarget],
    current: &[usize],
    started: &mut bool,
    recipe: &str,
) {
    let recipe = recipe.trim().trim_start_matches(['@', '-', '+']).trim();
    for &index in current {
        let target = &mut targets[index];
        if !*started {
            target.recipe_preview.clear();
            target.recipe_line_count = 0;
        }
        if !recipe.is_empty() {
            target.recipe_line_count += 1;
            if target.recipe_preview.len() < PREVIEW_LINES {
                let preview: String = recipe.chars().take(PREVIEW_CHARACTERS).collect();
                target.recipe_preview.push(preview);
            }
        }
    }
    *started = true;
}

fn unescaped(text: &str, needle: char) -> Option<usize> {
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        if !escaped && ch == needle {
            return Some(index);
        }
        escaped = !escaped && ch == '\\';
    }
    None
}

#[expect(
    clippy::string_slice,
    reason = "unescaped returns a char_indices UTF-8 boundary"
)]
fn without_comment(text: &str) -> &str {
    match (unescaped(text, '#'), unescaped(text, ';')) {
        (Some(comment), Some(recipe)) if recipe < comment => text,
        (Some(comment), _) => &text[..comment],
        _ => text,
    }
}

fn target_names(head: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut escaped = false;
    for ch in head.chars().chain(std::iter::once(' ')) {
        if escaped {
            word.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch.is_whitespace() {
            if !word.is_empty() {
                if word != "&" && !word.contains(['$', '%', '=']) {
                    words.push(std::mem::take(&mut word));
                } else {
                    word.clear();
                }
            }
        } else {
            word.push(ch);
        }
    }
    words
}
