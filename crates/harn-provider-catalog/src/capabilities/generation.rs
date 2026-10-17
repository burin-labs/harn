//! Pure generation parsing shared by capability matching and provider adapters.

/// Parse a Claude generation from dotted, dashed, dated, or routed model IDs.
/// A family without a numeric generation returns `None`.
pub fn claude_generation(model: &str) -> Option<(u32, u32)> {
    let lower = model.to_lowercase();
    if !is_claude_model_id(&lower) {
        return None;
    }
    if let Some(tail) = lower.split("claude-").nth(1) {
        if tail.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            return parse_major_minor_tail(tail);
        }
    }
    for family in ["opus", "sonnet", "haiku", "fable", "mythos"] {
        let needle = format!("{family}-");
        if let Some((_, tail)) = lower.split_once(&needle) {
            return parse_major_minor_tail(tail);
        }
    }
    None
}

/// Recognize direct and routed Claude model identities.
pub fn is_claude_model_id(model: &str) -> bool {
    let lower = model.to_lowercase();
    lower.starts_with("claude-") || lower.contains("/claude-") || lower.contains(".claude-")
}

/// Parse a GPT generation, stripping an optional routing prefix.
pub fn gpt_generation(model: &str) -> Option<(u32, u32)> {
    let lower = model.to_lowercase();
    let stripped = lower
        .rsplit_once('/')
        .map_or(lower.as_str(), |(_, tail)| tail);
    let (_, tail) = stripped.split_once("gpt-")?;
    parse_major_minor_tail(tail)
}

fn parse_major_minor_tail(tail: &str) -> Option<(u32, u32)> {
    if let Some((major, rest)) = tail.split_once('.') {
        if let Ok(major) = major.parse::<u32>() {
            let minor_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(minor) = minor_str.parse::<u32>() {
                return Some((major, minor));
            }
        }
    }
    let mut parts = tail.split('-');
    let major = parts.next()?.parse::<u32>().ok()?;
    let Some(minor_chunk) = parts.next() else {
        return Some((major, 0));
    };
    let Ok(minor) = minor_chunk.parse::<u32>() else {
        return Some((major, 0));
    };
    Some((major, if minor >= 1000 { 0 } else { minor }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_tails_preserve_dotted_dashed_and_dated_routes() {
        for (tail, expected) in [
            ("4.7", Some((4, 7))),
            ("5.4-preview", Some((5, 4))),
            ("5.4.1-turbo", Some((5, 4))),
            ("4-7", Some((4, 7))),
            ("5-4-preview", Some((5, 4))),
            ("5", Some((5, 0))),
            ("4-20251001", Some((4, 0))),
            ("5-20260115", Some((5, 0))),
            ("preview", None),
            ("", None),
        ] {
            assert_eq!(parse_major_minor_tail(tail), expected, "{tail}");
        }
    }
}
