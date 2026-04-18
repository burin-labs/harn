/// Simple URI template matching (RFC 6570 Level 1 only).
///
/// Matches a URI against a template like `file:///{path}` and extracts named
/// variables. Returns `None` if the URI doesn't match the template structure.
pub(super) fn match_uri_template(
    template: &str,
    uri: &str,
) -> Option<std::collections::HashMap<String, String>> {
    let mut vars = std::collections::HashMap::new();
    let mut t_pos = 0;
    let mut u_pos = 0;
    let t_bytes = template.as_bytes();
    let u_bytes = uri.as_bytes();

    while t_pos < t_bytes.len() {
        if t_bytes[t_pos] == b'{' {
            // Find the closing brace
            let close = template[t_pos..].find('}')? + t_pos;
            let var_name = &template[t_pos + 1..close];
            t_pos = close + 1;

            // Capture everything up to the next literal in the template (or end)
            let next_literal = if t_pos < t_bytes.len() {
                // Find how much literal follows
                let lit_start = t_pos;
                let lit_end = template[t_pos..]
                    .find('{')
                    .map(|i| t_pos + i)
                    .unwrap_or(t_bytes.len());
                Some(&template[lit_start..lit_end])
            } else {
                None
            };

            let value_end = match next_literal {
                Some(lit) if !lit.is_empty() => uri[u_pos..].find(lit).map(|i| u_pos + i)?,
                _ => u_bytes.len(),
            };

            vars.insert(var_name.to_string(), uri[u_pos..value_end].to_string());
            u_pos = value_end;
        } else {
            // Literal character must match
            if u_pos >= u_bytes.len() || t_bytes[t_pos] != u_bytes[u_pos] {
                return None;
            }
            t_pos += 1;
            u_pos += 1;
        }
    }

    if u_pos == u_bytes.len() {
        Some(vars)
    } else {
        None
    }
}
