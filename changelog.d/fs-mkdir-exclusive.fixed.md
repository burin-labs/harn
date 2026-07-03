`harness.fs.mkdir(path, false)` now performs non-recursive, exclusive directory creation so
Harn workflows can use directory creation as an atomic cross-process lock primitive.
