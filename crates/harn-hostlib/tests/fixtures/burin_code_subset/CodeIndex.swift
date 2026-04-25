// CodeIndex.swift
//
// Trimmed fixture mirroring the structure of
// `burin-labs/burin-code/Sources/BurinCodeIndex/CodeIndex.swift`. Used by
// `tests/code_index_scenario.rs` as a stand-in when the live repo isn't
// available — the tree-shape and `import` declarations are what the
// scenario asserts on, not the implementation details.

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
