    try std.testing.expect(std.mem.indexOf(u8, output, "# inline") == null);
}

// ─── Comprehensive round-trip and writer behavior tests ──────────────────────

test "round-trip: parse then write preserves content" {
    const allocator = std.testing.allocator;
    const input =
        \\[server]
        \\host = 0.0.0.0
        \\port = 8080
        \\
        \\[logging]
        \\level = info
        \\file = app.log
        \\
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    // Parse the output again and verify same content
    var doc2 = try parser.parse(allocator, output);
    defer doc2.deinit();

    try std.testing.expectEqualStrings("0.0.0.0", doc2.get("server", "host").?);
    try std.testing.expectEqualStrings("8080", doc2.get("server", "port").?);
    try std.testing.expectEqualStrings("info", doc2.get("logging", "level").?);
    try std.testing.expectEqualStrings("app.log", doc2.get("logging", "file").?);
}

test "round-trip: sections preserve order" {
    const allocator = std.testing.allocator;
    const input =
        \\[alpha]
        \\key = a
        \\
        \\[beta]
        \\key = b
        \\
        \\[gamma]
        \\key = c
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    // Verify section order in output
    const alpha_pos = std.mem.indexOf(u8, output, "[alpha]").?;
    const beta_pos = std.mem.indexOf(u8, output, "[beta]").?;
    const gamma_pos = std.mem.indexOf(u8, output, "[gamma]").?;
    try std.testing.expect(alpha_pos < beta_pos);
    try std.testing.expect(beta_pos < gamma_pos);

    // Re-parse and verify order
    var doc2 = try parser.parse(allocator, output);
    defer doc2.deinit();
    try std.testing.expectEqual(@as(usize, 3), doc2.sectionCount());
    try std.testing.expectEqualStrings("alpha", doc2.sections.items[0].name);
    try std.testing.expectEqualStrings("beta", doc2.sections.items[1].name);
    try std.testing.expectEqualStrings("gamma", doc2.sections.items[2].name);
}

test "round-trip: comments are retained in output" {
    const allocator = std.testing.allocator;
    const input =
        \\[section]
        \\# This is a comment
        \\key = value # inline comment
        \\
        \\# Another comment
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "# This is a comment") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "# inline comment") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "# Another comment") != null);

    // Re-parse and verify comments are still there
    var doc2 = try parser.parse(allocator, output);
    defer doc2.deinit();
    try std.testing.expectEqualStrings("value", doc2.get("section", "key").?);
}

test "round-trip: quoted values round-trip correctly" {
    const allocator = std.testing.allocator;
    const input =
        \\[section]
        \\quoted = "hello world"
        \\single_quoted = 'single'
        \\with_escape = "has\\backslash"
        \\with_newline = "line1\\nline2"
        \\unquoted = plain
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    // Re-parse and verify values
    var doc2 = try parser.parse(allocator, output);
    defer doc2.deinit();
    try std.testing.expectEqualStrings("hello world", doc2.get("section", "quoted").?);
    try std.testing.expectEqualStrings("single", doc2.get("section", "single_quoted").?);
    try std.testing.expectEqualStrings("has\\backslash", doc2.get("section", "with_escape").?);
    try std.testing.expectEqualStrings("line1\nline2", doc2.get("section", "with_newline").?);
    try std.testing.expectEqualStrings("plain", doc2.get("section", "unquoted").?);
}

test "round-trip: multiline values round-trip" {
    const allocator = std.testing.allocator;
    const input =
        \\[section]
        \\multi = first\\
        \\second\\
        \\third
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    // Re-parse and verify the multiline value was preserved
    var doc2 = try parser.parse(allocator, output);
    defer doc2.deinit();
    const val = doc2.get("section", "multi").?;
    try std.testing.expectEqualStrings("first\nsecond\nthird", val);
}

test "round-trip: empty sections are preserved" {
    const allocator = std.testing.allocator;
    const input =
        \\[section]
        \\
        \\[empty]
        \\
        \\[another]
        \\key = value
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    // Verify all sections exist in output
    try std.testing.expect(std.mem.indexOf(u8, output, "[section]") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "[empty]") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "[another]") != null);

    // Re-parse and verify empty section count
    var doc2 = try parser.parse(allocator, output);
    defer doc2.deinit();
    try std.testing.expectEqual(@as(usize, 3), doc2.sectionCount());
    try std.testing.expectEqual(@as(usize, 0), doc2.get("empty", "key"). == null);
}

test "alignment option produces padded output" {
    const allocator = std.testing.allocator;
    var document = doc.IniDocument.init(allocator);
    defer document.deinit();

    try document.set("db", "host", "localhost");
    try document.set("db", "port", "5432");
    try document.set("db", "connection_string", "tcp://localhost:5432");

    const output = try writeWithOptions(allocator, &document, .{
        .align_values = true,
        .section_spacing = false,
    });
    defer allocator.free(output);

    // Find the line with "host"
    const host_line_start = std.mem.indexOf(u8, output, "host").?;
    const host_line_end = std.mem.indexOfScalar(u8, output[host_line_start..], '\n').?;
    const host_line = output[host_line_start .. host_line_start + host_line_end];

    // Find the line with "connection_string"
    const conn_line_start = std.mem.indexOf(u8, output, "connection_string").?;
    const conn_line_end = std.mem.indexOfScalar(u8, output[conn_line_start..], '\n').?;
    const conn_line = output[conn_line_start .. conn_line_start + conn_line_end];

    // Both lines should have the '=' at the same column
    const host_eq_pos = std.mem.indexOfScalar(u8, host_line, '=').?;
    const conn_eq_pos = std.mem.indexOfScalar(u8, conn_line, '=').?;
    try std.testing.expectEqual(host_eq_pos, conn_eq_pos);
}

test "round-trip: global entries are preserved" {
    const allocator = std.testing.allocator;
    const input =
        \\global_key = global_value
        \\
        \\[section]
        \\key = value
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    // Re-parse and verify global entries
    var doc2 = try parser.parse(allocator, output);
    defer doc2.deinit();
    try std.testing.expectEqualStrings("global_value", doc2.get("section", "key").?);
}

test "round-trip: semicolon comments are retained" {
    const allocator = std.testing.allocator;
    const input =
        \\[section]
        \\; This is a semicolon comment
        \\key = value
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "; This is a semicolon comment") != null);
}

test "round-trip: section header comments are retained" {
    const allocator = std.testing.allocator;
    const input =
        \\# Header comment for section
        \\[section]
        \\key = value
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "# Header comment for section") != null);
}

test "write: empty document produces empty string" {
    const allocator = std.testing.allocator;
    var document = doc.IniDocument.init(allocator);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    try std.testing.expectEqualStrings("", output);
}

test "write: multiple sections with spacing between them" {
    const allocator = std.testing.allocator;
    var document = doc.IniDocument.init(allocator);
    defer document.deinit();

    try document.set("first", "a", "1");
    try document.set("second", "b", "2");
    try document.set("third", "c", "3");

    const output = try write(allocator, &document);
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "\n\n[second]") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "\n\n[third]") != null);
}

test "write: section_spacing=false removes blank lines between sections" {
    const allocator = std.testing.allocator;
    var document = doc.IniDocument.init(allocator);
    defer document.deinit();

    try document.set("a", "x", "1");
    try document.set("b", "y", "2");

    const output = try writeWithOptions(allocator, &document, .{
        .section_spacing = false,
    });
    defer allocator.free(output);

    // No blank line between sections
    try std.testing.expect(std.mem.indexOf(u8, output, "\n\n[b]") == null);
}

test "write: custom delimiter" {
    const allocator = std.testing.allocator;
    var document = doc.IniDocument.init(allocator);
    defer document.deinit();

    try document.set("s", "key", "value");

    const output = try writeWithOptions(allocator, &document, .{
        .delimiter = ':',
        .space_around_delimiter = true,
    });
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "key : value") != null);
}

test "write: auto_quote quotes values with special chars" {
    const allocator = std.testing.allocator;
    var document = doc.IniDocument.init(allocator);
    defer document.deinit();

    try document.set("s", "path", "/usr/local/bin");
    try document.set("s", "commented", "has # hash");
    try document.set("s", "normal", "ok");

    const output = try writeWithOptions(allocator, &document, .{
        .auto_quote = true,
        .write_comments = false,
    });
    defer allocator.free(output);

    // Values with special chars should be quoted
    try std.testing.expect(std.mem.indexOf(u8, output, '"has # hash"') != null);
    // Normal values should not be quoted
    try std.testing.expect(std.mem.indexOf(u8, output, '"ok"') == null);
    try std.testing.expect(std.mem.indexOf(u8, output, "normal = ok") != null);
}

test "write: was_quoted preserves quoting on round-trip" {
    const allocator = std.testing.allocator;
    const input =
        \\[section]
        \\name = "quoted value"
        \\plain = unquoted
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    // The originally-quoted value should still be quoted in output
    try std.testing.expect(std.mem.indexOf(u8, output, '"quoted value"') != null);
    // The unquoted value should remain unquoted
    try std.testing.expect(std.mem.indexOf(u8, output, "plain = unquoted") != null);
}

test "write: indented output" {
    const allocator = std.testing.allocator;
    var document = doc.IniDocument.init(allocator);
    defer document.deinit();

    try document.set("s", "key1", "val1");
    try document.set("s", "key2", "val2");

    const output = try writeWithOptions(allocator, &document, .{
        .indent = "    ",
    });
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "    key1 = val1") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "    key2 = val2") != null);
}

test "write: align_values with indent" {
    const allocator = std.testing.allocator;
    var document = doc.IniDocument.init(allocator);
    defer document.deinit();

    try document.set("section", "a", "1");
    try document.set("section", "very_long_key", "2");

    const output = try writeWithOptions(allocator, &document, .{
        .align_values = true,
        .indent = "  ",
    });
    defer allocator.free(output);

    // Both lines should start with indent
    try std.testing.expect(std.mem.indexOf(u8, output, "  a") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "  very_long_key") != null);

    // Both '=' should be at the same column
    const a_eq = std.mem.indexOf(u8, output, "  a").?;
    const long_eq = std.mem.indexOf(u8, output, "  very_long_key").?;
    // Find '=' position relative to line start
    const a_line = output[a_eq..];
    const long_line = output[long_eq..];
    const a_eq_rel = std.mem.indexOfScalar(u8, a_line, '=').?;
    const long_eq_rel = std.mem.indexOfScalar(u8, long_line, '=').?;
    try std.testing.expectEqual(a_eq_rel, long_eq_rel);
}

test "round-trip: complex document with all features" {
    const allocator = std.testing.allocator;
    const input =
        \\# Global comment
        \\global = value
        \\
        \\# Server section
        \\[server]
        \\host = 0.0.0.0
        \\port = "8080"
        \\mode = debug # enable debug mode
        \\
        \\# Database section
        \\[database]
        \\connection = "tcp://localhost:5432"
        \\timeout = 30
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    // Re-parse and verify all values
    var doc2 = try parser.parse(allocator, output);
    defer doc2.deinit();

    try std.testing.expectEqualStrings("value", doc2.get("server", "host").?);
    try std.testing.expectEqualStrings("8080", doc2.get("server", "port").?);
    try std.testing.expectEqualStrings("debug", doc2.get("server", "mode").?);
    try std.testing.expectEqualStrings("tcp://localhost:5432", doc2.get("database", "connection").?);
    try std.testing.expectEqualStrings("30", doc2.get("database", "timeout").?);

    // Verify comments are retained
    try std.testing.expect(std.mem.indexOf(u8, output, "# Global comment") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "# Server section") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "# Database section") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "# enable debug mode") != null);

    // Verify quoted values are preserved
    try std.testing.expect(std.mem.indexOf(u8, output, '"8080"') != null);
    try std.testing.expect(std.mem.indexOf(u8, output, '"tcp://localhost:5432"') != null);

    // Verify section order
    const server_pos = std.mem.indexOf(u8, output, "[server]").?;
    const database_pos = std.mem.indexOf(u8, output, "[database]").?;
    try std.testing.expect(server_pos < database_pos);
}

test "write: empty value is written" {
    const allocator = std.testing.allocator;
    var document = doc.IniDocument.init(allocator);
    defer document.deinit();

    try document.set("s", "empty", "");

    const output = try write(allocator, &document);
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "empty = ") != null);
}

test "write: entries within a section preserve order" {
    const allocator = std.testing.allocator;
    var document = doc.IniDocument.init(allocator);
    defer document.deinit();

    try document.set("s", "z_last", "1");
    try document.set("s", "a_first", "2");
    try document.set("s", "m_middle", "3");

    const output = try write(allocator, &document);
    defer allocator.free(output);

    const z_pos = std.mem.indexOf(u8, output, "z_last").?;
    const a_pos = std.mem.indexOf(u8, output, "a_first").?;
    const m_pos = std.mem.indexOf(u8, output, "m_middle").?;
    // Order should match insertion order, not alphabetical
    try std.testing.expect(z_pos < a_pos);
    try std.testing.expect(a_pos < m_pos);
}

test "write: inline comments preserved" {
    const allocator = std.testing.allocator;
    const input =
        \\[section]
        \\key = value # this is a comment
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "# this is a comment") != null);
}

test "write: delimiter=colon with spaces" {
    const allocator = std.testing.allocator;
    var document = doc.IniDocument.init(allocator);
    defer document.deinit();

    try document.set("s", "key", "value");

    const output = try writeWithOptions(allocator, &document, .{
        .delimiter = ':',
        .space_around_delimiter = true,
    });
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "key : value") != null);
}

test "write: delimiter=colon without spaces" {
    const allocator = std.testing.allocator;
    var document = doc.IniDocument.init(allocator);
    defer document.deinit();

    try document.set("s", "key", "value");

    const output = try writeWithOptions(allocator, &document, .{
        .delimiter = ':',
        .space_around_delimiter = false,
    });
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "key:value") != null);
}

test "needsQuoting: detects hash and semicolon" {
    try std.testing.expect(needsQuoting("has # hash"));
    try std.testing.expect(needsQuoting("has ; semicolon"));
}

test "needsQuoting: detects leading/trailing whitespace" {
    try std.testing.expect(needsQuoting(" leading"));
    try std.testing.expect(needsQuoting("trailing "));
    try std.testing.expect(needsQuoting(" both "));
}

test "needsQuoting: no quoting needed for normal values" {
    try std.testing.expect(!needsQuoting("normal"));
    try std.testing.expect(!needsQuoting("with-dash"));
    try std.testing.expect(!needsQuoting("with_underscore"));
    try std.testing.expect(!needsQuoting("with.dots"));
    try std.testing.expect(!needsQuoting("with123numbers"));
}

test "needsQuoting: no quoting for empty string" {
    try std.testing.expect(!needsQuoting(""));
}

test "needsQuoting: detects special characters" {
    try std.testing.expect(needsQuoting("has\"quote"));
    try std.testing.expect(needsQuoting("has'apostrophe"));
    try std.testing.expect(needsQuoting("has\ntab"));
    try std.testing.expect(needsQuoting("has\\backslash"));
}

test "round-trip: parse then write with alignment option" {
    const allocator = std.testing.allocator;
    const input =
        \\[section]
        \\a = 1
        \\long_key = 2
        \\x = 3
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try writeWithOptions(allocator, &document, .{
        .align_values = true,
        .section_spacing = false,
    });
    defer allocator.free(output);

    // Verify alignment in output
    const a_line_start = std.mem.indexOf(u8, output, "a =").?;
    const long_line_start = std.mem.indexOf(u8, output, "long_key =").?;
    const x_line_start = std.mem.indexOf(u8, output, "x =").?;

    const a_line = output[a_line_start..];
    const long_line = output[long_line_start..];
    const x_line = output[x_line_start..];

    const a_eq = std.mem.indexOfScalar(u8, a_line, '=').?;
    const long_eq = std.mem.indexOfScalar(u8, long_line, '=').?;
    const x_eq = std.mem.indexOfScalar(u8, x_line, '=').?;

    try std.testing.expectEqual(a_eq, long_eq);
    try std.testing.expectEqual(long_eq, x_eq);
}

test "round-trip: comment-only section" {
    const allocator = std.testing.allocator;
    const input =
        \\[section]
        \\# Just a comment
        \\# And another
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "[section]") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "# Just a comment") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "# And another") != null);

    // Re-parse
    var doc2 = try parser.parse(allocator, output);
    defer doc2.deinit();
    try std.testing.expectEqual(@as(usize, 1), doc2.sectionCount());
}

test "write: suppresses blank lines when section_spacing false" {
    const allocator = std.testing.allocator;
    var document = doc.IniDocument.init(allocator);
    defer document.deinit();

    try document.set("a", "x", "1");
    try document.set("b", "y", "2");

    const output = try writeWithOptions(allocator, &document, .{
        .section_spacing = false,
    });
    defer allocator.free(output);

    // Should not have double newlines between sections
    try std.testing.expect(std.mem.indexOf(u8, output, "\n\n[b]") == null);
}

test "round-trip: preserves section with only blank lines" {
    const allocator = std.testing.allocator;
    const input =
        \\[section]
        \\
        \\
        \\key = value
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    // Re-parse and verify
    var doc2 = try parser.parse(allocator, output);
    defer doc2.deinit();
    try std.testing.expectEqualStrings("value", doc2.get("section", "key").?);
}

test "write: quoted multiline escape sequences round-trip" {
    const allocator = std.testing.allocator;
    const input =
        \\[section]
        \\value = "tab\\there\\nnewline"
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    // Re-parse and verify
    var doc2 = try parser.parse(allocator, output);
    defer doc2.deinit();
    const val = doc2.get("section", "value").?;
    try std.testing.expectEqualStrings("tab\there\nnewline", val);
}

test "round-trip: global entries round-trip" {
    const allocator = std.testing.allocator;
    const input =
        \\key = value
        \\
        \\[section]
        \\key2 = value2
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    // Re-parse and verify
    var doc2 = try parser.parse(allocator, output);
    defer doc2.deinit();
    try std.testing.expectEqualStrings("value2", doc2.get("section", "key2").?);
}

test "round-trip: section header comments round-trip" {
    const allocator = std.testing.allocator;
    const input =
        \\# comment before section
        \\[section]
        \\key = value
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "# comment before section") != null);
}

test "write: write_comments=false suppresses all comments" {
    const allocator = std.testing.allocator;
    const input =
        \\# global comment
        \\[section]
        \\# section comment
        \\key = value # inline
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try writeWithOptions(allocator, &document, .{
        .write_comments = false,
    });
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "# global comment") == null);
    try std.testing.expect(std.mem.indexOf(u8, output, "# section comment") == null);
    try std.testing.expect(std.mem.indexOf(u8, output, "# inline") == null);
    // But key-value should still be there
    try std.testing.expect(std.mem.indexOf(u8, output, "key = value") != null);
}

test "write: section header comment with semicolon prefix" {
    const allocator = std.testing.allocator;
    const input =
        \\; semicolon comment
        \\[section]
        \\key = value
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "; semicolon comment") != null);
}

test "write: multiple global entries" {
    const allocator = std.testing.allocator;
    const input =
        \\key1 = val1
        \\key2 = val2
        \\
        \\[section]
        \\key3 = val3
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "key1 = val1") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "key2 = val2") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "key3 = val3") != null);
}

test "write: document with only global entries" {
    const allocator = std.testing.allocator;
    const input =
        \\key = value
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "key = value") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "[") == null);
}

test "write: document with only sections, no globals" {
    const allocator = std.testing.allocator;
    const input =
        \\[section]
        \\key = value
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "[section]") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "key = value") != null);
}

test "round-trip: section with only comments" {
    const allocator = std.testing.allocator;
    const input =
        \\[section]
        \\# comment 1
        \\# comment 2
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try write(allocator, &document);
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "[section]") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "# comment 1") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "# comment 2") != null);
}

test "write: alignment with mixed entry types" {
    const allocator = std.testing.allocator;
    var document = doc.IniDocument.init(allocator);
    defer document.deinit();

    try document.set("s", "short", "1");
    try document.set("s", "longer_key", "2");

    const output = try writeWithOptions(allocator, &document, .{
        .align_values = true,
    });
    defer allocator.free(output);

    // Find both lines
    const short_line_start = std.mem.indexOf(u8, output, "short").?;
    const long_line_start = std.mem.indexOf(u8, output, "longer_key").?;
    const short_line = output[short_line_start..];
    const long_line = output[long_line_start..];
    const short_eq = std.mem.indexOfScalar(u8, short_line, '=').?;
    const long_eq = std.mem.indexOfScalar(u8, long_line, '=').?;
    try std.testing.expectEqual(short_eq, long_eq);
}

test "round-trip: complex alignment preserves values" {
    const allocator = std.testing.allocator;
    const input =
        \\[section]
        \\a = 1
        \\bb = 2
        \\ccc = 3
    ;

    var document = try parser.parse(allocator, input);
    defer document.deinit();

    const output = try writeWithOptions(allocator, &document, .{
        .align_values = true,
        .section_spacing = false,
    });
    defer allocator.free(output);

    // Re-parse and verify values
    var doc2 = try parser.parse(allocator, output);
    defer doc2.deinit();
    try std.testing.expectEqualStrings("1", doc2.get("section", "a").?);
    try std.testing.expectEqualStrings("2", doc2.get("section", "bb").?);
    try std.testing.expectEqualStrings("3", doc2.get("section", "ccc").?);
}

test "write: comment entry type is written" {
    const allocator = std.testing.allocator;
    var document = doc.IniDocument.init(allocator);
    defer document.deinit();

    try document.set("s", "key", "value");

    const section = document.findSectionMut("s").?;
    try section.entries.append(allocator, .{ .comment = .{
        .text = try allocator.dupe(u8, "a comment"),
        .prefix = '#',
    } });

    const output = try write(allocator, &document);
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "# a comment") != null);
}

test "write: blank entry type produces blank line" {
    const allocator = std.testing.allocator;
    var document = doc.IniDocument.init(allocator);
    defer document.deinit();

    try document.set("s", "key", "value");

    const section = document.findSectionMut("s").?;
    try section.entries.append(allocator, .{ .blank = {} });

    const output = try write(allocator, &document);
    defer allocator.free(output);

    try std.testing.expect(std.mem.indexOf(u8, output, "key = value\n\n") != null);
}
