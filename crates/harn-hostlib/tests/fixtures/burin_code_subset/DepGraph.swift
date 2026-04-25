// DepGraph.swift
//
// Forward + reverse dep edge fixture. Tracks `imports of` and
// `importers of` semantics; used by the scenario test purely as a
// substring target.

import Foundation

public struct DepGraph {
    public private(set) var forward: [UInt32: Set<UInt32>] = [:]
    public private(set) var reverse: [UInt32: Set<UInt32>] = [:]

    public init() {}

    public func importers(of file: UInt32) -> [UInt32] {
        Array(reverse[file] ?? [])
    }

    public func imports(of file: UInt32) -> [UInt32] {
        Array(forward[file] ?? [])
    }
}
