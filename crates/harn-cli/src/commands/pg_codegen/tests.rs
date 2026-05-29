use super::ddl::parse_migrations;
use super::emit::render;

/// Parse `sources` and render the table bodies, dropping the file header so
/// assertions focus on the generated types.
fn codegen(sources: &[&str]) -> String {
    let owned: Vec<String> = sources.iter().map(|s| s.to_string()).collect();
    let schema = parse_migrations(&owned);
    render(&schema, "", "Row")
}

#[test]
fn create_table_maps_columns_and_nullability() {
    let out = codegen(&[r"
        CREATE TABLE receipts (
            id BIGSERIAL PRIMARY KEY,
            kind TEXT NOT NULL,
            amount NUMERIC(10, 2) NOT NULL,
            note TEXT,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now()
        );
    "]);
    assert_eq!(
        out,
        "type ReceiptsRow = {\n  \
         id: int,\n  \
         kind: string,\n  \
         amount: string,\n  \
         note: string?,\n  \
         created_at: string,\n\
         }\n"
    );
}

#[test]
fn type_coverage_maps_each_family() {
    let out = codegen(&[r"
        CREATE TABLE t (
            a boolean NOT NULL,
            b integer NOT NULL,
            c bigint NOT NULL,
            d smallint NOT NULL,
            e real NOT NULL,
            f double precision NOT NULL,
            g numeric NOT NULL,
            h text NOT NULL,
            i varchar(40) NOT NULL,
            j uuid NOT NULL,
            k jsonb NOT NULL,
            l bytea NOT NULL,
            m date NOT NULL,
            n timestamp with time zone NOT NULL
        );
    "]);
    for expected in [
        "a: bool,",
        "b: int,",
        "c: int,",
        "d: int,",
        "e: float,",
        "f: float,",
        "g: string,",
        "h: string,",
        "i: string,",
        "j: string,",
        "k: any,",
        "l: bytes,",
        "m: string,",
        "n: string,",
    ] {
        assert!(out.contains(expected), "missing {expected:?} in:\n{out}");
    }
}

#[test]
fn arrays_become_nested_lists() {
    let out = codegen(&[r"
        CREATE TABLE arr (
            tags text[] NOT NULL,
            grid int[][],
            optional_ids bigint[]
        );
    "]);
    assert!(out.contains("tags: list<string>,"), "{out}");
    assert!(out.contains("grid: list<list<int>>?,"), "{out}");
    assert!(out.contains("optional_ids: list<int>?,"), "{out}");
}

#[test]
fn alter_add_and_drop_column_track_live_shape() {
    let out = codegen(&[
        "CREATE TABLE users (id BIGSERIAL PRIMARY KEY, email TEXT NOT NULL, legacy TEXT);",
        "ALTER TABLE users ADD COLUMN display_name TEXT;",
        "ALTER TABLE users DROP COLUMN legacy;",
        "ALTER TABLE users ALTER COLUMN display_name SET NOT NULL;",
    ]);
    assert_eq!(
        out,
        "type UsersRow = {\n  \
         id: int,\n  \
         email: string,\n  \
         display_name: string,\n\
         }\n"
    );
}

#[test]
fn rename_column_and_table_follow_through() {
    let out = codegen(&[
        "CREATE TABLE old_name (id INT NOT NULL, full_name TEXT NOT NULL);",
        "ALTER TABLE old_name RENAME COLUMN full_name TO display_name;",
        "ALTER TABLE old_name RENAME TO accounts;",
    ]);
    assert_eq!(
        out,
        "type AccountsRow = {\n  \
         id: int,\n  \
         display_name: string,\n\
         }\n"
    );
}

#[test]
fn drop_table_removes_record() {
    let out = codegen(&[
        "CREATE TABLE keep (id INT NOT NULL);",
        "CREATE TABLE temp (id INT NOT NULL);",
        "DROP TABLE temp;",
    ]);
    assert!(out.contains("type KeepRow"));
    assert!(!out.contains("TempRow"), "{out}");
}

#[test]
fn table_level_primary_key_implies_not_null() {
    let out = codegen(&[r"
        CREATE TABLE memberships (
            org_id BIGINT,
            user_id BIGINT,
            role TEXT,
            PRIMARY KEY (org_id, user_id)
        );
    "]);
    assert!(out.contains("org_id: int,"), "{out}");
    assert!(out.contains("user_id: int,"), "{out}");
    assert!(out.contains("role: string?,"), "{out}");
}

#[test]
fn check_constraint_does_not_leak_not_null() {
    // A `CHECK (... IS NOT NULL)` clause must not promote the column.
    let out = codegen(&["CREATE TABLE t (val TEXT CHECK (val IS NOT NULL));"]);
    assert!(out.contains("val: string?,"), "{out}");
}

#[test]
fn comments_and_dollar_quotes_are_ignored() {
    let out = codegen(&[r"
        -- a leading comment
        CREATE TABLE t ( /* inline */ id INT NOT NULL );
        CREATE FUNCTION noop() RETURNS void AS $$
            BEGIN
                -- this ; should not split the function body
                RETURN;
            END;
        $$ LANGUAGE plpgsql;
    "]);
    assert_eq!(out, "type TRow = {\n  id: int,\n}\n");
}

#[test]
fn schema_qualified_names_strip_prefix() {
    let out = codegen(&["CREATE TABLE public.events (id INT NOT NULL);"]);
    assert!(out.contains("type EventsRow"), "{out}");
}

#[test]
fn unknown_types_fall_back_to_string() {
    // A user-defined enum decodes as text at runtime, so it maps to `string`.
    let out = codegen(&[
        "CREATE TYPE mood AS ENUM ('happy', 'sad');",
        "CREATE TABLE t (current_mood mood NOT NULL);",
    ]);
    assert!(out.contains("current_mood: string,"), "{out}");
}

#[test]
fn output_is_deterministically_sorted_by_table() {
    let out = codegen(&[
        "CREATE TABLE zebra (id INT NOT NULL);",
        "CREATE TABLE alpha (id INT NOT NULL);",
    ]);
    let alpha = out.find("AlphaRow").expect("alpha present");
    let zebra = out.find("ZebraRow").expect("zebra present");
    assert!(alpha < zebra, "tables should sort alphabetically:\n{out}");
}

#[test]
fn generated_types_parse_and_typecheck() {
    // The whole point: generated declarations must be valid Harn that the
    // type-checker accepts.
    let types = codegen(&[r"
        CREATE TABLE receipts (
            id BIGSERIAL PRIMARY KEY,
            kind TEXT NOT NULL,
            tags TEXT[] NOT NULL,
            meta JSONB,
            note TEXT
        );
    "]);
    let program = format!("{types}\nfn use_it(r: ReceiptsRow) -> string {{ return r.kind }}\n");

    let mut lexer = harn_lexer::Lexer::new(&program);
    let tokens = lexer.tokenize().expect("generated types should tokenize");
    let mut parser = harn_parser::Parser::new(tokens);
    let module = parser.parse().expect("generated types should parse");
    let diagnostics = harn_parser::TypeChecker::new().check(&module);
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.severity == harn_parser::DiagnosticSeverity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "generated types should type-check, got: {errors:#?}\n\nprogram:\n{program}"
    );
}
