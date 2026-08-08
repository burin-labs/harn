//! The structural token the scanner emits, and its wire shape.
//!
//! A unit is a delimited top-level span of the response plus the few facts
//! Rust already had to compute in order to find its end. It carries no
//! judgement: whether a `block` names a real tool, whether a `text` run is
//! prose or a stray call, and what any of it means for the turn are decisions
//! `std/llm/tool_parse` makes.
//!
//! Two shapes here exist for a measured reason.
//!
//! * **Text is pre-sliced.** Every unit ships the bytes it covers, so the Harn
//!   walk never re-slices `source`. Slicing per unit in Harn was a per-unit
//!   cost the 2x budget could not absorb.
//! * **`head_name` / `head_sep` are pre-extracted.** A body that opens with
//!   `name(` or `name{` reports the identifier and the separator, so the
//!   common branch in Harn is a dict lookup rather than a `regex_captures`
//!   call. That single field pair was the difference between fitting the perf
//!   budget and blowing it — regex per unit is most of the gap between the
//!   17 µs/unit dispatch floor and the 46 µs/unit full walk.

/// The leading call head of a unit body: `name(` or `name{`.
///
/// Absent when the body does not open with an identifier immediately followed
/// (modulo inline whitespace) by one of those separators. Rust deliberately
/// does NOT decide whether `name` is a registered tool — that is policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallHead {
    pub(crate) name: String,
    pub(crate) sep: char,
}

/// What a delimited span turned out to be, with the per-kind facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UnitPayload {
    /// A run containing no structural opener. Heredoc bodies stay *inside*
    /// the run, so a bare `name({ key: <<EOF … EOF })` survives the chunker.
    Text { text: String },
    /// One line consumed because the cursor sits inside a markdown fence, or
    /// is neither line-leading nor adjacent to a consumed block.
    ///
    /// Fencing does NOT make a call inert: the stray sniffer still runs over
    /// this text downstream and dispatches a call naming a registered tool.
    FencedLine { text: String },
    /// A resolved `<tag>…</tag>`.
    Block {
        tag: String,
        body: String,
        head: Option<CallHead>,
        /// True when the span was recovered from a reserved wire opener that
        /// was normalized to the canonical tag. Diagnostics only.
        reserved: bool,
    },
    /// An open tag with no close. `body` is the remainder; consumes to EOF.
    UnclosedBlock {
        tag: String,
        body: String,
        head: Option<CallHead>,
        reserved: bool,
    },
    /// An orphan `</tag>`.
    StrayClose { tag: String },
    /// A contentless wrapper tag.
    Wrapper { tag: String },
    /// Top-level function markup through its close tag, or EOF.
    Markup { opener: String, text: String },
    /// `<name(…)>` where `name` is a known tool. Rust parses the expression
    /// because the extent of the unit depends on it.
    AngleCall {
        name: String,
        arguments: serde_json::Value,
    },
    /// A `<|message|>tool_call to=…` header line.
    HarmonyLine { text: String },
    /// A frame marker or corrupted wrapper consumed structurally.
    HarmonySkip,
    /// An unknown or unclosed tag fragment, to end-of-line or `>`.
    Tag { raw: String, name: String },
}

impl UnitPayload {
    /// The `kind` discriminant Harn switches on.
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            UnitPayload::Text { .. } => "text",
            UnitPayload::FencedLine { .. } => "fenced_line",
            UnitPayload::Block { .. } => "block",
            UnitPayload::UnclosedBlock { .. } => "unclosed_block",
            UnitPayload::StrayClose { .. } => "stray_close",
            UnitPayload::Wrapper { .. } => "wrapper",
            UnitPayload::Markup { .. } => "markup",
            UnitPayload::AngleCall { .. } => "angle_call",
            UnitPayload::HarmonyLine { .. } => "harmony_line",
            UnitPayload::HarmonySkip => "harmony_skip",
            UnitPayload::Tag { .. } => "tag",
        }
    }
}

/// A delimited top-level span of the scanned source.
///
/// `start` and `end` index the `source` the scan returns — the
/// post-thinking-strip text — never the caller's original string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unit {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) payload: UnitPayload,
}

impl Unit {
    pub(crate) fn kind(&self) -> &'static str {
        self.payload.kind()
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert("kind".to_string(), self.kind().into());
        map.insert("start".to_string(), self.start.into());
        map.insert("end".to_string(), self.end.into());
        match &self.payload {
            UnitPayload::Text { text } => {
                map.insert("text".to_string(), text.as_str().into());
                map.insert("fenced".to_string(), false.into());
            }
            UnitPayload::FencedLine { text } | UnitPayload::HarmonyLine { text } => {
                map.insert("text".to_string(), text.as_str().into());
            }
            UnitPayload::Block {
                tag,
                body,
                head,
                reserved,
            } => {
                map.insert("tag".to_string(), tag.as_str().into());
                map.insert("body".to_string(), body.as_str().into());
                map.insert("closed".to_string(), true.into());
                insert_head(&mut map, head);
                insert_reserved(&mut map, *reserved);
            }
            UnitPayload::UnclosedBlock {
                tag,
                body,
                head,
                reserved,
            } => {
                map.insert("tag".to_string(), tag.as_str().into());
                map.insert("body".to_string(), body.as_str().into());
                map.insert("closed".to_string(), false.into());
                insert_head(&mut map, head);
                insert_reserved(&mut map, *reserved);
            }
            UnitPayload::StrayClose { tag } | UnitPayload::Wrapper { tag } => {
                map.insert("tag".to_string(), tag.as_str().into());
            }
            UnitPayload::Markup { opener, text } => {
                map.insert("opener".to_string(), opener.as_str().into());
                map.insert("text".to_string(), text.as_str().into());
            }
            UnitPayload::AngleCall { name, arguments } => {
                map.insert("name".to_string(), name.as_str().into());
                map.insert("arguments".to_string(), arguments.clone());
            }
            UnitPayload::HarmonySkip => {}
            UnitPayload::Tag { raw, name } => {
                map.insert("raw".to_string(), raw.as_str().into());
                map.insert("name".to_string(), name.as_str().into());
            }
        }
        serde_json::Value::Object(map)
    }
}

fn insert_head(map: &mut serde_json::Map<String, serde_json::Value>, head: &Option<CallHead>) {
    if let Some(head) = head {
        map.insert("head_name".to_string(), head.name.as_str().into());
        map.insert("head_sep".to_string(), head.sep.to_string().into());
    }
}

fn insert_reserved(map: &mut serde_json::Map<String, serde_json::Value>, reserved: bool) {
    if reserved {
        map.insert("reserved".to_string(), true.into());
    }
}
