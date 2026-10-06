//! Region data flow for `extract_function`: inputs, outputs, and the jumps
//! that would leave the region.

use super::*;

/// Inputs, outputs, and escapes of one target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Flow {
    pub(super) inputs: Vec<String>,
    pub(super) outputs: Vec<String>,
    /// Outputs the region declares (`let`, `const`); the rest it assigns.
    pub(super) declared: Vec<String>,
    /// Declared outputs bound `mut` (Rust).
    pub(super) mutable: Vec<String>,
    /// `const`, `let`, or `var` for declared outputs (ECMAScript).
    pub(super) keyword: &'static str,
    /// Inputs bound by `let mut` outside the region (Rust).
    pub(super) mut_locals: Vec<String>,
    pub(super) escapes: Vec<Site>,
    /// Why the region only works inside its class, if it does.
    pub(super) class_bound: Option<String>,
}

pub(super) fn flow(
    dialect: Dialect,
    language: Language,
    source: &str,
    scope: Node<'_>,
    target: &Target<'_>,
) -> Flow {
    let syn = dialect.syntax();
    let (inner_start, inner_end) = match target.closure_body() {
        Some(body) => (body.start_byte(), body.end_byte()),
        None => (target.outer_start(), target.outer_end()),
    };
    let inside = |byte: usize| byte >= inner_start && byte < inner_end;
    let units = target.units(syn);
    let unit_of = |byte: usize| {
        units
            .iter()
            .position(|u| byte >= u.start_byte() && byte < u.end_byte())
            .unwrap_or(0)
    };

    let scan = scan_names(scope, source, language);
    let defined: HashSet<String> = scan.as_ref().map(|s| s.defined.clone()).unwrap_or_default();
    let reads: HashSet<(u32, u32)> = scan
        .as_ref()
        .map(|s| s.references.iter().map(|r| (r.row, r.column)).collect())
        .unwrap_or_default();

    // Every local occurrence, classified.
    let own_name = scope.child_by_field_name("name").map(|n| n.id());
    let mut events: Vec<Event> = Vec::new();
    walk(scope, &mut |node| {
        if !syn.locals.contains(&node.kind()) || Some(node.id()) == own_name {
            return;
        }
        let name = text(node, source);
        if !defined.contains(name) {
            return;
        }
        let point = node.start_position();
        let byte = node.start_byte();
        let assigned = is_assignment_target(node, syn);
        let kind = if reads.contains(&(point.row as u32, point.column as u32)) {
            if assigned.is_some() {
                EventKind::Assign {
                    reads: assigned == Some(true),
                }
            } else {
                EventKind::Read
            }
        } else if is_binding_site(node, dialect) {
            EventKind::Bind
        } else {
            return;
        };
        events.push(Event {
            name: name.to_string(),
            byte,
            kind,
            node_id: node.id(),
        });
    });

    // Declared-in-region names are visible only to the end of their block
    // in brace languages; Python locals live to the end of the function.
    let block_end = if dialect.is_brace() {
        target.nodes[0]
            .parent()
            .filter(|_| matches!(target.shape, Shape::Statements { .. }))
            .map_or(scope.end_byte(), |b| b.end_byte())
    } else {
        scope.end_byte()
    };

    let mut inputs: Vec<String> = Vec::new();
    let mut outputs: Vec<String> = Vec::new();
    let mut declared: Vec<String> = Vec::new();
    let mut mutable: Vec<String> = Vec::new();
    let mut mut_locals: Vec<String> = Vec::new();
    let mut keyword = "const";

    let mut names: Vec<&str> = Vec::new();
    for event in &events {
        if inside(event.byte) && !names.contains(&event.name.as_str()) {
            names.push(event.name.as_str());
        }
    }
    // Closure parameters lead the input list in declaration order.
    if let Some(closure) = (target.shape == Shape::ClosureBody).then(|| target.nodes[0]) {
        if let Some(params) = closure
            .child_by_field_name("parameters")
            .or_else(|| closure.child_by_field_name("parameter"))
        {
            let mut ordered: Vec<&str> = Vec::new();
            walk(params, &mut |node| {
                if syn.locals.contains(&node.kind()) && is_binding_site(node, dialect) {
                    let name = text(node, source);
                    if names.contains(&name) && !ordered.contains(&name) {
                        ordered.push(name);
                    }
                }
            });
            names.retain(|n| !ordered.contains(n));
            ordered.extend(names);
            names = ordered;
        }
    }

    for name in names {
        let of_name = || events.iter().filter(move |e| e.name == name);
        let binds_outside = of_name().any(|e| e.kind == EventKind::Bind && !inside(e.byte));
        let first_read = of_name()
            .filter(|e| inside(e.byte) && e.reads())
            .map(|e| e.byte)
            .min();
        if let Some(first_read) = first_read {
            let shadowed_before = of_name().any(|e| {
                inside(e.byte) && e.kind == EventKind::Bind && unit_of(e.byte) < unit_of(first_read)
            });
            if binds_outside && !shadowed_before {
                inputs.push(name.to_string());
                if dialect == Dialect::Rust
                    && of_name().any(|e| {
                        e.kind == EventKind::Bind && !inside(e.byte) && is_let_mut(e, scope, source)
                    })
                {
                    mut_locals.push(name.to_string());
                }
            }
        }
        if target.shape == Shape::ClosureBody {
            continue;
        }
        let bound_inside: Vec<&Event> = of_name()
            .filter(|e| inside(e.byte) && e.kind == EventKind::Bind)
            .collect();
        let assigned_inside =
            of_name().any(|e| inside(e.byte) && matches!(e.kind, EventKind::Assign { .. }));
        if bound_inside.is_empty() && !assigned_inside {
            continue;
        }
        let horizon = if bound_inside.is_empty() {
            scope.end_byte()
        } else {
            block_end
        };
        let read_after = of_name().any(|e| e.byte >= inner_end && e.byte < horizon && e.reads());
        if !read_after {
            continue;
        }
        outputs.push(name.to_string());
        if !bound_inside.is_empty() {
            declared.push(name.to_string());
            // `mut` survives the call only if the caller still assigns it.
            let assigned_after = of_name().any(|e| {
                e.byte >= inner_end
                    && e.byte < horizon
                    && matches!(e.kind, EventKind::Assign { .. })
            });
            if assigned_after && bound_inside.iter().any(|e| is_let_mut(e, scope, source)) {
                mutable.push(name.to_string());
            }
            if let Some(kw) = bound_inside
                .iter()
                .find_map(|e| declaration_keyword(e, scope, source))
            {
                if kw != "const" {
                    keyword = "let";
                }
            }
        }
    }

    let mut escapes = Vec::new();
    let mut class_bound = None;
    let roots: Vec<Node<'_>> = match target.closure_body() {
        Some(body) => vec![body],
        None => target.nodes.clone(),
    };
    for node in roots {
        find_escapes(
            node,
            target.shape,
            syn,
            source,
            0,
            &mut escapes,
            &mut class_bound,
        );
    }

    Flow {
        inputs,
        outputs,
        declared,
        mutable,
        keyword,
        mut_locals,
        escapes,
        class_bound,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EventKind {
    Read,
    Bind,
    /// The left side of an assignment; compound forms also read.
    Assign {
        reads: bool,
    },
}

struct Event {
    name: String,
    byte: usize,
    kind: EventKind,
    node_id: usize,
}

impl Event {
    fn reads(&self) -> bool {
        matches!(
            self.kind,
            EventKind::Read | EventKind::Assign { reads: true }
        )
    }
}

/// `Some(reads)` when `node` is the target of an assignment; compound
/// assignment and increments also read the old value.
fn is_assignment_target(node: Node<'_>, syn: &Syntax) -> Option<bool> {
    let parent = node.parent()?;
    syn.assignments.iter().find_map(|(kind, field)| {
        (parent.kind() == *kind
            && parent.child_by_field_name(field).map(|c| c.id()) == Some(node.id()))
        .then(|| *kind != "assignment_expression")
    })
}

/// Whether an identifier the profile did not count as a read binds a name,
/// rather than naming a member, keyword argument, path segment, or macro.
fn is_binding_site(node: Node<'_>, dialect: Dialect) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let field_is =
        |field: &str| parent.child_by_field_name(field).map(|c| c.id()) == Some(node.id());
    match dialect {
        Dialect::Python => {
            !(parent.kind() == "attribute" && field_is("attribute")
                || parent.kind() == "keyword_argument" && field_is("name"))
        }
        Dialect::Rust => {
            let pattern_type = matches!(parent.kind(), "tuple_struct_pattern" | "struct_pattern")
                && field_is("type");
            let qualified = matches!(
                parent.kind(),
                "scoped_identifier" | "macro_invocation" | "token_tree" | "label"
            );
            !(qualified || pattern_type)
        }
        Dialect::TypeScript | Dialect::JavaScript => {
            !matches!(parent.kind(), "import_specifier" | "export_specifier")
        }
    }
}

fn is_let_mut(event: &Event, scope: Node<'_>, source: &str) -> bool {
    let Some(node) = find_by_id(scope, event.node_id) else {
        return false;
    };
    let mut current = node.parent();
    let mut in_let = false;
    while let Some(n) = current {
        if n.kind() == "let_declaration" || n.kind() == "parameter" {
            in_let = n.kind() == "let_declaration";
            break;
        }
        current = n.parent();
    }
    in_let && slice(source, 0, event.byte).trim_end().ends_with("mut")
}

fn declaration_keyword(event: &Event, scope: Node<'_>, source: &str) -> Option<&'static str> {
    let node = find_by_id(scope, event.node_id)?;
    let mut current = node.parent();
    while let Some(n) = current {
        if matches!(n.kind(), "lexical_declaration" | "variable_declaration") {
            let first = text(n, source).split_whitespace().next()?;
            return Some(match first {
                "let" => "let",
                "var" => "var",
                _ => "const",
            });
        }
        current = n.parent();
    }
    None
}

fn find_by_id(root: Node<'_>, id: usize) -> Option<Node<'_>> {
    let mut found = None;
    walk(root, &mut |node| {
        if found.is_none() && node.id() == id {
            found = Some(node);
        }
    });
    found
}

/// Jumps that leave the region. `loops` counts loops (and switches, for
/// `break`) opened inside it.
fn find_escapes(
    node: Node<'_>,
    shape: Shape,
    syn: &Syntax,
    source: &str,
    loops: usize,
    escapes: &mut Vec<Site>,
    class_bound: &mut Option<String>,
) {
    let kind = node.kind();
    if class_bound.is_none() {
        *class_bound = class_binding(node, kind, syn, source);
    }
    let mut site = |label: &'static str, reason: &str| {
        escapes.push(Site {
            line: node.start_position().row + 1,
            kind: label,
            reason: reason.to_string(),
            text: first_line(text(node, source)),
        });
    };
    if syn.returns.contains(&kind) && shape != Shape::ClosureBody {
        site("return", "returns from the enclosing function");
    } else if syn.tries.contains(&kind) && shape != Shape::ClosureBody {
        site("try", "`?` returns early from the enclosing function");
    } else if syn.suspends.contains(&kind) {
        site("suspend", "suspends the enclosing function");
    } else if syn.breaks.contains(&kind) || syn.continues.contains(&kind) {
        let labeled = named_children(node)
            .iter()
            .any(|c| matches!(c.kind(), "label" | "statement_identifier" | "lifetime"));
        let targetable = if syn.breaks.contains(&kind) {
            loops > 0
        } else {
            loops > 0 && !syn.loops.is_empty()
        };
        if labeled || !targetable {
            site(
                if syn.breaks.contains(&kind) {
                    "break"
                } else {
                    "continue"
                },
                "jumps to a loop outside the region",
            );
        }
    }
    let mut depth = loops;
    if syn.loops.contains(&kind) || syn.switches.contains(&kind) {
        depth += 1;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if syn.barriers.contains(&child.kind()) {
            // A nested function owns its own jumps; only a receiver read
            // inside a closure still reaches the enclosing method.
            let mut ignored = Vec::new();
            find_escapes(
                child,
                Shape::ClosureBody,
                syn,
                source,
                usize::MAX / 2,
                &mut ignored,
                class_bound,
            );
            continue;
        }
        // `continue` targets loops only, never a switch.
        let child_depth = if syn.switches.contains(&kind) && syn.continues.contains(&child.kind()) {
            loops
        } else {
            depth
        };
        find_escapes(child, shape, syn, source, child_depth, escapes, class_bound);
    }
}

pub(super) fn returns_value(target: &Target<'_>, flow: &Flow, syn: &Syntax) -> bool {
    match target.shape {
        Shape::Expression => true,
        Shape::Statements { tail } => tail || !flow.outputs.is_empty(),
        Shape::ClosureBody => match target.closure_body() {
            Some(body) if syn.blocks.contains(&body.kind()) => {
                if syn.blocks == RUST.blocks {
                    named_children(body)
                        .into_iter()
                        .rfind(|n| !syn.comments.contains(&n.kind()))
                        .is_some_and(|n| !is_rust_statement(n.kind()))
                } else {
                    let mut found = false;
                    let mut stack = vec![body];
                    while let Some(n) = stack.pop() {
                        if syn.returns.contains(&n.kind()) && n.named_child_count() > 0 {
                            found = true;
                        }
                        for child in named_children(n) {
                            if !syn.barriers.contains(&child.kind()) {
                                stack.push(child);
                            }
                        }
                    }
                    found
                }
            }
            _ => true,
        },
    }
}

/// A construct that works only inside the enclosing class, so a module-level
/// helper cannot run it. Rust `self` and ECMAScript `this`/`super` are
/// keywords no free function can bind under that name. Python's receiver is
/// an ordinary parameter and is passed like any other input; only zero-argument
/// `super()` (it reads the class cell) and `__name` (mangled to `_Class__name`
/// in the class body only) are bound to the class.
fn class_binding(node: Node<'_>, kind: &str, syn: &Syntax, source: &str) -> Option<String> {
    if syn.receivers.contains(&kind) {
        return Some(format!(
            "the region reads the method receiver `{kind}`, which a free function cannot bind"
        ));
    }
    if syn.modules != PYTHON.modules {
        return None;
    }
    if kind == "call"
        && node
            .child_by_field_name("function")
            .is_some_and(|f| f.kind() == "identifier" && text(f, source) == "super")
        && node
            .child_by_field_name("arguments")
            .is_some_and(|a| a.named_child_count() == 0)
    {
        return Some("zero-argument `super()` only works inside the class body".into());
    }
    let name = text(node, source);
    (kind == "identifier" && name.starts_with("__") && !name.ends_with("__")).then(|| {
        format!("`{name}` is name-mangled inside the class body, so a module-level helper would read a different attribute")
    })
}

pub(super) fn same_flow(copy: &Flow, primary: &Flow) -> bool {
    let set = |v: &[String]| v.iter().cloned().collect::<HashSet<_>>();
    let extractable = copy.escapes.is_empty() && copy.class_bound.is_none();
    extractable
        && set(&copy.inputs) == set(&primary.inputs)
        && set(&copy.outputs) == set(&primary.outputs)
        && set(&copy.declared) == set(&primary.declared)
        && set(&copy.mutable) == set(&primary.mutable)
}
