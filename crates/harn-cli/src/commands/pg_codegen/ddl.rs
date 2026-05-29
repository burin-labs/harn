//! A focused parser for the DDL subset that appears in Postgres migration
//! files. It tracks the live shape of every table across an ordered sequence
//! of migrations so a downstream emitter can render one Harn record type per
//! table that mirrors the schema state on disk.
//!
//! This is deliberately *not* a general SQL parser. It understands only the
//! statements that change a table's column set — `CREATE TABLE`,
//! `ALTER TABLE`, and `DROP TABLE` — and silently ignores everything else
//! (indexes, functions, `INSERT`s, `CREATE TYPE`, …). Sizes and precisions
//! (`varchar(255)`, `numeric(10, 2)`) are accepted and discarded; the runtime
//! row decode never distinguishes them.

use std::collections::BTreeMap;

/// A single column's source-level type, normalized to a lowercase base name
/// plus a count of trailing array dimensions. Sizes/precisions are dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SqlType {
    /// Lowercase canonical base name, e.g. `integer`, `timestamp with time zone`.
    pub(crate) base: String,
    /// Number of `[]` suffixes; 0 for a scalar column.
    pub(crate) array_dims: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Column {
    pub(crate) name: String,
    pub(crate) sql_type: SqlType,
    /// `true` when the column can never be SQL NULL (NOT NULL, PRIMARY KEY,
    /// or a `serial`/identity column).
    pub(crate) not_null: bool,
}

/// A table's column set, in definition order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Table {
    pub(crate) columns: Vec<Column>,
}

impl Table {
    fn column_mut(&mut self, name: &str) -> Option<&mut Column> {
        self.columns.iter_mut().find(|col| col.name == name)
    }

    fn remove_column(&mut self, name: &str) {
        self.columns.retain(|col| col.name != name);
    }
}

/// The accumulated schema after replaying every migration in order. Tables are
/// keyed by their unqualified name; emit order is the caller's concern.
#[derive(Debug, Clone, Default)]
pub(crate) struct Schema {
    tables: BTreeMap<String, Table>,
}

impl Schema {
    /// Tables in deterministic (name-sorted) order, skipping any that have no
    /// columns left — an all-columns-dropped table has no record to emit.
    pub(crate) fn tables(&self) -> impl Iterator<Item = (&str, &Table)> {
        self.tables
            .iter()
            .filter(|(_, table)| !table.columns.is_empty())
            .map(|(name, table)| (name.as_str(), table))
    }

    fn rename_table(&mut self, from: &str, to: &str) {
        if let Some(table) = self.tables.remove(from) {
            self.tables.insert(to.to_string(), table);
        }
    }
}

/// Replay an ordered list of migration SQL sources into a single schema.
///
/// Infallible by design: any statement (or fragment) this focused parser does
/// not recognize is silently skipped rather than rejected. The migrations have
/// already been accepted by Postgres, so the goal is to extract the table
/// shapes we *can* read, not to re-validate the SQL.
pub(crate) fn parse_migrations(sources: &[String]) -> Schema {
    let mut schema = Schema::default();
    for source in sources {
        for statement in split_statements(source) {
            apply_statement(&mut schema, &statement);
        }
    }
    schema
}

// --- Tokenizer ---------------------------------------------------------------

/// A statement is a flat list of these tokens. String/dollar-quoted literals
/// collapse to a single placeholder since DDL column shapes never depend on a
/// literal's contents.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// An identifier or keyword. Stored verbatim; callers compare
    /// case-insensitively for keywords and strip quotes for quoted idents.
    Word(String),
    LParen,
    RParen,
    Comma,
    LBracket,
    RBracket,
    Dot,
    /// A string, quoted-identifier, or dollar-quoted literal, contents elided.
    Literal,
    /// Any other punctuation (`=`, `::`, operators in CHECK clauses, …).
    Other,
}

/// Split a source file into statements, each already tokenized. Comments and
/// literal contents are stripped during tokenization so the splitter only ever
/// sees structural tokens.
fn split_statements(source: &str) -> Vec<Vec<Token>> {
    let chars: Vec<char> = source.chars().collect();
    let mut statements = Vec::new();
    let mut current = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        match c {
            _ if c.is_whitespace() => i += 1,
            '-' if chars.get(i + 1) == Some(&'-') => i = skip_line_comment(&chars, i),
            '/' if chars.get(i + 1) == Some(&'*') => i = skip_block_comment(&chars, i),
            '\'' => {
                i = skip_quoted(&chars, i, '\'');
                current.push(Token::Literal);
            }
            '"' => {
                let (ident, next) = read_quoted(&chars, i, '"');
                current.push(Token::Word(ident));
                i = next;
            }
            '$' if is_dollar_quote_start(&chars, i) => {
                i = skip_dollar_quoted(&chars, i);
                current.push(Token::Literal);
            }
            ';' => {
                i += 1;
                if !current.is_empty() {
                    statements.push(std::mem::take(&mut current));
                }
            }
            '(' => push_punct(&mut current, &mut i, Token::LParen),
            ')' => push_punct(&mut current, &mut i, Token::RParen),
            ',' => push_punct(&mut current, &mut i, Token::Comma),
            '[' => push_punct(&mut current, &mut i, Token::LBracket),
            ']' => push_punct(&mut current, &mut i, Token::RBracket),
            '.' => push_punct(&mut current, &mut i, Token::Dot),
            _ if c.is_alphanumeric() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                current.push(Token::Word(chars[start..i].iter().collect()));
            }
            _ => push_punct(&mut current, &mut i, Token::Other),
        }
    }

    if !current.is_empty() {
        statements.push(current);
    }
    statements
}

fn push_punct(current: &mut Vec<Token>, i: &mut usize, token: Token) {
    current.push(token);
    *i += 1;
}

/// Index just past the end of a `--` line comment starting at `start`.
fn skip_line_comment(chars: &[char], start: usize) -> usize {
    let mut i = start;
    while i < chars.len() && chars[i] != '\n' {
        i += 1;
    }
    i
}

/// Index just past the closing `*/` of a block comment starting at `start`.
fn skip_block_comment(chars: &[char], start: usize) -> usize {
    let mut i = start + 2;
    while i + 1 < chars.len() {
        if chars[i] == '*' && chars[i + 1] == '/' {
            return i + 2;
        }
        i += 1;
    }
    chars.len()
}

/// Read a `quote`-delimited identifier starting at `start`, returning its
/// unescaped contents and the index past the closing quote. Postgres escapes
/// an embedded delimiter by doubling it (`""`).
fn read_quoted(chars: &[char], start: usize, quote: char) -> (String, usize) {
    let mut out = String::new();
    let mut i = start + 1; // past opening quote
    while i < chars.len() {
        if chars[i] == quote {
            if chars.get(i + 1) == Some(&quote) {
                out.push(quote);
                i += 2;
            } else {
                i += 1;
                break;
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    (out, i)
}

/// Index just past a `quote`-delimited run starting at `start`, discarding the
/// contents (used for string literals, whose value never affects a row shape).
fn skip_quoted(chars: &[char], start: usize, quote: char) -> usize {
    read_quoted(chars, start, quote).1
}

/// Whether `chars[start]` begins a `$tag$` dollar-quote opener.
fn is_dollar_quote_start(chars: &[char], start: usize) -> bool {
    let mut i = start + 1; // past opening '$'
    while i < chars.len() {
        match chars[i] {
            '$' => return true,
            c if c.is_alphanumeric() || c == '_' => i += 1,
            _ => return false,
        }
    }
    false
}

/// Index just past a dollar-quoted literal (`$tag$ … $tag$`) starting at
/// `start`. Reads the opening tag, then scans for its matching close.
fn skip_dollar_quoted(chars: &[char], start: usize) -> usize {
    let mut i = start + 1; // past opening '$'
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c == '$' {
            break;
        }
    }
    let tag = &chars[start..i];
    while i + tag.len() <= chars.len() {
        if &chars[i..i + tag.len()] == tag {
            return i + tag.len();
        }
        i += 1;
    }
    chars.len()
}

// --- Statement application ---------------------------------------------------

fn keyword_eq(token: &Token, keyword: &str) -> bool {
    matches!(token, Token::Word(w) if w.eq_ignore_ascii_case(keyword))
}

fn apply_statement(schema: &mut Schema, tokens: &[Token]) {
    if tokens.len() < 2 {
        return;
    }
    if keyword_eq(&tokens[0], "create") && keyword_eq(&tokens[1], "table") {
        apply_create_table(schema, &tokens[2..]);
    } else if keyword_eq(&tokens[0], "alter") && keyword_eq(&tokens[1], "table") {
        apply_alter_table(schema, &tokens[2..]);
    } else if keyword_eq(&tokens[0], "drop") && keyword_eq(&tokens[1], "table") {
        apply_drop_table(schema, &tokens[2..]);
    }
}

/// Read an (optionally schema-qualified, optionally quoted) table name, leaving
/// the cursor just past it. Returns the unqualified base name.
fn read_table_name(tokens: &[Token], cursor: &mut usize) -> Option<String> {
    let mut name = match tokens.get(*cursor) {
        Some(Token::Word(w)) => w.clone(),
        _ => return None,
    };
    *cursor += 1;
    // `schema.table` — keep only the final component.
    while matches!(tokens.get(*cursor), Some(Token::Dot)) {
        *cursor += 1;
        match tokens.get(*cursor) {
            Some(Token::Word(w)) => {
                name = w.clone();
                *cursor += 1;
            }
            _ => break,
        }
    }
    Some(name)
}

/// Skip leading `IF NOT EXISTS` / `IF EXISTS` / `ONLY` modifiers.
fn skip_existence_modifiers(tokens: &[Token], cursor: &mut usize) {
    loop {
        match tokens.get(*cursor) {
            Some(t) if keyword_eq(t, "if") => {
                *cursor += 1;
                if matches!(tokens.get(*cursor), Some(t) if keyword_eq(t, "not")) {
                    *cursor += 1;
                }
                if matches!(tokens.get(*cursor), Some(t) if keyword_eq(t, "exists")) {
                    *cursor += 1;
                }
            }
            Some(t) if keyword_eq(t, "only") => *cursor += 1,
            _ => break,
        }
    }
}

fn apply_create_table(schema: &mut Schema, tokens: &[Token]) {
    let mut cursor = 0;
    skip_existence_modifiers(tokens, &mut cursor);
    let if_not_exists = tokens.iter().take(cursor).any(|t| keyword_eq(t, "exists"));
    let Some(name) = read_table_name(tokens, &mut cursor) else {
        return;
    };
    if if_not_exists && schema.tables.contains_key(&name) {
        return;
    }
    // The column list lives inside the outermost parentheses.
    let Some(body) = paren_body(&tokens[cursor..]) else {
        // `CREATE TABLE x (LIKE y)` and `... AS SELECT` have no literal column
        // list we can read; leave the table absent rather than guess.
        return;
    };

    let mut table = Table::default();
    let mut primary_key_cols: Vec<String> = Vec::new();
    for item in split_top_level_commas(body) {
        if item.is_empty() {
            continue;
        }
        if let Some(cols) = table_level_primary_key(item) {
            primary_key_cols.extend(cols);
            continue;
        }
        if is_table_constraint(item) {
            continue;
        }
        if let Some(column) = parse_column_def(item) {
            table.columns.push(column);
        }
    }
    for pk in primary_key_cols {
        if let Some(col) = table.column_mut(&pk) {
            col.not_null = true;
        }
    }

    schema.tables.insert(name, table);
}

fn apply_alter_table(schema: &mut Schema, tokens: &[Token]) {
    let mut cursor = 0;
    skip_existence_modifiers(tokens, &mut cursor);
    let Some(name) = read_table_name(tokens, &mut cursor) else {
        return;
    };
    let rest = &tokens[cursor..];

    // `RENAME` is its own statement form and never comma-combines.
    if matches!(rest.first(), Some(t) if keyword_eq(t, "rename")) {
        apply_alter_rename(schema, &name, &rest[1..]);
        return;
    }

    for action in split_top_level_commas(rest) {
        apply_alter_action(schema, &name, action);
    }
}

fn apply_alter_rename(schema: &mut Schema, table_name: &str, tokens: &[Token]) {
    // `RENAME TO new_table`
    if matches!(tokens.first(), Some(t) if keyword_eq(t, "to")) {
        if let Some(Token::Word(new_name)) = tokens.get(1) {
            schema.rename_table(table_name, new_name);
        }
        return;
    }
    // `RENAME [COLUMN] old TO new`
    let mut idx = 0;
    if matches!(tokens.get(idx), Some(t) if keyword_eq(t, "column")) {
        idx += 1;
    }
    let old = match tokens.get(idx) {
        Some(Token::Word(w)) => w.clone(),
        _ => return,
    };
    idx += 1;
    if !matches!(tokens.get(idx), Some(t) if keyword_eq(t, "to")) {
        return;
    }
    idx += 1;
    if let Some(Token::Word(new)) = tokens.get(idx) {
        if let Some(table) = schema.tables.get_mut(table_name) {
            if let Some(col) = table.column_mut(&old) {
                col.name = new.clone();
            }
        }
    }
}

fn apply_alter_action(schema: &mut Schema, table_name: &str, action: &[Token]) {
    let Some(first) = action.first() else {
        return;
    };
    if keyword_eq(first, "add") {
        apply_alter_add(schema, table_name, &action[1..]);
    } else if keyword_eq(first, "drop") {
        apply_alter_drop(schema, table_name, &action[1..]);
    } else if keyword_eq(first, "alter") {
        apply_alter_column(schema, table_name, &action[1..]);
    }
}

/// Promote the listed columns of `table_name` to NOT NULL, if present.
fn mark_primary_key(schema: &mut Schema, table_name: &str, cols: Vec<String>) {
    if let Some(table) = schema.tables.get_mut(table_name) {
        for pk in cols {
            if let Some(col) = table.column_mut(&pk) {
                col.not_null = true;
            }
        }
    }
}

fn apply_alter_add(schema: &mut Schema, table_name: &str, tokens: &[Token]) {
    let mut cursor = 0;
    // `ADD CONSTRAINT ... PRIMARY KEY (cols)` promotes those columns to NOT NULL.
    if matches!(tokens.get(cursor), Some(t) if keyword_eq(t, "constraint")) {
        if let Some(cols) = table_level_primary_key(tokens) {
            mark_primary_key(schema, table_name, cols);
        }
        return;
    }
    if matches!(tokens.get(cursor), Some(t) if keyword_eq(t, "column")) {
        cursor += 1;
    }
    skip_existence_modifiers(tokens, &mut cursor);
    // A bare `ADD PRIMARY KEY (cols)` (no CONSTRAINT keyword).
    if matches!(tokens.get(cursor), Some(t) if keyword_eq(t, "primary")) {
        if let Some(cols) = table_level_primary_key(&tokens[cursor..]) {
            mark_primary_key(schema, table_name, cols);
        }
        return;
    }
    if let Some(column) = parse_column_def(&tokens[cursor..]) {
        if let Some(table) = schema.tables.get_mut(table_name) {
            // A re-added column replaces any prior definition of the same name.
            table.remove_column(&column.name);
            table.columns.push(column);
        }
    }
}

fn apply_alter_drop(schema: &mut Schema, table_name: &str, tokens: &[Token]) {
    let mut cursor = 0;
    if matches!(tokens.get(cursor), Some(t) if keyword_eq(t, "column")) {
        cursor += 1;
    }
    skip_existence_modifiers(tokens, &mut cursor);
    if let Some(Token::Word(col)) = tokens.get(cursor) {
        if let Some(table) = schema.tables.get_mut(table_name) {
            table.remove_column(col);
        }
    }
}

fn apply_alter_column(schema: &mut Schema, table_name: &str, tokens: &[Token]) {
    let mut cursor = 0;
    if matches!(tokens.get(cursor), Some(t) if keyword_eq(t, "column")) {
        cursor += 1;
    }
    let col_name = match tokens.get(cursor) {
        Some(Token::Word(w)) => w.clone(),
        _ => return,
    };
    cursor += 1;
    let rest = &tokens[cursor..];

    let Some(table) = schema.tables.get_mut(table_name) else {
        return;
    };
    let Some(column) = table.column_mut(&col_name) else {
        return;
    };

    if matches!(rest.first(), Some(t) if keyword_eq(t, "set"))
        && matches!(rest.get(1), Some(t) if keyword_eq(t, "not"))
        && matches!(rest.get(2), Some(t) if keyword_eq(t, "null"))
    {
        column.not_null = true;
    } else if matches!(rest.first(), Some(t) if keyword_eq(t, "drop"))
        && matches!(rest.get(1), Some(t) if keyword_eq(t, "not"))
        && matches!(rest.get(2), Some(t) if keyword_eq(t, "null"))
    {
        column.not_null = false;
    } else if let Some(type_start) = type_keyword_offset(rest) {
        if let Some(sql_type) = parse_type(&rest[type_start..]) {
            column.sql_type = sql_type;
        }
    }
}

/// Offset just past `TYPE` / `SET DATA TYPE` in an `ALTER COLUMN` tail.
fn type_keyword_offset(tokens: &[Token]) -> Option<usize> {
    for (idx, token) in tokens.iter().enumerate() {
        if keyword_eq(token, "type") {
            return Some(idx + 1);
        }
    }
    None
}

fn apply_drop_table(schema: &mut Schema, tokens: &[Token]) {
    let mut cursor = 0;
    skip_existence_modifiers(tokens, &mut cursor);
    // `DROP TABLE a, b, c` — drop each named table.
    while let Some(name) = read_table_name(tokens, &mut cursor) {
        schema.tables.remove(&name);
        if matches!(tokens.get(cursor), Some(Token::Comma)) {
            cursor += 1;
        } else {
            break;
        }
    }
}

// --- Column / type parsing ---------------------------------------------------

/// Parse a single column definition (`name type [constraints…]`). Returns
/// `None` when the item turns out not to be a column after all.
fn parse_column_def(tokens: &[Token]) -> Option<Column> {
    let name = match tokens.first() {
        Some(Token::Word(w)) => w.clone(),
        _ => return None,
    };
    let sql_type = parse_type(&tokens[1..])?;
    let not_null = is_serial(&sql_type.base)
        || tokens_contain_phrase(tokens, &["not", "null"])
        || tokens_contain_phrase(tokens, &["primary", "key"]);
    Some(Column {
        name,
        sql_type,
        not_null,
    })
}

/// Multi-word base type names, longest first so greedy matching is correct.
const MULTIWORD_TYPES: &[&[&str]] = &[
    &["timestamp", "with", "time", "zone"],
    &["timestamp", "without", "time", "zone"],
    &["time", "with", "time", "zone"],
    &["time", "without", "time", "zone"],
    &["double", "precision"],
    &["character", "varying"],
    &["bit", "varying"],
];

/// Parse a type starting at `tokens[0]`. Recognizes multi-word base names,
/// discards `(size[, scale])`, and counts trailing `[]` array dimensions.
fn parse_type(tokens: &[Token]) -> Option<SqlType> {
    let words: Vec<String> = tokens
        .iter()
        .map(|t| match t {
            Token::Word(w) => Some(w.to_ascii_lowercase()),
            _ => None,
        })
        .take_while(Option::is_some)
        .flatten()
        .collect();
    if words.is_empty() {
        return None;
    }

    let mut consumed = 1;
    let mut base = words[0].clone();
    for phrase in MULTIWORD_TYPES {
        if words.len() >= phrase.len()
            && words
                .iter()
                .zip(phrase.iter())
                .take(phrase.len())
                .all(|(w, p)| w == p)
        {
            base = phrase.join(" ");
            consumed = phrase.len();
            break;
        }
    }

    // Re-walk the raw token stream to skip `(size)` and count `[]` suffixes,
    // since those are punctuation tokens the `words` view dropped.
    let mut idx = consumed;
    if matches!(tokens.get(idx), Some(Token::LParen)) {
        idx = skip_balanced_parens(tokens, idx);
    }
    let mut array_dims = 0;
    // `int[]`, `int[][]`, and `int array` all denote arrays.
    while matches!(tokens.get(idx), Some(Token::LBracket)) {
        array_dims += 1;
        idx += 1;
        // Optional dimension size inside the brackets.
        while matches!(tokens.get(idx), Some(Token::Word(_))) {
            idx += 1;
        }
        if matches!(tokens.get(idx), Some(Token::RBracket)) {
            idx += 1;
        }
    }
    if matches!(tokens.get(idx), Some(t) if keyword_eq(t, "array")) {
        array_dims += 1;
    }

    Some(SqlType { base, array_dims })
}

fn is_serial(base: &str) -> bool {
    matches!(
        base,
        "serial" | "serial4" | "bigserial" | "serial8" | "smallserial" | "serial2"
    )
}

// --- Token helpers -----------------------------------------------------------

/// Contents of the first balanced `( … )` group in `tokens`, if any.
fn paren_body(tokens: &[Token]) -> Option<&[Token]> {
    let start = tokens.iter().position(|t| *t == Token::LParen)?;
    let mut depth = 0;
    for (idx, token) in tokens.iter().enumerate().skip(start) {
        match token {
            Token::LParen => depth += 1,
            Token::RParen => {
                depth -= 1;
                if depth == 0 {
                    return Some(&tokens[start + 1..idx]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Index just past a balanced paren group that starts at `tokens[start]`.
fn skip_balanced_parens(tokens: &[Token], start: usize) -> usize {
    let mut depth = 0;
    for (idx, token) in tokens.iter().enumerate().skip(start) {
        match token {
            Token::LParen => depth += 1,
            Token::RParen => {
                depth -= 1;
                if depth == 0 {
                    return idx + 1;
                }
            }
            _ => {}
        }
    }
    tokens.len()
}

/// Split a token slice on commas that sit at paren depth 0.
fn split_top_level_commas(tokens: &[Token]) -> Vec<&[Token]> {
    let mut parts = Vec::new();
    let mut depth: i32 = 0;
    let mut start = 0;
    for (idx, token) in tokens.iter().enumerate() {
        match token {
            Token::LParen => depth += 1,
            Token::RParen => depth -= 1,
            Token::Comma if depth == 0 => {
                parts.push(&tokens[start..idx]);
                start = idx + 1;
            }
            _ => {}
        }
    }
    parts.push(&tokens[start..]);
    parts
}

/// `true` when an item is a table-level constraint rather than a column.
fn is_table_constraint(item: &[Token]) -> bool {
    matches!(
        item.first(),
        Some(t) if keyword_eq(t, "constraint")
            || keyword_eq(t, "primary")
            || keyword_eq(t, "foreign")
            || keyword_eq(t, "unique")
            || keyword_eq(t, "check")
            || keyword_eq(t, "exclude")
            || keyword_eq(t, "like")
    )
}

/// If `item` is a `PRIMARY KEY (a, b, …)` clause (optionally `CONSTRAINT name`
/// prefixed), return the listed column names.
fn table_level_primary_key(item: &[Token]) -> Option<Vec<String>> {
    let mut idx = 0;
    if matches!(item.get(idx), Some(t) if keyword_eq(t, "constraint")) {
        idx += 1;
        // Skip the optional constraint name.
        if matches!(item.get(idx), Some(Token::Word(_))) {
            idx += 1;
        }
    }
    if !(matches!(item.get(idx), Some(t) if keyword_eq(t, "primary"))
        && matches!(item.get(idx + 1), Some(t) if keyword_eq(t, "key")))
    {
        return None;
    }
    let body = paren_body(&item[idx..])?;
    let cols = body
        .iter()
        .filter_map(|t| match t {
            Token::Word(w) => Some(w.clone()),
            _ => None,
        })
        .collect();
    Some(cols)
}

/// Whether `tokens` contains `phrase` as consecutive case-insensitive words at
/// paren depth 0 (so a `CHECK (x IS NOT NULL)` clause never trips `not null`).
fn tokens_contain_phrase(tokens: &[Token], phrase: &[&str]) -> bool {
    let mut depth = 0;
    let mut matched = 0;
    for token in tokens {
        match token {
            Token::LParen => {
                depth += 1;
                matched = 0;
            }
            Token::RParen => {
                depth -= 1;
                matched = 0;
            }
            Token::Word(w) if depth == 0 && w.eq_ignore_ascii_case(phrase[matched]) => {
                matched += 1;
                if matched == phrase.len() {
                    return true;
                }
            }
            _ => matched = 0,
        }
    }
    false
}
