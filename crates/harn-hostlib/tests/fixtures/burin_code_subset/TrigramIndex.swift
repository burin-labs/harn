// TrigramIndex.swift
//
// Trimmed fixture for the scenario test. Mirrors the public surface of
// the Swift `TrigramIndex` so the scenario asserts queries by substring
// can find this file.

import Foundation

public typealias Trigram = UInt32
public typealias FileID = UInt32

public struct TrigramIndex {
    public private(set) var index: [Trigram: Set<FileID>] = [:]

    public init() {}

    public func query(_ trigrams: [Trigram]) -> Set<FileID> {
        var acc: Set<FileID> = []
        for tg in trigrams {
            if let set = index[tg] {
                acc.formUnion(set)
            }
        }
        return acc
    }
}
