// WordIndex.swift
//
// Fixture stand-in for the Swift `WordIndex` actor. Only shape matters
// for the scenario test — substring queries should find this file by
// name.

import Foundation

public struct WordHit {
    public let file: UInt32
    public let line: UInt32
}

public struct WordIndex {
    public private(set) var index: [String: [WordHit]] = [:]

    public init() {}

    public func get(_ word: String) -> [WordHit] {
        index[word] ?? []
    }
}
