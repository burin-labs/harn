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

use harn_vm::value::DictMap;
use tree_sitter::Tree;

use crate::error::HostlibError;
use crate::tools::args::optional_string;

use super::model::{Family, Lang, ModuleRef};
use super::response::{refuse_with_sites, Outcome, Site};
use super::{rust, text};

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

    /// The `allow_visibility_widening` request parameter, when given.
    pub(crate) fn from_request(
        builtin: &'static str,
        dict: &DictMap,
    ) -> Result<Option<Self>, HostlibError> {
        optional_string(builtin, dict, "allow_visibility_widening")?
            .map(|raw| {
                Self::parse(&raw).ok_or_else(|| HostlibError::InvalidParameter {
                    builtin,
                    param: "allow_visibility_widening",
                    message: format!("expected {}, got `{raw}`", Self::ACCEPTED),
                })
            })
            .transpose()
    }

    /// Reject a level the source language does not have, rather than ignore it.
    pub(crate) fn require_for(
        self,
        builtin: &'static str,
        family: Family,
        language: &str,
    ) -> Result<(), HostlibError> {
        let (fits, has) = match family {
            Family::Rust => (self != Self::Export, "`pub(super)`, `pub(crate)` and `pub`"),
            Family::Script => (self == Self::Export, "`export`"),
            Family::Python => (false, "no visibility levels"),
        };
        if fits {
            return Ok(());
        }
        Err(HostlibError::InvalidParameter {
            builtin,
            param: "allow_visibility_widening",
            message: format!(
                "`{}` does not apply to {language} (it has {has})",
                self.as_str()
            ),
        })
    }
}

/// Whether a Rust declaration restricted to `restriction` (`super`,
/// `crate::a`, ...) and living in `home` is visible from `viewer`.
pub(crate) fn sees(restriction: &str, home: &ModuleRef, viewer: &ModuleRef) -> bool {
    let (
        ModuleRef::Rust {
            crate_dir: home_crate,
            path: home_path,
        },
        ModuleRef::Rust {
            crate_dir: viewer_crate,
            path: viewer_path,
        },
    ) = (home, viewer)
    else {
        return false;
    };
    home_crate == viewer_crate
        && rust::visible_from(home_path, restriction)
            .is_some_and(|region| viewer_path.starts_with(&region))
}

/// The source file a move starts from.
#[derive(Clone, Copy)]
pub(crate) struct Origin<'a> {
    pub rel: &'a str,
    pub source: &'a str,
    pub tree: &'a Tree,
}

/// The declaration spanning `range`, as a blocker whose visibility the move
/// may widen; unplaceable when the engine cannot find or mark it exactly.
pub(crate) fn blocker(
    lang: &Lang,
    origin: &Origin<'_>,
    range: &Range<usize>,
    reason: String,
) -> Declared {
    let Origin { rel, source, tree } = *origin;
    let declared = match lang {
        Lang::Rust(_) => rust::declared_item(rel, source, tree, range, reason.clone()),
        _ => Some(unexported(rel, source, range, reason.clone())),
    };
    declared
        .unwrap_or_else(|| unplaceable(Site::at(rel, source, range.start, "declaration", reason)))
}

/// The fix for a private moved item that other modules use: it lands in the
/// destination and must be visible from each of them. A user outside the
/// library's module tree has no module to reason about, so that only refuses.
pub(crate) fn item_fix(
    lang: &mut Lang,
    declared: Declared,
    outside_users: &[Site],
    dest_module: &ModuleRef,
    source: &str,
) -> Option<Vec<Fix>> {
    let reach = match lang {
        Lang::Rust(_) => {
            let mut viewers = Vec::new();
            for site in outside_users {
                viewers.push(lang.module_of(&site.path)?);
            }
            Reach::Rust {
                home: dest_module.clone(),
                viewers,
            }
        }
        _ => Reach::Export,
    };
    declared.fix(source, reach, true, true).map(|fix| vec![fix])
}

/// The fixes for fields other modules read: each plainly read field is
/// widened for the modules that read it, keeping the modules that saw it
/// before. A constructor or pattern needs every field, so those readers only
/// refuse. `regions` pairs each hidden field with where it was and is visible;
/// `origin` is the source crate, file and text.
pub(crate) fn reader_fixes(
    ws: &mut rust::Workspace,
    readers: &[(Site, Option<String>)],
    hidden_fields: &[rust::HiddenField],
    regions: &[(String, Vec<String>, Vec<String>)],
    origin: (&str, &str, &str),
    dest_module: &ModuleRef,
) -> Option<Vec<Fix>> {
    let (crate_dir, path, source) = origin;
    let mut names: Vec<&str> = Vec::new();
    for (_, field) in readers {
        let field = field.as_deref()?;
        if !names.contains(&field) {
            names.push(field);
        }
    }
    let mut fixes = Vec::new();
    for field in names {
        let hidden = hidden_fields.iter().find(|h| h.name == field)?;
        let (_, _, after) = regions.iter().find(|(f, ..)| f.as_str() == field)?;
        let mut viewers = vec![ModuleRef::Rust {
            crate_dir: crate_dir.to_string(),
            path: after.clone(),
        }];
        for (site, read) in readers {
            if read.as_deref() == Some(field) {
                viewers.push(ws.module_of(&site.path)?);
            }
        }
        let from = hidden.modifier.as_ref().map_or("private".to_string(), |m| {
            text::slice(source, m.clone()).replace(' ', "")
        });
        if from.starts_with("pub(in") {
            return None;
        }
        let at = hidden.modifier.clone().unwrap_or(hidden.at..hidden.at);
        fixes.push(Fix {
            path: path.to_string(),
            line: text::line_of(source, at.start),
            at,
            from,
            reach: Reach::Rust {
                home: dest_module.clone(),
                viewers,
            },
            in_moved: true,
            item: false,
        });
    }
    Some(fixes)
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

impl Reach {
    /// A source declaration the moved item uses: it stays in the source
    /// module and the destination must see it.
    pub(crate) fn of_blocked(family: Family, source: &ModuleRef, dest: &ModuleRef) -> Self {
        match family {
            Family::Rust => Self::Rust {
                home: source.clone(),
                viewers: vec![dest.clone()],
            },
            _ => Self::Export,
        }
    }
}

/// The tail of the move's `details` when it widened anything.
pub(crate) fn summary(widened: &[Widening]) -> String {
    if widened.is_empty() {
        String::new()
    } else {
        format!("; widened {} declaration(s), see `widened`", widened.len())
    }
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
    /// The module path a Rust `pub(super)` or `pub(in ...)` modifier names
    /// (`self` when private); the widened level must still reach it.
    pub restriction: Option<String>,
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
        let mut reach = reach;
        if let (Reach::Rust { home, viewers }, Some(path), false) =
            (&mut reach, &self.restriction, item)
        {
            // The declaration stays where it is, so modules that saw it keep seeing it.
            let region = match home {
                ModuleRef::Rust {
                    crate_dir,
                    path: at_home,
                } => rust::visible_from(at_home, path).map(|region| (crate_dir.clone(), region)),
                _ => None,
            };
            if let Some((crate_dir, region)) = region {
                viewers.push(ModuleRef::Rust {
                    crate_dir,
                    path: region,
                });
            }
        }
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
        restriction: None,
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
        restriction: None,
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

    /// The widest level the moved item was widened to, if any.
    fn item_level(&self) -> Option<Level> {
        self.applied.iter().filter(|w| w.item).map(|w| w.to).max()
    }

    /// The visibility of the `mod` line a new destination module needs. A
    /// widened item is reached through its module, so the module is only as
    /// visible as that item's level requires: a `pub(super)` item is seen
    /// from the module's parent, where a private `mod` already is.
    pub(crate) fn module_visibility(&self, exported: bool) -> &'static str {
        if exported {
            return "pub";
        }
        match self.item_level() {
            Some(Level::Crate) => "pub(crate)",
            Some(Level::Public) => "pub",
            _ => "",
        }
    }

    /// The report entry for a new module declaration that is visible beyond
    /// the default private `mod`, because the widened item needs it.
    pub(crate) fn module_widening(
        &self,
        exported: bool,
        parent: &str,
        line: u32,
    ) -> Option<Widening> {
        let to = self.item_level().filter(|_| !exported)?;
        (!self.module_visibility(false).is_empty()).then(|| Widening {
            path: parent.to_string(),
            line,
            from: "private".to_string(),
            to,
            at: 0..0,
            in_moved: false,
            item: false,
        })
    }
}

/// Whether every user is inside the region a restricted item (`pub(super)`,
/// `pub(in ...)`) reaches once it lives in `home`.
pub(crate) fn users_see(
    lang: &mut Lang,
    restriction: Option<&str>,
    home: &ModuleRef,
    users: &[Site],
) -> bool {
    restriction.is_some_and(|path| {
        users.iter().all(|site| {
            lang.module_of(&site.path)
                .is_some_and(|m| sees(path, home, &m))
        })
    })
}
