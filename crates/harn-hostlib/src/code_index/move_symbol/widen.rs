//! Caller-allowed visibility widening for `move_symbol`.
//!
//! A move can leave an item unreachable: the moved item uses a private item
//! of the source module, the source or another module reads a field the
//! moved struct hides, or the moved item is private and others use it.
//! Without `allow_visibility_widening` each of those refuses. With it, the
//! engine widens exactly the blocking declarations, each to the narrowest
//! level that makes it reachable, when that level does not exceed the
//! allowance, and reports every widening. Anything it cannot place exactly
//! still refuses.

use std::ops::Range;

use super::model::{Family, ModuleRef};
use super::response::{refuse_with_sites, Outcome, Site};
use super::text;

/// A visibility level a caller may allow, narrowest first. Rust levels
/// precede `Export`, which is the only TypeScript/JavaScript level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Level {
    /// Rust `pub(super)`.
    Super,
    /// Rust `pub(crate)`.
    Crate,
    /// Rust `pub`.
    Public,
    /// TypeScript/JavaScript `export`.
    Export,
}

impl Level {
    pub(crate) const ACCEPTED: &'static str =
        "`pub(super)`, `pub(crate)`, `pub` (Rust) or `export` (TypeScript/JavaScript)";

    pub(crate) fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "pub(super)" => Self::Super,
            "pub(crate)" => Self::Crate,
            "pub" => Self::Public,
            "export" => Self::Export,
            _ => return None,
        })
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Super => "pub(super)",
            Self::Crate => "pub(crate)",
            Self::Public => "pub",
            Self::Export => "export",
        }
    }

    /// Whether `family` has this level.
    pub(crate) fn fits(self, family: Family) -> bool {
        match family {
            Family::Rust => self != Self::Export,
            Family::Script => self == Self::Export,
            Family::Python => false,
        }
    }
}

/// Who must be able to see a declaration after the move.
#[derive(Clone, Debug)]
pub(crate) enum Reach {
    /// A Rust declaration living in `home`, seen from every module in `viewers`.
    Rust {
        home: ModuleRef,
        viewers: Vec<ModuleRef>,
    },
    /// A TypeScript/JavaScript declaration that other modules import.
    Export,
}

/// One declaration to widen.
#[derive(Clone, Debug)]
pub(crate) struct Fix {
    pub path: String,
    /// The existing modifier, or an empty range where one is inserted.
    pub at: Range<usize>,
    pub line: u32,
    /// The current visibility as written: `private`, `pub(super)`, ...
    pub from: String,
    pub reach: Reach,
    /// The edit lands in the moved item's text rather than a source-file edit.
    pub in_moved: bool,
    /// The fix widens the moved item itself, not something it uses or hides.
    pub item: bool,
}

impl Fix {
    /// The narrowest level that reaches every viewer; `None` when the
    /// declaration is already reachable.
    fn needed(&self) -> Option<Level> {
        let Reach::Rust { home, viewers } = &self.reach else {
            return Some(Level::Export);
        };
        let ModuleRef::Rust {
            crate_dir,
            path: home_path,
        } = home
        else {
            return Some(Level::Public);
        };
        let mut inside_home = true;
        let mut inside_parent = !home_path.is_empty();
        for viewer in viewers {
            let ModuleRef::Rust { crate_dir: c, path } = viewer else {
                return Some(Level::Public);
            };
            if c != crate_dir {
                return Some(Level::Public);
            }
            if !path.starts_with(home_path) {
                inside_home = false;
            }
            if !home_path.is_empty() && !path.starts_with(&home_path[..home_path.len() - 1]) {
                inside_parent = false;
            }
        }
        if inside_home {
            None
        } else if inside_parent {
            Some(Level::Super)
        } else {
            Some(Level::Crate)
        }
    }
}

/// A declaration that blocks the move, with the text edit that would unblock it.
pub(crate) struct Declared {
    pub site: Site,
    pub path: String,
    pub modifier: Option<Range<usize>>,
    /// Where a modifier is inserted when there is none.
    pub at: usize,
    pub from: String,
    /// Whether the engine can place the modifier exactly.
    pub placeable: bool,
}

impl Declared {
    pub(crate) fn fix(
        &self,
        source: &str,
        reach: Reach,
        in_moved: bool,
        item: bool,
    ) -> Option<Fix> {
        if !self.placeable {
            return None;
        }
        let at = self.modifier.clone().unwrap_or(self.at..self.at);
        Some(Fix {
            path: self.path.clone(),
            line: text::line_of(source, at.start),
            at,
            from: self.from.clone(),
            reach,
            in_moved,
            item,
        })
    }
}

/// An unexported TypeScript/JavaScript declaration spanning `range`. A
/// decorated one is not placed: `export` before or after the decorator is
/// not valid under both decorator proposals.
pub(crate) fn unexported(
    rel: &str,
    source: &str,
    range: &Range<usize>,
    reason: String,
) -> Declared {
    Declared {
        site: Site::at(rel, source, range.start, "declaration", reason),
        path: rel.to_string(),
        modifier: None,
        at: range.start,
        from: "unexported".to_string(),
        placeable: !text::slice(source, range.clone()).starts_with('@'),
    }
}

/// A blocker the engine cannot place a modifier on exactly.
pub(crate) fn unplaceable(site: Site) -> Declared {
    Declared {
        path: site.path.clone(),
        site,
        modifier: None,
        at: 0,
        from: String::new(),
        placeable: false,
    }
}

/// A widening decided for the plan.
#[derive(Clone, Debug)]
pub(crate) struct Widening {
    pub path: String,
    pub line: u32,
    pub from: String,
    pub to: Level,
    pub at: Range<usize>,
    pub in_moved: bool,
    pub item: bool,
}

impl Widening {
    /// The replacement text: the level, plus a space when it is inserted.
    pub(crate) fn text(&self) -> String {
        if self.at.is_empty() {
            format!("{} ", self.to.as_str())
        } else {
            self.to.as_str().to_string()
        }
    }
}

/// The caller's allowance and the widenings accepted so far.
pub(crate) struct Widener {
    allowance: Option<Level>,
    pub applied: Vec<Widening>,
}

impl Widener {
    pub(crate) fn new(allowance: Option<Level>) -> Self {
        Self {
            allowance,
            applied: Vec::new(),
        }
    }

    pub(crate) fn allowed(&self) -> bool {
        self.allowance.is_some()
    }

    /// Accept the fixes when every one is placeable and within the allowance;
    /// otherwise return the refusal with the blocking sites, naming the level needed.
    /// `fixes` is `None` when some blocker has no exact fix.
    pub(crate) fn settle(
        &mut self,
        details: String,
        sites: Vec<Site>,
        fixes: Option<Vec<Fix>>,
    ) -> Option<Outcome> {
        let Some(fixes) = fixes else {
            return Some(refuse_with_sites("visibility_required", details, sites));
        };
        let needs: Vec<(Fix, Option<Level>)> = fixes
            .into_iter()
            .map(|fix| {
                let needed = fix.needed();
                (fix, needed)
            })
            .collect();
        let required = needs.iter().filter_map(|(_, level)| *level).max();
        let within = match (required, self.allowance) {
            (None, Some(_)) => true,
            (Some(required), Some(allowed)) => required <= allowed,
            _ => false,
        };
        if !within {
            let details = match required {
                Some(required) => format!(
                    "{details}; the narrowest fix is `{}` (pass `allow_visibility_widening` to allow it)",
                    required.as_str()
                ),
                None => details,
            };
            let mut outcome = refuse_with_sites("visibility_required", details, sites);
            outcome.response.required_visibility = required.map(|level| level.as_str().to_string());
            return Some(outcome);
        }
        for (fix, needed) in needs {
            let Some(to) = needed else { continue };
            // A "widening" that rewrites the modifier to itself would claim
            // a fix that fixes nothing.
            if fix.from == to.as_str() {
                return Some(refuse_with_sites("visibility_required", details, sites));
            }
            self.applied.push(Widening {
                path: fix.path,
                line: fix.line,
                from: fix.from,
                to,
                at: fix.at,
                in_moved: fix.in_moved,
                item: fix.item,
            });
        }
        None
    }

    /// Rewrites inside the moved item's text, as absolute source ranges.
    pub(crate) fn moved_rewrites(&self) -> Vec<(Range<usize>, String)> {
        self.applied
            .iter()
            .filter(|w| w.in_moved)
            .map(|w| (w.at.clone(), w.text()))
            .collect()
    }

    /// Whether the moved item itself was widened.
    pub(crate) fn widened_item(&self) -> bool {
        self.applied.iter().any(|w| w.item)
    }
}
