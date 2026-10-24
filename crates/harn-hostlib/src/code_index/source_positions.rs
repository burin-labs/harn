//! One conversion between source byte offsets and protocol character positions.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Encoding {
    Utf8,
    Utf16,
}

pub(crate) struct LineIndex<'a> {
    source: &'a str,
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub(crate) fn new(source: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(source.match_indices('\n').map(|(at, _)| at + 1));
        Self { source, starts }
    }

    fn line_text(&self, line: usize) -> Option<&'a str> {
        let start = *self.starts.get(line)?;
        let end = self
            .starts
            .get(line + 1)
            .map_or(self.source.len(), |next| next - 1);
        self.source.get(start..end)
    }

    pub(crate) fn offset(&self, position: Position, encoding: Encoding) -> Option<usize> {
        let line = position.line as usize;
        let text = self.line_text(line)?;
        let wanted = position.character as usize;
        let mut units = 0;
        for (at, ch) in text.char_indices() {
            if units >= wanted {
                return (units == wanted).then_some(self.starts[line] + at);
            }
            units += match encoding {
                Encoding::Utf8 => ch.len_utf8(),
                Encoding::Utf16 => ch.len_utf16(),
            };
        }
        (units == wanted).then_some(self.starts[line] + text.len())
    }

    pub(crate) fn position(&self, offset: usize, encoding: Encoding) -> Position {
        let line = self.starts.partition_point(|start| *start <= offset) - 1;
        // Tree-sitter and offset() supply character-boundary offsets.
        let prefix = self
            .source
            .get(self.starts[line]..offset)
            .unwrap_or_default();
        let character = match encoding {
            Encoding::Utf8 => prefix.len(),
            Encoding::Utf16 => prefix.encode_utf16().count(),
        };
        Position {
            line: line as u32,
            character: character as u32,
        }
    }
}
