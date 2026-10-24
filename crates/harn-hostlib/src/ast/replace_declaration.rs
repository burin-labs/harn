use super::language::Language;
use super::replace_body::contains_multiline_token;

pub(super) fn replacement(
    source: &str,
    start: usize,
    end: usize,
    language: Language,
    new_text: &str,
) -> String {
    let before = source.get(..start).expect("parsed declaration start");
    let line_start = before.rfind('\n').map_or(0, |offset| offset + 1);
    let prefix = source.get(line_start..start).expect("declaration prefix");
    let indent = if prefix.bytes().all(|byte| matches!(byte, b' ' | b'\t')) {
        prefix
    } else {
        ""
    };
    let incoming = new_text.trim_matches(['\r', '\n']);
    let replacement = if contains_multiline_token(incoming, language) != Some(false) {
        // Multiline token bytes belong to the caller. Layout changes must not
        // rewrite string literals, block comments or other multiline tokens.
        incoming.trim_start_matches([' ', '\t']).to_owned()
    } else {
        let incoming_indent = incoming
            .lines()
            .find(|line| !line.trim().is_empty())
            .and_then(|line| line.get(..line.len() - line.trim_start_matches([' ', '\t']).len()))
            .unwrap_or("");
        let newline = source
            .get(start..)
            .expect("parsed declaration start")
            .find('\n')
            .map(|offset| start + offset)
            .or_else(|| before.rfind('\n'))
            .map_or("\n", |offset| {
                if source
                    .get(..offset)
                    .expect("newline boundary")
                    .ends_with('\r')
                {
                    "\r\n"
                } else {
                    "\n"
                }
            });
        incoming
            .lines()
            .enumerate()
            .map(|(index, line)| {
                let content = line.strip_prefix(incoming_indent).unwrap_or(line);
                if index == 0 || content.is_empty() {
                    content.to_owned()
                } else {
                    format!("{indent}{content}")
                }
            })
            .collect::<Vec<_>>()
            .join(newline)
    };
    format!(
        "{before}{replacement}{}",
        source.get(end..).expect("parsed declaration end")
    )
}
