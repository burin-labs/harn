- **Swift imports resolve to build targets, and same-target files see each
  other without one (#8114).**
  Swift declared no import resolver, so the dep graph answered nothing for
  every Swift file in a workspace. It now resolves `import Core` to every file
  in the `Core` target, reading target membership from the SwiftPM layout: the
  directory immediately below the nearest `Sources` or `Tests` ancestor. The
  `@testable import` spelling a test target uses to name the target under test
  is recognised too, as are `import Core.Net` and `import struct Core.Box`.
  Files inside one target are also visible to each other with no import
  statement at all, which is what Swift actually means by a module, so they are
  part of the answer.
  Neither half is stored as a dependency edge per pair of files. A target of N
  files named by M importers costs N plus M rows rather than N times M, which
  is the cross-product shape removed from the symbol graph in #8081. On a
  7,038-file workspace 6,185 stored rows expand to 1,546,696 dependency
  answers. Swift files whose import statements resolve go from 0 to 1,089, and
  1,868 of them additionally see their target's other files.
  The census reports those two counts separately, because a language with
  implicit module membership would otherwise report every file as resolved
  while its import-path resolver did nothing at all.
