Fixed release binary builds to restore a same-target broad Rust cache fallback
before Swatinem's precise cache key, avoiding cold release compiles when
workspace/version hashes rotate.
