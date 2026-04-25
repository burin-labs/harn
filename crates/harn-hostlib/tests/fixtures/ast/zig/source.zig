const std = @import("std");

pub fn add(a: i32, b: i32) i32 {
    return a + b;
}

pub fn shout(comptime s: []const u8) []const u8 {
    return s;
}

test "add returns sum" {
    try std.testing.expectEqual(@as(i32, 3), add(1, 2));
}
