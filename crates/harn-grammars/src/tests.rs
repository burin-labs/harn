//! Every packed grammar must parse exactly as the upstream crate does.

use tree_sitter::{Language, Parser};

fn sexp(language: &Language, source: &str) -> String {
    let mut parser = Parser::new();
    parser.set_language(language).expect("compatible grammar");
    let tree = parser.parse(source, None).expect("parse");
    tree.root_node().to_sexp()
}

macro_rules! same_as_upstream {
    ($($feature:literal => $name:ident, $upstream:path, $source:expr;)*) => {
        $(
            #[test]
            #[cfg(feature = $feature)]
            fn $name() {
                let packed = sexp(&super::$name(), $source);
                assert_eq!(packed, sexp(&$upstream.into(), $source));
                assert!(!packed.contains("ERROR"), "{packed}");
            }
        )*
    };
}

same_as_upstream! {
    "bash" => bash, tree_sitter_bash::LANGUAGE, "for f in *.rs; do echo \"$f\" | wc -l; done\n";
    "c" => c, tree_sitter_c::LANGUAGE, "int main(void) { return sizeof(struct s *); }\n";
    "c-sharp" => c_sharp, tree_sitter_c_sharp::LANGUAGE, "class A { async Task<int> F() => await G(x => x + 1); }\n";
    "cpp" => cpp, tree_sitter_cpp::LANGUAGE, "template <class T> auto f(T&& t) -> decltype(t) { return std::forward<T>(t); }\n";
    "css" => css, tree_sitter_css::LANGUAGE, "a:hover > b { color: rgb(1 2 3 / 50%); }\n";
    "elixir" => elixir, tree_sitter_elixir::LANGUAGE, "defmodule A do\n  def f(x) when x > 0, do: x |> g()\nend\n";
    "go" => go, tree_sitter_go::LANGUAGE, "package a\nfunc F[T any](c chan<- T) { go func() { c <- *new(T) }() }\n";
    "harn" => harn, tree_sitter_harn::LANGUAGE, "fn main(harness: Harness) {\n  let x = [1, 2]\n  println(x)\n}\n";
    "haskell" => haskell, tree_sitter_haskell::LANGUAGE, "module A where\nf :: Maybe a -> [a]\nf (Just x) = [x | _ <- [1..3]]\n";
    "html" => html, tree_sitter_html::LANGUAGE, "<p class=a>Hi <b>there</b><br></p>\n";
    "java" => java, tree_sitter_java::LANGUAGE, "class A { <T> List<T> f() { return switch (x) { case 1 -> List.of(); default -> null; }; } }\n";
    "javascript" => javascript, tree_sitter_javascript::LANGUAGE, "const f = async ({a, ...b}) => <div>{a?.[b] ?? `t${b}`}</div>;\n";
    "json" => json, tree_sitter_json::LANGUAGE, "{\"a\": [1, 2.5e3, true, null, \"\\u00e9\"]}\n";
    "kotlin" => kotlin, tree_sitter_kotlin_ng::LANGUAGE, "suspend fun <T> f(x: T?): List<T> = listOfNotNull(x).map { it }\n";
    "lua" => lua, tree_sitter_lua::LANGUAGE, "local function f(...) return #{...}, [[long]] end\n";
    "markdown" => markdown, tree_sitter_md::LANGUAGE, "# Title\n\n- item\n  1. nested\n\n```rust\nfn f() {}\n```\n";
    "php" => php, tree_sitter_php::LANGUAGE_PHP, "<?php\nfunction f(int ...$a): ?array { return match(true) { default => $a }; }\n";
    "powershell" => powershell, tree_sitter_powershell::LANGUAGE, "Get-ChildItem -Recurse | Where-Object { $_.Length -gt 1kb } | ForEach-Object { $_.Name }\n";
    "python" => python, tree_sitter_python::LANGUAGE, "async def f(*a, **k):\n    match a:\n        case [x, *_]: return f'{x!r}'\n";
    "r" => r, tree_sitter_r::LANGUAGE, "f <- function(x, ...) x |> sapply(\\(y) y[[1]])\n";
    "ruby" => ruby, tree_sitter_ruby::LANGUAGE, "class A < B\n  def f(*a, &b) = a.map { |x| x ** 2 }\nend\n";
    "rust" => rust, tree_sitter_rust::LANGUAGE, "fn f<'a, T: Iterator<Item = &'a str>>(t: T) -> impl Fn() { move || { let _ = r#\"x\"#; } }\n";
    "scala" => scala, tree_sitter_scala::LANGUAGE, "object A:\n  def f[T](xs: List[T]): Int = xs match\n    case h :: _ => 1\n    case Nil => 0\n";
    "sql" => sql, tree_sitter_sequel::LANGUAGE, "SELECT a, count(*) FROM t WHERE b IN (1, 2) GROUP BY a ORDER BY 2 DESC;\n";
    "swift" => swift, tree_sitter_swift::LANGUAGE, "func f<T: Equatable>(_ x: T?) async throws -> [T] { guard let x else { return [] }; return [x] }\n";
    "toml" => toml, tree_sitter_toml_ng::LANGUAGE, "[a.b]\nc = { d = [1, 2], e = 1979-05-27T07:32:00Z }\n";
    "typescript" => typescript, super::typescript::LANGUAGE_TYPESCRIPT, "type A<T> = { [K in keyof T]?: T[K] extends string ? K : never };\n";
    "typescript" => tsx, super::typescript::LANGUAGE_TSX, "const C = <T,>(p: { x: T }) => <A x={p.x as any}>{p.x}</A>;\n";
    "yaml" => yaml, tree_sitter_yaml::LANGUAGE, "a: &x\n  - b: |\n      text\n  - *x\n";
    "zig" => zig, tree_sitter_zig::LANGUAGE, "pub fn f(comptime T: type, x: ?T) !T { return x orelse error.None; }\n";
}

/// The comparisons above pass vacuously when the build falls back to the
/// upstream grammars, so a fallback fails here instead.
#[test]
#[cfg(not(harn_grammars_packed))]
fn this_workspace_builds_packed_grammars() {
    panic!("the build script fell back to unpacked grammars; see its warning");
}
