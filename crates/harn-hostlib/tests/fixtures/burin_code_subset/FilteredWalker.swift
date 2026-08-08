// FilteredWalker.swift
//
// Filtered-walker shaped fixture. Used by the scenario test
// to assert substring queries find this file — the body is irrelevant.

import Foundation

public enum FilteredWalker {
    public static let skipDirs: Set<String> = [
        ".git", "node_modules", "build", "dist", "target",
    ]

    public static let indexableExtensions: Set<String> = [
        "swift", "rs", "ts", "py",
    ]
}
