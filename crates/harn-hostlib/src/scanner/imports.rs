//! Language-aware import-statement extraction for the scanner pipeline.
//!
//! Only the subset used by the scanner pipeline is implemented here:
//! `extract_imports`. Import-rewriting belongs to editor-specific host
//! surfaces and should be added as separate hostlib methods if needed.

use regex::Regex;
use std::sync::OnceLock;

/// Extract zero or more import-target strings from `content`.
///
/// `language` is the lowercase file extension (`"rs"`, `"ts"`, …). Unknown
/// languages return an empty vec.
pub fn extract_imports(content: &str, language: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_go_block = false;

    for raw in content.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        match language {
            "swift" => {
                if let Some(name) = capture(swift_import(), trimmed) {
                    out.push(name);
                }
            }
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
                if let Some(name) = capture(js_from(), trimmed) {
                    out.push(name);
                } else if let Some(name) = capture(js_require(), trimmed) {
                    out.push(name);
                }
            }
            "py" => {
                if trimmed.starts_with('#') {
                    continue;
                }
                if let Some(name) = capture(py_import(), trimmed) {
                    out.push(name);
                } else if let Some(name) = capture(py_from(), trimmed) {
                    out.push(name);
                }
            }
            "go" => extract_go_import(trimmed, &mut out, &mut in_go_block),
            "rs" => {
                if let Some(name) = capture(rust_use(), trimmed) {
                    out.push(name);
                } else if let Some(name) = capture(rust_extern_crate(), trimmed) {
                    out.push(name);
                }
            }
            "java" | "kt" | "scala" | "sc" => {
                if let Some(name) = capture(jvm_import(), trimmed) {
                    out.push(name);
                }
            }
            "c" | "cpp" | "h" | "hpp" => {
                if let Some(name) = capture(c_include(), trimmed) {
                    out.push(name);
                }
            }
            "rb" => {
                if let Some(name) = capture(ruby_require(), trimmed) {
                    out.push(name);
                }
            }
            "php" => {
                if let Some(name) = capture(php_use(), trimmed) {
                    out.push(name);
                } else if let Some(name) = capture(php_include(), trimmed) {
                    out.push(name);
                }
            }
            "cs" => {
                if let Some(name) = capture(csharp_using(), trimmed) {
                    out.push(name);
                }
            }
            "sh" | "bash" | "zsh" => {
                if let Some(name) = capture(shell_source(), trimmed) {
                    out.push(name);
                }
            }
            "dart" => {
                if let Some(name) = capture(dart_import(), trimmed) {
                    out.push(name);
                }
            }
            _ => {}
        }
    }
    out
}

fn extract_go_import(trimmed: &str, out: &mut Vec<String>, in_block: &mut bool) {
    if trimmed == "import (" || trimmed.starts_with("import (") {
        *in_block = true;
        return;
    }
    if *in_block {
        if trimmed == ")" {
            *in_block = false;
            return;
        }
        if let Some(name) = capture(go_block_line(), trimmed) {
            out.push(name);
        }
    } else if let Some(name) = capture(go_single_import(), trimmed) {
        out.push(name);
    }
}

fn capture(regex: &Regex, line: &str) -> Option<String> {
    regex.captures(line)?.get(1).map(|m| m.as_str().to_string())
}

macro_rules! pattern {
    ($name:ident, $expr:expr) => {
        fn $name() -> &'static Regex {
            static R: OnceLock<Regex> = OnceLock::new();
            R.get_or_init(|| {
                Regex::new($expr).expect(concat!("invalid pattern: ", stringify!($name)))
            })
        }
    };
}

pattern!(
    swift_import,
    r"^import\s+(?:struct|class|enum|protocol|func|var|let|typealias)?\s*([A-Za-z_][\w.]*)"
);
pattern!(js_from, r#"from\s+["']([^"']+)["']"#);
pattern!(js_require, r#"require\s*\(\s*["']([^"']+)["']\s*\)"#);
pattern!(py_import, r"^import\s+([\w.]+)");
pattern!(py_from, r"^from\s+([\w.]+)\s+import");
pattern!(go_single_import, r#"^import\s+["']([^"']+)["']"#);
pattern!(go_block_line, r#"["']([^"']+)["']"#);
pattern!(rust_use, r"^use\s+([\w:]+)");
pattern!(rust_extern_crate, r"^extern\s+crate\s+(\w+)");
pattern!(jvm_import, r"^import\s+(?:static\s+)?([\w.]+)");
pattern!(c_include, r#"#include\s+[<"]([^>"]+)[>"]"#);
pattern!(ruby_require, r#"^require(?:_relative)?\s+['"]([^'"]+)['"]"#);
pattern!(php_use, r"^use\s+([\w\\]+)");
pattern!(
    php_include,
    r#"^(?:require|include)(?:_once)?\s+['"]([^'"]+)['"]"#
);
pattern!(csharp_using, r"^using\s+(?:static\s+)?([\w.]+)\s*;");
pattern!(shell_source, r#"^(?:source|\.)\s+['"]?([^\s'"]+)['"]?"#);
pattern!(dart_import, r#"^(?:import|export|part)\s+['"]([^'"]+)['"]"#);

#[cfg(test)]
mod tests {
    use super::extract_imports;

    #[test]
    fn rust_use_and_extern_crate() {
        let src = "use std::collections::HashMap;\nextern crate serde_json;\n";
        let imports = extract_imports(src, "rs");
        assert_eq!(imports, vec!["std::collections::HashMap", "serde_json"]);
    }

    #[test]
    fn typescript_imports() {
        let src = "import { Foo } from \"./foo\";\nconst y = require('./bar.js');\n";
        let imports = extract_imports(src, "ts");
        assert_eq!(imports, vec!["./foo", "./bar.js"]);
    }

    #[test]
    fn python_import_and_from() {
        let src = "# header\nimport os.path\nfrom collections import deque\n";
        let imports = extract_imports(src, "py");
        assert_eq!(imports, vec!["os.path", "collections"]);
    }

    #[test]
    fn go_block_imports() {
        let src = "import (\n  \"fmt\"\n  \"os\"\n)\nimport \"errors\"\n";
        let imports = extract_imports(src, "go");
        assert_eq!(imports, vec!["fmt", "os", "errors"]);
    }
}
