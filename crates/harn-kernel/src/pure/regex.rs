use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

const REGEX_CACHE_LIMIT: usize = 128;
pub const MAX_REGEX_PATTERN_BYTES: usize = 64 * 1024;

thread_local! {
    static REGEX_CACHE: RefCell<HashMap<String, Rc<regex::Regex>>> = RefCell::new(HashMap::new());
    static LAST_REGEX: RefCell<Option<(String, String, Rc<regex::Regex>)>> =
        const { RefCell::new(None) };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegexCapture {
    pub full_match: String,
    pub groups: Vec<Option<String>>,
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub named: BTreeMap<String, String>,
}

fn compiled(pattern: &str, flags: &str) -> Result<Rc<regex::Regex>, String> {
    if pattern.len() > MAX_REGEX_PATTERN_BYTES {
        return Err(format!(
            "regex pattern exceeds the {MAX_REGEX_PATTERN_BYTES}-byte limit"
        ));
    }
    if let Some(regex) = LAST_REGEX.with(|slot| {
        slot.borrow()
            .as_ref()
            .filter(|(cached_pattern, cached_flags, _)| {
                cached_pattern == pattern && cached_flags == flags
            })
            .map(|(_, _, regex)| Rc::clone(regex))
    }) {
        return Ok(regex);
    }

    let regex = REGEX_CACHE.with(|cache| -> Result<Rc<regex::Regex>, String> {
        let key = format!("{flags}\0{pattern}");
        let mut cache = cache.borrow_mut();
        if let Some(regex) = cache.get(&key) {
            return Ok(Rc::clone(regex));
        }
        let mut builder = regex::RegexBuilder::new(pattern);
        for flag in flags.chars() {
            match flag {
                'i' => builder.case_insensitive(true),
                'm' => builder.multi_line(true),
                's' => builder.dot_matches_new_line(true),
                'x' => builder.ignore_whitespace(true),
                _ => {
                    return Err(format!(
                        "unsupported regex flag '{flag}', expected one of i/m/s/x"
                    ));
                }
            };
        }
        let regex = Rc::new(builder.build().map_err(|error| error.to_string())?);
        if cache.len() >= REGEX_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(key, Rc::clone(&regex));
        Ok(regex)
    })?;

    LAST_REGEX.with(|slot| {
        *slot.borrow_mut() = Some((pattern.to_string(), flags.to_string(), Rc::clone(&regex)));
    });
    Ok(regex)
}

pub fn regex_matches(pattern: &str, text: &str, flags: &str) -> Result<Vec<String>, String> {
    Ok(compiled(pattern, flags)?
        .find_iter(text)
        .map(|matched| matched.as_str().to_string())
        .collect())
}

pub fn regex_replace(
    pattern: &str,
    replacement: &str,
    text: &str,
    flags: &str,
) -> Result<String, String> {
    Ok(compiled(pattern, flags)?
        .replace_all(text, replacement)
        .into_owned())
}

pub fn regex_split(pattern: &str, text: &str, flags: &str) -> Result<Vec<String>, String> {
    Ok(compiled(pattern, flags)?
        .split(text)
        .map(str::to_string)
        .collect())
}

pub fn regex_captures(pattern: &str, text: &str, flags: &str) -> Result<Vec<RegexCapture>, String> {
    let regex = compiled(pattern, flags)?;
    let names = regex
        .capture_names()
        .flatten()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut scanned_byte = 0;
    let mut chars_before = 0;
    let mut newlines_before = 0;
    let mut results = Vec::new();

    for captures in regex.captures_iter(text) {
        let whole = captures
            .get(0)
            .expect("regex capture always includes the full match");
        #[expect(clippy::string_slice, reason = "regex match bounds are char boundaries")]
        let gap = &text[scanned_byte..whole.start()];
        chars_before += gap.chars().count();
        newlines_before += gap.bytes().filter(|byte| *byte == b'\n').count();
        let start = chars_before;
        let line = newlines_before + 1;
        let matched = whole.as_str();
        chars_before += matched.chars().count();
        newlines_before += matched.bytes().filter(|byte| *byte == b'\n').count();
        scanned_byte = whole.end();

        let groups = (1..captures.len())
            .map(|index| captures.get(index).map(|value| value.as_str().to_string()))
            .collect();
        let named = names
            .iter()
            .filter_map(|name| {
                captures
                    .name(name)
                    .map(|value| (name.clone(), value.as_str().to_string()))
            })
            .collect();
        results.push(RegexCapture {
            full_match: matched.to_string(),
            groups,
            start,
            end: chars_before,
            line,
            named,
        });
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_use_character_offsets_names_and_lines() {
        let captures = regex_captures(r"(?m)^(?<word>\w+)-(\d+)$", "λ-1\nHarn-42", "").unwrap();
        assert_eq!(captures.len(), 2);
        assert_eq!(captures[0].start, 0);
        assert_eq!(captures[0].end, 3);
        assert_eq!(captures[1].line, 2);
        assert_eq!(captures[1].named["word"], "Harn");
        assert_eq!(captures[1].groups[1].as_deref(), Some("42"));
    }

    #[test]
    fn flags_and_pattern_size_are_bounded() {
        assert!(regex_matches("harn", "HARN", "i").unwrap().len() == 1);
        assert!(regex_matches("harn", "harn", "q").is_err());
        assert!(regex_matches(&"x".repeat(MAX_REGEX_PATTERN_BYTES + 1), "x", "").is_err());
    }
}
