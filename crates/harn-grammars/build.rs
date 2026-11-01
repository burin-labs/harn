//! Compiles each enabled grammar with its parse tables moved out of the
//! binary's read-only data.
//!
//! A generated `parser.c` is mostly constant tables. For each grammar this
//! script:
//!
//! 1. builds and runs a host program that includes the grammar's original
//!    `parser.c` and writes every plain-data table's bytes and size;
//! 2. compresses those bytes into `OUT_DIR`, where `src/packed.rs` embeds them;
//! 3. compiles, for the target, a copy of `parser.c` whose tables are declared
//!    zero-filled and writable, so they occupy `.bss` instead of the file.
//!
//! The library fills the tables once, on the grammar's first use. The copy's
//! entry point is renamed so nothing links the upstream parser object by
//! accident; the upstream crate still supplies the external scanner. When a
//! grammar's sources cannot be found the build links the upstream crates
//! unchanged and says so.

use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::{env, fs, thread};

/// One tree-sitter grammar: the crate that ships it, the directory of its
/// `src/` inside that crate, and the C name after `tree_sitter_`.
struct Grammar {
    feature: &'static str,
    krate: &'static str,
    dir: &'static str,
    symbol: &'static str,
}

const fn grammar(
    feature: &'static str,
    krate: &'static str,
    dir: &'static str,
    symbol: &'static str,
) -> Grammar {
    Grammar {
        feature,
        krate,
        dir,
        symbol,
    }
}

const GRAMMARS: &[Grammar] = &[
    grammar("bash", "tree-sitter-bash", "", "bash"),
    grammar("c", "tree-sitter-c", "", "c"),
    grammar("c-sharp", "tree-sitter-c-sharp", "", "c_sharp"),
    grammar("cpp", "tree-sitter-cpp", "", "cpp"),
    grammar("css", "tree-sitter-css", "", "css"),
    grammar("elixir", "tree-sitter-elixir", "", "elixir"),
    grammar("go", "tree-sitter-go", "", "go"),
    grammar("harn", "tree-sitter-harn", "", "harn"),
    grammar("haskell", "tree-sitter-haskell", "", "haskell"),
    grammar("html", "tree-sitter-html", "", "html"),
    grammar("java", "tree-sitter-java", "", "java"),
    grammar("javascript", "tree-sitter-javascript", "", "javascript"),
    grammar("json", "tree-sitter-json", "", "json"),
    grammar("kotlin", "tree-sitter-kotlin-ng", "", "kotlin"),
    grammar("lua", "tree-sitter-lua", "", "lua"),
    grammar(
        "markdown",
        "tree-sitter-md",
        "tree-sitter-markdown",
        "markdown",
    ),
    grammar("php", "tree-sitter-php", "php", "php"),
    grammar("powershell", "tree-sitter-powershell", "", "powershell"),
    grammar("python", "tree-sitter-python", "", "python"),
    grammar("r", "tree-sitter-r", "", "r"),
    grammar("ruby", "tree-sitter-ruby", "", "ruby"),
    grammar("rust", "tree-sitter-rust", "", "rust"),
    grammar("scala", "tree-sitter-scala", "", "scala"),
    grammar("sql", "tree-sitter-sequel", "", "sql"),
    grammar("swift", "tree-sitter-swift", "", "swift"),
    grammar("toml", "tree-sitter-toml-ng", "", "toml"),
    grammar("typescript", "harn-grammars", "typescript", "typescript"),
    grammar("typescript", "harn-grammars", "tsx", "tsx"),
    grammar("yaml", "tree-sitter-yaml", "", "yaml"),
    grammar("zig", "tree-sitter-zig", "", "zig"),
];

/// Element types of the tables worth moving. None holds a pointer, so a
/// table's bytes are the same on the build host as on every target Harn ships
/// (all little-endian, with these types' sizes and alignment fixed).
const PLAIN_TYPES: &[&str] = &[
    "uint16_t",
    "uint32_t",
    "bool",
    "TSStateId",
    "TSSymbol",
    "TSParseActionEntry",
    "TSSymbolMetadata",
    "TSFieldMapEntry",
    "TSFieldMapSlice",
    "TSMapSlice",
    "TSLexMode",
    "TSLexerMode",
];

/// A table definition in a generated `parser.c`, spanning
/// `lines[start..=end]`.
struct Table {
    ty: String,
    name: String,
    dims: String,
    start: usize,
    end: usize,
}

struct Packed {
    symbol: &'static str,
    krate: &'static str,
    sizes: Vec<usize>,
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rustc-check-cfg=cfg(harn_grammars_packed)");

    let enabled: Vec<&Grammar> = GRAMMARS
        .iter()
        .filter(|g| feature_enabled(g.feature))
        .collect();
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    if enabled.is_empty() {
        write_bindings(&out, &[]);
        return;
    }
    if feature_enabled("typescript") {
        compile_typescript();
    }
    // The upstream crates carry the libc shims a wasm32 build needs.
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        return fallback("the target is wasm32");
    }
    // The dump program writes the build host's byte order; `cfg!` here
    // describes the host this script runs on.
    if env::var("CARGO_CFG_TARGET_ENDIAN").as_deref() != Ok("little") {
        return fallback("the target is not little-endian");
    }
    if cfg!(target_endian = "big") {
        return fallback("the build host is not little-endian");
    }
    let sources = match locate_sources(&enabled) {
        Ok(sources) => sources,
        Err(why) => return fallback(&why),
    };

    let queue = Mutex::new(enabled.iter().zip(sources).collect::<Vec<_>>());
    let results = Mutex::new(Vec::new());
    let workers = env::var("NUM_JOBS")
        .ok()
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, enabled.len());
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let Some((grammar, src)) = queue.lock().unwrap().pop() else {
                    break;
                };
                let packed = pack(grammar, &src, &out);
                results.lock().unwrap().push(packed);
            });
        }
    });
    let mut packed = results.into_inner().unwrap();
    packed.sort_by_key(|p| p.symbol);
    write_bindings(&out, &packed);
    println!("cargo:rustc-cfg=harn_grammars_packed");
}

fn feature_enabled(feature: &str) -> bool {
    let var = format!(
        "CARGO_FEATURE_{}",
        feature.to_ascii_uppercase().replace('-', "_")
    );
    env::var_os(var).is_some()
}

fn fallback(why: &str) {
    println!("cargo:warning=harn-grammars: linking unpacked grammars because {why}");
}

fn compile_typescript() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"))
        .join("vendor/tree-sitter-typescript");
    let mut build = cc::Build::new();
    build.std("c11").warnings(false);
    for dialect in ["typescript", "tsx"] {
        let src = root.join(dialect).join("src");
        build.include(&src);
        for file in ["parser.c", "scanner.c"] {
            let path = src.join(file);
            println!("cargo:rerun-if-changed={}", path.display());
            build.file(path);
        }
    }
    println!(
        "cargo:rerun-if-changed={}",
        root.join("common/scanner.h").display()
    );
    build.compile("harn_typescript_grammars");
}

/// Finds each grammar's `src/` directory from the exact version this crate
/// pins, in Cargo's registry or at the dependency's path.
fn locate_sources(enabled: &[&Grammar]) -> Result<Vec<PathBuf>, String> {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let manifest = fs::read_to_string(manifest_dir.join("Cargo.toml"))
        .map_err(|e| format!("Cargo.toml is unreadable: {e}"))?;
    let manifest: toml::Table = manifest
        .parse()
        .map_err(|e| format!("Cargo.toml does not parse: {e}"))?;
    let deps = manifest
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .ok_or("Cargo.toml has no [dependencies]")?;
    let lock_path = manifest_dir
        .ancestors()
        .map(|dir| dir.join("Cargo.lock"))
        .find(|path| path.is_file())
        .ok_or("Cargo.lock is unavailable")?;
    println!("cargo:rerun-if-changed={}", lock_path.display());
    let lock: toml::Table = fs::read_to_string(&lock_path)
        .map_err(|e| format!("Cargo.lock is unreadable: {e}"))?
        .parse()
        .map_err(|e| format!("Cargo.lock does not parse: {e}"))?;
    let packages = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or("Cargo.lock has no packages")?;
    let registry = registry_roots();
    enabled
        .iter()
        .map(|grammar| {
            if grammar.krate == "harn-grammars" {
                return Ok(manifest_dir
                    .join("vendor/tree-sitter-typescript")
                    .join(grammar.dir)
                    .join("src"));
            }
            let dep = deps
                .get(grammar.krate)
                .and_then(toml::Value::as_table)
                .ok_or_else(|| format!("{} is not a dependency", grammar.krate))?;
            // Registry sources never change under a pinned version, so only a
            // path dependency's parser is watched. Its path is spelled as
            // Cargo spells it, so the dependency lists one file once.
            let root = if let Some(path) = dep.get("path").and_then(toml::Value::as_str) {
                let root = normalize(&manifest_dir.join(path));
                let parser = root.join(grammar.dir).join("src").join("parser.c");
                println!("cargo:rerun-if-changed={}", parser.display());
                root
            } else {
                let version = dep
                    .get("version")
                    .and_then(toml::Value::as_str)
                    .and_then(|v| v.strip_prefix('='))
                    .ok_or_else(|| format!("{} is not pinned with `=`", grammar.krate))?;
                let dir = format!("{}-{version}", grammar.krate);
                let matches: Vec<_> = packages
                    .iter()
                    .filter(|package| {
                        package.get("name").and_then(toml::Value::as_str) == Some(grammar.krate)
                            && package.get("version").and_then(toml::Value::as_str) == Some(version)
                    })
                    .collect();
                let [package] = matches.as_slice() else {
                    return Err(format!(
                        "{} has an ambiguous resolved package",
                        grammar.krate
                    ));
                };
                if package.get("source").and_then(toml::Value::as_str)
                    != Some("registry+https://github.com/rust-lang/crates.io-index")
                {
                    return Err(format!("{} has a non-registry override", grammar.krate));
                }
                let checksum = package
                    .get("checksum")
                    .and_then(toml::Value::as_str)
                    .ok_or_else(|| format!("{} has no registry checksum", grammar.krate))?;
                registry
                    .iter()
                    .find_map(|index| {
                        let root = index.join(&dir);
                        let archive = index
                            .parent()?
                            .parent()?
                            .join("cache")
                            .join(index.file_name()?)
                            .join(format!("{dir}.crate"));
                        let matches = fs::read(archive)
                            .is_ok_and(|data| format!("{:x}", Sha256::digest(data)) == checksum);
                        (matches && root.is_dir()).then_some(root)
                    })
                    .ok_or_else(|| format!("{dir} is not in Cargo's registry"))?
            };
            let src = root.join(grammar.dir).join("src");
            if src.join("parser.c").is_file() {
                Ok(src)
            } else {
                Err(format!("{} has no parser.c", src.display()))
            }
        })
        .collect()
}

/// `path` with `.` and `..` removed lexically, as Cargo resolves a path
/// dependency.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn registry_roots() -> Vec<PathBuf> {
    let cargo_home = env::var_os("CARGO_HOME").map(PathBuf::from).or_else(|| {
        env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(|home| PathBuf::from(home).join(".cargo"))
    });
    let Some(src) = cargo_home.map(|home| home.join("registry").join("src")) else {
        return Vec::new();
    };
    let mut roots: Vec<PathBuf> = fs::read_dir(src)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    roots.sort();
    roots
}

fn pack(grammar: &Grammar, src: &Path, out: &Path) -> Packed {
    let parser = src.join("parser.c");
    let scanner = src.join("scanner.c");
    let has_scanner = scanner.is_file();
    let text =
        fs::read_to_string(&parser).unwrap_or_else(|e| panic!("reading {}: {e}", parser.display()));
    let lines: Vec<&str> = text.lines().collect();
    let tables = find_tables(&lines);
    assert!(
        tables.iter().any(|t| t.name == "ts_parse_table"),
        "{}: found no ts_parse_table definition",
        parser.display()
    );

    let dir = out.join(grammar.symbol);
    fs::create_dir_all(&dir).expect("create grammar dir");
    let (bytes, sizes, counts) = dump_tables(grammar, src, has_scanner, &tables, &dir);

    let compressed = zstd::bulk::compress(&bytes, 19).expect("compress tables");
    fs::write(
        out.join(format!("{}.tables.zst", grammar.symbol)),
        compressed,
    )
    .expect("write packed tables");

    // Not `parser.c`: the dump program's `#include "parser.c"` would find it
    // beside itself on the next build.
    let packed_parser = dir.join("packed_parser.c");
    fs::write(&packed_parser, rewrite(grammar, &lines, &tables, &counts))
        .expect("write packed parser.c");

    // The external scanner stays the upstream crate's object: scanners define
    // ordinary globals, so a second copy would collide with it.
    let entry = format!("tree_sitter_{}", grammar.symbol);
    let mut build = cc::Build::new();
    build
        .std("c11")
        .include(src)
        .warnings(false)
        .define(&entry, format!("harn_packed_{entry}").as_str())
        .file(&packed_parser);
    if build.get_compiler().is_like_msvc() {
        build.flag("-utf-8");
    }
    build.compile(&format!("harn_grammar_{}", grammar.symbol));
    Packed {
        symbol: grammar.symbol,
        krate: grammar.krate,
        sizes,
    }
}

/// Every top-level `static const <plain type> ts_*[...] = {` definition.
fn find_tables(lines: &[&str]) -> Vec<Table> {
    let mut tables = Vec::new();
    let mut seen = BTreeSet::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let parsed = line
            .strip_prefix("static const ")
            .and_then(|rest| rest.strip_suffix(" = {"))
            .and_then(|decl| decl.split_once(' '))
            .and_then(|(ty, rest)| {
                let (name, dims) = rest.split_at(rest.find('[')?);
                Some((ty, name, dims))
            });
        if let Some((ty, name, dims)) = parsed {
            if PLAIN_TYPES.contains(&ty) && name.starts_with("ts_") && dims.ends_with(']') {
                let end = (i + 1..lines.len())
                    .find(|&j| lines[j] == "};")
                    .unwrap_or_else(|| panic!("table {name} is never closed"));
                assert!(seen.insert(name.to_string()), "table {name} defined twice");
                tables.push(Table {
                    ty: ty.to_string(),
                    name: name.to_string(),
                    dims: dims.to_string(),
                    start: i,
                    end,
                });
                i = end;
            }
        }
        i += 1;
    }
    tables
}

/// Builds and runs a host program that includes the original `parser.c` and
/// writes each table's bytes. Returns the concatenated bytes, each table's
/// size in bytes, and each table's element count.
fn dump_tables(
    grammar: &Grammar,
    src: &Path,
    has_scanner: bool,
    tables: &[Table],
    dir: &Path,
) -> (Vec<u8>, Vec<usize>, Vec<usize>) {
    let mut program = String::from("#include <stdio.h>\n#include \"parser.c\"\n");
    program.push_str("int main(int argc, char **argv) {\n");
    program.push_str("  FILE *out = argc > 1 ? fopen(argv[1], \"wb\") : NULL;\n");
    program.push_str("  if (!out) return 2;\n");
    for table in tables {
        let n = &table.name;
        let _ = writeln!(
            program,
            "  if (fwrite({n}, 1, sizeof {n}, out) != sizeof {n}) return 3;\n  \
             printf(\"%lu %lu\\n\", (unsigned long)sizeof {n}, \
             (unsigned long)(sizeof {n} / sizeof {n}[0]));"
        );
    }
    program.push_str("  return fclose(out) != 0;\n}\n");
    let source = dir.join("dump.c");
    fs::write(&source, program).expect("write dump.c");

    let host = env::var("HOST").expect("HOST");
    let compiler = cc::Build::new()
        .target(&host)
        .host(&host)
        .opt_level(0)
        .debug(false)
        .cargo_metadata(false)
        .warnings(false)
        .get_compiler();
    let exe = dir.join(if host.contains("windows") {
        "dump.exe"
    } else {
        "dump"
    });
    let mut cmd = compiler.to_command();
    if compiler.is_like_msvc() {
        cmd.arg("/nologo")
            .arg("/w")
            .arg("/std:c11")
            .arg("/utf-8")
            .arg(format!("/I{}", src.display()))
            .arg(format!("/Fo{}\\", dir.display()))
            .arg(format!("/Fe{}", exe.display()));
    } else {
        cmd.arg("-w")
            .arg("-std=c11")
            .arg("-I")
            .arg(src)
            .arg("-o")
            .arg(&exe);
    }
    cmd.arg(&source);
    if has_scanner {
        cmd.arg(src.join("scanner.c"));
    }
    run(&mut cmd, grammar.symbol);

    let blob = dir.join("tables.bin");
    let stdout = run(Command::new(&exe).arg(&blob), grammar.symbol);
    let mut sizes = Vec::new();
    let mut counts = Vec::new();
    for line in stdout.lines() {
        let (size, count) = line.split_once(' ').expect("dump output");
        sizes.push(size.parse().expect("table size"));
        counts.push(count.parse().expect("table count"));
    }
    assert_eq!(sizes.len(), tables.len(), "{}: dump output", grammar.symbol);
    let bytes = fs::read(&blob).expect("read dumped tables");
    assert_eq!(bytes.len(), sizes.iter().sum::<usize>());
    (bytes, sizes, counts)
}

fn run(cmd: &mut Command, what: &str) -> String {
    let output = cmd
        .output()
        .unwrap_or_else(|e| panic!("{what}: running {cmd:?}: {e}"));
    assert!(
        output.status.success(),
        "{what}: {cmd:?} failed with {}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8 output")
}

/// The original `parser.c` with every table declared zero-filled and
/// writable, plus an accessor the library uses to fill them.
fn rewrite(grammar: &Grammar, lines: &[&str], tables: &[Table], counts: &[usize]) -> String {
    let mut text = String::with_capacity(lines.len() * 16);
    let mut next = 0;
    for (table, count) in tables.iter().zip(counts) {
        for line in &lines[next..table.start] {
            text.push_str(line);
            text.push('\n');
        }
        let dims = if table.dims == "[]" {
            format!("[{count}]")
        } else {
            table.dims.clone()
        };
        let _ = writeln!(text, "static {} {}{dims};", table.ty, table.name);
        next = table.end + 1;
    }
    for line in &lines[next..] {
        text.push_str(line);
        text.push('\n');
    }
    let _ = writeln!(
        text,
        "\n#include <stddef.h>\n\
         size_t harn_packed_tables_{}(void **tables, size_t *sizes, size_t capacity) {{",
        grammar.symbol
    );
    for (i, table) in tables.iter().enumerate() {
        let n = &table.name;
        let _ = writeln!(
            text,
            "  if ({i} < capacity) {{ tables[{i}] = (void *){n}; sizes[{i}] = sizeof {n}; }}"
        );
    }
    let _ = writeln!(text, "  return {};\n}}", tables.len());
    text
}

fn write_bindings(out: &Path, packed: &[Packed]) {
    let mut rust = String::new();
    // Link each upstream crate for its external scanner object. Its parser
    // object stays out: nothing references the upstream entry point.
    let crates: BTreeSet<&str> = packed
        .iter()
        .map(|p| p.krate)
        .filter(|name| *name != "harn-grammars")
        .collect();
    for krate in crates {
        let _ = writeln!(rust, "extern crate {} as _;", krate.replace('-', "_"));
    }
    for p in packed {
        let s = p.symbol;
        let upper = s.to_ascii_uppercase();
        let _ = write!(
            rust,
            "extern \"C\" {{\n    \
                 fn harn_packed_tree_sitter_{s}() -> *const ();\n    \
                 fn harn_packed_tables_{s}(tables: *mut *mut u8, sizes: *mut usize, capacity: usize) -> usize;\n\
             }}\n\
             pub(crate) static {upper}: Packed = Packed {{\n    \
                 name: \"{s}\",\n    \
                 blob: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{s}.tables.zst\")),\n    \
                 sizes: &{sizes:?},\n    \
                 tables: harn_packed_tables_{s},\n    \
                 entry: harn_packed_tree_sitter_{s},\n    \
                 ready: std::sync::OnceLock::new(),\n\
             }};\n",
            sizes = p.sizes,
        );
    }
    fs::write(out.join("packed_grammars.rs"), rust).expect("write packed_grammars.rs");
}
