// CodeIndex.swift
//
// Trimmed Swift-shaped code-index fixture. Used by
// `tests/code_index_scenario.rs`; the tree-shape and `import`
// declarations are what the scenario asserts on, not the implementation
// details.

import Foundation

public actor CodeIndex {
    public let workspaceRoot: String

    public init(workspaceRoot: String) {
        self.workspaceRoot = workspaceRoot
    }

    public func reindexBatch(absolutePaths: [String]) {
        // Real implementation walks files; the fixture only needs to be
        // reachable by name.
    }
}
