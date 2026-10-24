use crate::{schema, value::VmValue};

/// Keep the longest valid prefix, closing unfinished strings and containers.
pub(super) fn best_effort_partial(body: &str) -> VmValue {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return VmValue::Nil;
    }
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return schema::json_to_vm_value(&json);
    }
    // Bound the repeated parses for large incomplete documents.
    const MAX_PARTIAL_SEARCH_BYTES: usize = 256 * 1024;
    if trimmed.len() > MAX_PARTIAL_SEARCH_BYTES {
        return VmValue::Nil;
    }
    let mut frontiers = collect_partial_frontiers(trimmed);
    frontiers.sort_by_key(|frontier| std::cmp::Reverse(frontier.end));
    for frontier in frontiers {
        let candidate = close_partial_frontier(
            trimmed.get(..frontier.end).expect("character boundary"),
            frontier.in_string,
            &frontier.stack,
        );
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&candidate) {
            return schema::json_to_vm_value(&json);
        }
    }
    VmValue::Nil
}

struct PartialFrontier {
    end: usize,
    in_string: bool,
    stack: Vec<char>,
}

fn collect_partial_frontiers(body: &str) -> Vec<PartialFrontier> {
    let mut frontiers = Vec::new();
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in body.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => {
                stack.push('}');
                frontiers.push(PartialFrontier {
                    end: index + ch.len_utf8(),
                    in_string: false,
                    stack: stack.clone(),
                });
            }
            '[' => {
                stack.push(']');
                frontiers.push(PartialFrontier {
                    end: index + ch.len_utf8(),
                    in_string: false,
                    stack: stack.clone(),
                });
            }
            '}' | ']' => {
                stack.pop();
                frontiers.push(PartialFrontier {
                    end: index + ch.len_utf8(),
                    in_string: false,
                    stack: stack.clone(),
                });
            }
            // Drop an unfinished member together with its separator.
            ',' => frontiers.push(PartialFrontier {
                end: index,
                in_string: false,
                stack: stack.clone(),
            }),
            _ => {}
        }
    }
    frontiers.push(PartialFrontier {
        end: body.len(),
        in_string,
        stack,
    });
    frontiers
}

fn close_partial_frontier(head: &str, in_string: bool, stack: &[char]) -> String {
    let mut candidate = String::with_capacity(head.len() + stack.len() + 1);
    candidate.push_str(head);
    if in_string {
        candidate.push('"');
    }
    for closer in stack.iter().rev() {
        candidate.push(*closer);
    }
    candidate
}
