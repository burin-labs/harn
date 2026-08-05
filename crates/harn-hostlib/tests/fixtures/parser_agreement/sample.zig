const std = @import("std");
const builtin = @import("builtin");

// A `\\` multiline string literal: the exact construct the bundled
// tree-sitter-zig grammar once mis-lexed into a phantom `unexpected \`
// parse-error storm. The corpus pins that this parses CLEANLY.
const query =
    \\SELECT id, name
    \\FROM users
    \\WHERE active = true
;

pub fn run() void {
    _ = std;
    _ = builtin;
    _ = query;
}
