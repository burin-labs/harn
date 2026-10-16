//! Author-facing filter contracts. Native rendering dispatches exhaustively
//! over `FilterId`, so a declared filter cannot lack an implementation.

/// One filter's name, arguments, and completion/hover documentation.
pub struct Filter {
    pub id: FilterId,
    pub name: &'static str,
    pub params: &'static [&'static str],
    pub required: usize,
    pub summary: &'static str,
}

macro_rules! define_filters {
    ($($id:ident => ($name:literal, $params:expr, $required:literal, $summary:literal)),* $(,)?) => {
        /// Closed set of filters the renderer must implement.
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum FilterId { $($id),* }

        /// Every filter accepted by the prompt-template engine.
        pub static FILTERS: &[Filter] = &[
            $(Filter { id: FilterId::$id, name: $name, params: $params,
                required: $required, summary: $summary }),*
        ];
    };
}

define_filters! {
    Upper => ("upper", &[], 0, "Uppercase the value."),
    Lower => ("lower", &[], 0, "Lowercase the value."),
    Trim => ("trim", &[], 0, "Strip leading and trailing whitespace."),
    Capitalize => ("capitalize", &[], 0, "Uppercase the first character and lowercase the rest."),
    Title => ("title", &[], 0, "Uppercase the first character of every word."),
    Length => ("length", &[], 0, "Number of characters, items, or entries."),
    First => ("first", &[], 0, "First item of a list or set, or first character of a string."),
    Last => ("last", &[], 0, "Last item of a list or set, or last character of a string."),
    Reverse => ("reverse", &[], 0, "Reverse a list or string. Other values pass through unchanged."),
    Join => ("join", &["separator"], 1, "Join a list or set into a string with the given separator."),
    Default => ("default", &["fallback"], 1, "Substitute the fallback when the value is falsy."),
    Json => ("json", &["pretty"], 0, "Serialize the value as JSON, optionally pretty-printed."),
    Indent => ("indent", &["width", "indent_first"], 1,
        "Indent every line by `width` spaces, skipping the first line unless `indent_first` is true."),
    Lines => ("lines", &[], 0, "Split the value into a list of lines."),
    EscapeMd => ("escape_md", &[], 0, "Backslash-escape Markdown punctuation."),
    Replace => ("replace", &["from", "to"], 2, "Replace every occurrence of `from` with `to`."),
}

impl Filter {
    /// Author-facing call signature, including optional trailing arguments.
    pub fn signature(&self) -> String {
        if self.params.is_empty() {
            return self.name.to_string();
        }
        let mut out = format!("{}: ", self.name);
        for (index, param) in self.params.iter().enumerate() {
            if index == self.required {
                out.push('[');
            }
            if index > 0 {
                out.push_str(", ");
            }
            out.push_str(param);
        }
        if self.required < self.params.len() {
            out.push(']');
        }
        out
    }
}

/// Resolve an author-written filter name.
pub fn lookup(name: &str) -> Option<&'static Filter> {
    FILTERS.iter().find(|filter| filter.name == name)
}

#[cfg(test)]
mod tests {
    use super::{lookup, FILTERS};

    #[test]
    fn filter_names_are_unique() {
        let mut names: Vec<&str> = FILTERS.iter().map(|filter| filter.name).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate filter name in the table");
    }

    #[test]
    fn required_arguments_are_a_prefix_of_the_parameter_list() {
        for filter in FILTERS {
            assert!(filter.required <= filter.params.len(), "{}", filter.name);
        }
    }

    #[test]
    fn signatures_mark_optional_arguments() {
        for (name, expected) in [
            ("upper", "upper"),
            ("join", "join: separator"),
            ("replace", "replace: from, to"),
            ("json", "json: [pretty]"),
            ("indent", "indent: width[, indent_first]"),
        ] {
            assert_eq!(lookup(name).unwrap().signature(), expected);
        }
    }

    #[test]
    fn unknown_names_are_not_filters() {
        assert!(lookup("endif").is_none());
        assert!(lookup("").is_none());
    }
}
