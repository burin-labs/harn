//! Package-manifest dependency extraction. Ports
//! `ManifestDependencyParser.swift` from Burin Code so the deterministic
//! "what dependencies does this project declare" fact is computed once, in
//! the cross-platform Rust scanner, instead of duplicated in Swift.
//!
//! The parsing is intentionally lexical (no real TOML / YAML / XML parser
//! dependency beyond `serde_json` for the two JSON manifests) so every
//! supported ecosystem stays in this one file. Coverage mirrors
//! [`super::subproject::MARKERS`] and the `packageManifests` field of every
//! entry in Burin's `data/language-catalog.json`.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use regex::Regex;
use std::sync::OnceLock;

/// A lexical parser mapping one manifest's text to its declared dependency
/// identifiers.
type ManifestParser = fn(&str) -> BTreeSet<String>;

/// Manifest file names recognized by a fixed-name parser, paired with the
/// parser function. Order is irrelevant — each manifest contributes
/// independently and the union is returned. `*.csproj` files are handled
/// separately in [`directory_dependencies`] because their names vary.
const PARSERS: &[(&str, ManifestParser)] = &[
    ("package.json", package_json),
    ("composer.json", composer_json),
    ("pyproject.toml", pyproject_toml),
    ("Cargo.toml", cargo_toml),
    ("go.mod", go_module),
    ("Gemfile", ruby_gemfile),
    ("pubspec.yaml", pubspec_yaml),
    ("build.sbt", sbt),
    ("build.gradle", gradle_dependencies),
    ("build.gradle.kts", gradle_dependencies),
    ("pom.xml", maven_pom),
    ("Package.swift", swift_package),
    ("mix.exs", mix_exs),
    ("Directory.Build.props", msbuild_xml),
];

/// Parse a single manifest's `content` given its `file_name`. Returns an
/// empty set for unrecognized names (including `*.csproj`, which
/// [`directory_dependencies`] routes through [`msbuild_xml`] directly).
pub fn parse_manifest(file_name: &str, content: &str) -> BTreeSet<String> {
    for (name, parse) in PARSERS {
        if *name == file_name {
            return parse(content);
        }
    }
    BTreeSet::new()
}

/// Aggregated dependencies declared in any recognized manifest located
/// directly under `directory` (non-recursive — siblings of nested
/// sub-projects are deliberately ignored to keep the result scoped to one
/// project root at a time). The returned vector is sorted and de-duplicated.
pub fn directory_dependencies(directory: &Path) -> Vec<String> {
    let mut deps: BTreeSet<String> = BTreeSet::new();
    for (manifest_name, _) in PARSERS {
        let path = directory.join(manifest_name);
        if let Ok(content) = fs::read_to_string(&path) {
            deps.extend(parse_manifest(manifest_name, &content));
        }
    }
    // `.csproj` filenames vary per project; pick them up by extension.
    if let Ok(entries) = fs::read_dir(directory) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.ends_with(".csproj") {
                if let Ok(content) = fs::read_to_string(entry.path()) {
                    deps.extend(msbuild_xml(&content));
                }
            }
        }
    }
    deps.into_iter().collect()
}

// MARK: - Per-manifest parsers

/// `package.json` — JSON object with `dependencies` / `devDependencies` /
/// `peerDependencies` / `optionalDependencies` keys.
fn package_json(content: &str) -> BTreeSet<String> {
    let mut deps = BTreeSet::new();
    let json: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(_) => return deps,
    };
    for key in [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ] {
        if let Some(map) = json.get(key).and_then(|v| v.as_object()) {
            deps.extend(map.keys().cloned());
        }
    }
    deps
}

/// `composer.json` — same shape as `package.json`, different key names.
/// Skips the PHP runtime + `ext-*` extension pseudo-dependencies.
fn composer_json(content: &str) -> BTreeSet<String> {
    let mut deps = BTreeSet::new();
    let json: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(_) => return deps,
    };
    for key in ["require", "require-dev"] {
        if let Some(map) = json.get(key).and_then(|v| v.as_object()) {
            deps.extend(
                map.keys()
                    .filter(|k| *k != "php" && !k.starts_with("ext-"))
                    .cloned(),
            );
        }
    }
    deps
}

/// `pyproject.toml` — quoted strings under `[project] dependencies`,
/// `[project.optional-dependencies.*]`, `[tool.poetry.dependencies]`, and
/// `[build-system] requires`.
fn pyproject_toml(content: &str) -> BTreeSet<String> {
    let mut deps = BTreeSet::new();
    // Quoted-string scan (covers PEP 621 lists). Conservative: any quoted
    // identifier inside a dependency-flavored array becomes a candidate,
    // with PEP 508 markers stripped.
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('"') || trimmed.starts_with('\'') {
            let stripped: String = trimmed
                .trim_matches(['"', '\'', ' '])
                .split(['>', '<', '=', '!', '~', ';', ' ', '['])
                .next()
                .unwrap_or("")
                .to_string();
            if !stripped.is_empty() && !stripped.contains('[') {
                deps.insert(stripped);
            }
        }
    }
    // Poetry-style `name = "version"` under `[tool.poetry.dependencies]`.
    const POETRY_SECTIONS: &[&str] = &[
        "[tool.poetry.dependencies]",
        "[tool.poetry.dev-dependencies]",
        "[tool.poetry.group.dev.dependencies]",
    ];
    let mut current_section = "";
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            current_section = trimmed;
            continue;
        }
        if !POETRY_SECTIONS.contains(&current_section) {
            continue;
        }
        if let Some((name, _)) = trimmed.split_once('=') {
            let name = name.trim();
            if !name.is_empty() && name != "python" {
                deps.insert(name.to_string());
            }
        }
    }
    deps
}

/// `Cargo.toml` — `name = …` lines under any `[*dependencies*]` table.
fn cargo_toml(content: &str) -> BTreeSet<String> {
    let mut deps = BTreeSet::new();
    let mut in_deps = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            // Matches `[dependencies]`, `[dev-dependencies]`,
            // `[build-dependencies]`, `[target.x.dependencies]`.
            in_deps = trimmed.contains("dependencies");
            continue;
        }
        if in_deps {
            if let Some((name, _)) = trimmed.split_once('=') {
                let name = name.trim();
                if !name.is_empty() {
                    deps.insert(name.to_string());
                }
            }
        }
    }
    deps
}

/// `go.mod` — paths inside `require ( … )` blocks plus single-line `require`.
fn go_module(content: &str) -> BTreeSet<String> {
    let mut deps = BTreeSet::new();
    let mut in_require = false;
    for raw_line in content.lines() {
        let trimmed = raw_line.trim();
        if trimmed.starts_with("//") || trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("require (") {
            in_require = true;
            continue;
        }
        if in_require {
            if trimmed == ")" {
                in_require = false;
                continue;
            }
            if let Some(module) = trimmed.split(' ').next() {
                if module.contains('/') {
                    deps.insert(module.to_string());
                }
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("require ") {
            if let Some(module) = rest.split(' ').next() {
                if module.contains('/') {
                    deps.insert(module.to_string());
                }
            }
        }
    }
    deps
}

/// `Gemfile` — `gem '<name>'` / `gem "<name>"` lines.
fn ruby_gemfile(content: &str) -> BTreeSet<String> {
    let mut deps = BTreeSet::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if !(trimmed.starts_with("gem ") || trimmed.starts_with("gem\t")) {
            continue;
        }
        if let Some(name) = quoted_name(trimmed) {
            deps.insert(name);
        }
    }
    deps
}

/// `pubspec.yaml` — top-level `name:` entries under `dependencies:` /
/// `dev_dependencies:` / `dependency_overrides:`. SDK constraints are
/// skipped.
fn pubspec_yaml(content: &str) -> BTreeSet<String> {
    const SECTIONS: &[&str] = &[
        "dependencies:",
        "dev_dependencies:",
        "dependency_overrides:",
    ];
    let mut current = "";
    let mut deps = BTreeSet::new();
    for raw_line in content.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let starts_at_root = !raw_line.starts_with([' ', '\t']);
        if starts_at_root {
            current = if SECTIONS.contains(&trimmed) {
                trimmed
            } else {
                ""
            };
            continue;
        }
        if current.is_empty() {
            continue;
        }
        // Capture `name:` or `name: version` entries that sit at the
        // section's first nesting level. Sub-keys of an inline block (e.g.
        // `git:` / `path:` under `mypkg:`) have a deeper indent and are
        // ignored here.
        let leading = raw_line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .count();
        if leading > 2 {
            continue;
        }
        let name = trimmed.split(':').next().unwrap_or("");
        if name.is_empty() || name == "sdk" {
            continue;
        }
        deps.insert(name.trim().to_string());
    }
    deps
}

/// `build.sbt` — `libraryDependencies += "org" %% "name" % "ver"` and the
/// `++= Seq(…)` variants. For the `org % name % version` triple the middle
/// quoted token is the artifact name; for `Seq("a", "b")` every quoted
/// token is its own dependency name.
fn sbt(content: &str) -> BTreeSet<String> {
    let mut deps = BTreeSet::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if !(trimmed.contains("libraryDependencies") || trimmed.starts_with('"')) {
            continue;
        }
        let quoted = quoted_tokens(trimmed);
        if quoted.len() == 3 && trimmed.contains('%') {
            deps.insert(quoted[1].clone());
        } else {
            deps.extend(quoted);
        }
    }
    deps
}

const GRADLE_CONFIGURATIONS: &[&str] = &[
    "implementation",
    "api",
    "compileOnly",
    "runtimeOnly",
    "testImplementation",
    "testRuntimeOnly",
    "testCompileOnly",
    "annotationProcessor",
    "ksp",
    "kapt",
];

/// `build.gradle` (Groovy) and `build.gradle.kts` (Kotlin) — quoted Maven
/// coordinates inside `implementation`, `api`, `testImplementation`, etc.
/// Returns the artifact name (middle component of `group:name:version`).
fn gradle_dependencies(content: &str) -> BTreeSet<String> {
    let mut deps = BTreeSet::new();
    for line in content.lines() {
        let trimmed = line.trim();
        let first_token = trimmed.split(['(', ' ', '\t']).next().unwrap_or("");
        if !GRADLE_CONFIGURATIONS.contains(&first_token) {
            continue;
        }
        for token in quoted_tokens(trimmed) {
            if token.contains(':') {
                let parts: Vec<&str> = token.split(':').collect();
                if parts.len() >= 2 && !parts[1].is_empty() {
                    deps.insert(parts[1].to_string());
                }
            }
        }
    }
    deps
}

/// `pom.xml` — `<dependency> … <artifactId>name</artifactId> … </dependency>`
/// tags. Matches all `<artifactId>` content; for Maven projects the parent /
/// plugin artifact ids end up in the same set, which is the behavior we want
/// here (any artifact the project knows about is "available").
fn maven_pom(content: &str) -> BTreeSet<String> {
    let mut deps = BTreeSet::new();
    let mut remainder = content;
    while let Some(open) = remainder.find("<artifactId>") {
        let after_open = open + "<artifactId>".len();
        let rest = &remainder[after_open..];
        let close = match rest.find("</artifactId>") {
            Some(c) => c,
            None => break,
        };
        let name = rest[..close].trim();
        if !name.is_empty() {
            deps.insert(name.to_string());
        }
        remainder = &rest[close + "</artifactId>".len()..];
    }
    deps
}

/// `Package.swift` — quoted package URLs / names inside `.package(...)`
/// entries. Returns the last path component (or `name:` value) — the shape
/// SwiftPM products use to refer to a dependency.
fn swift_package(content: &str) -> BTreeSet<String> {
    let mut deps = BTreeSet::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.contains(".package(") {
            continue;
        }
        if let Some(idx) = trimmed.find("name:") {
            let after = &trimmed[idx + "name:".len()..];
            if let Some(name) = quoted_name(after) {
                deps.insert(name);
                continue;
            }
        }
        if let Some(url) = quoted_name(trimmed) {
            let stripped = url.strip_suffix(".git").unwrap_or(&url);
            let last = stripped.rsplit('/').next().unwrap_or(stripped);
            if !last.is_empty() {
                deps.insert(last.to_string());
            }
        }
    }
    deps
}

/// `mix.exs` — `{:name, "~> version"}` tuples inside the `deps` callback.
/// The atom (`:name`) is the dependency identifier.
fn mix_exs(content: &str) -> BTreeSet<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\{\s*:([a-z][a-z0-9_]*)").unwrap());
    let mut deps = BTreeSet::new();
    for caps in re.captures_iter(content) {
        if let Some(name) = caps.get(1) {
            deps.insert(name.as_str().to_string());
        }
    }
    deps
}

/// `Directory.Build.props` / `*.csproj` — `<PackageReference Include="name" .../>`
/// elements (any whitespace / attribute order). Doubles as the parser for
/// project-level `.csproj` files.
fn msbuild_xml(content: &str) -> BTreeSet<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r#"<PackageReference\b[^>]*\bInclude\s*=\s*"([^"]+)""#).unwrap()
    });
    let mut deps = BTreeSet::new();
    for caps in re.captures_iter(content) {
        if let Some(name) = caps.get(1) {
            deps.insert(name.as_str().to_string());
        }
    }
    deps
}

// MARK: - Lexical helpers

/// Return the first quoted (`'...'` or `"..."`) substring in `line`.
fn quoted_name(line: &str) -> Option<String> {
    quoted_tokens(line).into_iter().next()
}

/// Return every quoted (`'...'` or `"..."`) substring in `line`, in order.
/// Used by parsers whose syntax embeds Maven coordinates or
/// `(group, name, version)` triples as quoted strings.
fn quoted_tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in line.chars() {
        if let Some(active) = quote {
            if ch == active {
                tokens.push(std::mem::take(&mut current));
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            current.clear();
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;
    use tempfile::tempdir;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn package_json_includes_every_dependency_bucket() {
        let content = r#"{
          "dependencies": {"express": "^4"},
          "devDependencies": {"jest": "^29"},
          "peerDependencies": {"react": "^18"},
          "optionalDependencies": {"fsevents": "^2"}
        }"#;
        assert_eq!(
            package_json(content),
            set(&["express", "jest", "react", "fsevents"])
        );
    }

    #[test]
    fn composer_json_skips_runtime_and_extension_pseudo_deps() {
        let content = r#"{
          "require": {"php": ">=8.1", "ext-curl": "*", "symfony/console": "^6.0"},
          "require-dev": {"phpunit/phpunit": "^10"}
        }"#;
        assert_eq!(
            composer_json(content),
            set(&["symfony/console", "phpunit/phpunit"])
        );
    }

    #[test]
    fn pyproject_toml_handles_pep621_and_poetry() {
        let content = r#"[project]
name = "demo"
dependencies = [
  "requests>=2.31",
  "pydantic==2.0",
]

[tool.poetry.dependencies]
python = "^3.11"
rich = "^13.0"
"#;
        let deps = pyproject_toml(content);
        assert!(deps.contains("requests"));
        assert!(deps.contains("pydantic"));
        assert!(deps.contains("rich"));
        assert!(!deps.contains("python"));
    }

    #[test]
    fn cargo_toml_picks_up_dev_and_build_dependencies() {
        let content = r#"[package]
name = "demo"

[dependencies]
serde = "1"

[dev-dependencies]
proptest = "1"

[build-dependencies]
cc = "1"
"#;
        assert_eq!(cargo_toml(content), set(&["serde", "proptest", "cc"]));
    }

    #[test]
    fn go_module_parses_require_blocks_and_single_lines() {
        let content = r"module example.com/app

go 1.22

require github.com/foo/single v1.0.0

require (
  github.com/foo/bar v1.0.0
  github.com/baz/qux v0.5.0 // indirect
)
";
        assert_eq!(
            go_module(content),
            set(&[
                "github.com/foo/single",
                "github.com/foo/bar",
                "github.com/baz/qux"
            ])
        );
    }

    #[test]
    fn ruby_gemfile_extracts_gem_names() {
        let content = r#"source "https://rubygems.org"

gem "rails", "~> 7.1"
gem 'pg', '~> 1.5'
group :development do
  gem "rspec-rails"
end
"#;
        assert_eq!(ruby_gemfile(content), set(&["rails", "pg", "rspec-rails"]));
    }

    #[test]
    fn pubspec_yaml_keeps_top_level_dependency_entries() {
        let content = r#"name: demo
environment:
  sdk: ">=3.0.0 <4.0.0"

dependencies:
  http: ^1.1.0
  path:
    git:
      url: https://github.com/dart-lang/path
      ref: main
dev_dependencies:
  test: ^1.24.0
"#;
        let deps = pubspec_yaml(content);
        assert!(deps.contains("http"));
        assert!(deps.contains("path"));
        assert!(deps.contains("test"));
        assert!(!deps.contains("sdk"));
        assert!(!deps.contains("git"));
        assert!(!deps.contains("url"));
    }

    #[test]
    fn sbt_extracts_artifact_name_from_coordinate_triple() {
        let content = r#"name := "demo"
libraryDependencies += "org.typelevel" %% "cats-core" % "2.10.0"
libraryDependencies ++= Seq(
  "org.scalatest" %% "scalatest" % "3.2.17" % Test,
  "io.circe" %% "circe-core" % "0.14.0"
)
"#;
        let deps = sbt(content);
        assert!(deps.contains("cats-core"));
        assert!(deps.contains("scalatest"));
        assert!(deps.contains("circe-core"));
    }

    #[test]
    fn gradle_groovy_extracts_artifact_id_from_maven_coordinates() {
        let content = r#"plugins { id 'java' }

dependencies {
  implementation 'org.apache.commons:commons-lang3:3.14.0'
  api "com.google.guava:guava:32.1.3-jre"
  testImplementation 'org.junit.jupiter:junit-jupiter-api:5.10.0'
  runtimeOnly 'mysql:mysql-connector-java:8.0.33'
}
"#;
        assert_eq!(
            gradle_dependencies(content),
            set(&[
                "commons-lang3",
                "guava",
                "junit-jupiter-api",
                "mysql-connector-java"
            ])
        );
    }

    #[test]
    fn gradle_kotlin_uses_parenthesized_syntax() {
        let content = r#"plugins { kotlin("jvm") version "1.9.22" }

dependencies {
  implementation("io.ktor:ktor-server-core:2.3.7")
  testImplementation("io.kotest:kotest-runner-junit5:5.8.0")
}
"#;
        assert_eq!(
            gradle_dependencies(content),
            set(&["ktor-server-core", "kotest-runner-junit5"])
        );
    }

    #[test]
    fn maven_pom_collects_artifact_ids() {
        let content = r"<project>
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.example</groupId>
  <artifactId>demo</artifactId>
  <dependencies>
    <dependency>
      <groupId>org.springframework</groupId>
      <artifactId>spring-core</artifactId>
      <version>6.1.0</version>
    </dependency>
    <dependency>
      <groupId>junit</groupId>
      <artifactId>junit</artifactId>
      <version>4.13.2</version>
      <scope>test</scope>
    </dependency>
  </dependencies>
</project>
";
        let deps = maven_pom(content);
        assert!(deps.contains("spring-core"));
        assert!(deps.contains("junit"));
        assert!(deps.contains("demo"));
    }

    #[test]
    fn swift_package_handles_name_and_url_forms() {
        let content = r#"// swift-tools-version:5.9
import PackageDescription

let package = Package(
  name: "demo",
  dependencies: [
    .package(name: "ArgumentParser", url: "https://github.com/apple/swift-argument-parser.git", from: "1.3.0"),
    .package(url: "https://github.com/apple/swift-log.git", from: "1.5.0"),
  ],
  targets: []
)
"#;
        let deps = swift_package(content);
        assert!(deps.contains("ArgumentParser"));
        assert!(deps.contains("swift-log"));
    }

    #[test]
    fn mix_exs_collects_atom_dep_names() {
        let content = r#"defmodule Demo.MixProject do
  use Mix.Project

  def project, do: [app: :demo, deps: deps()]

  defp deps do
    [
      {:phoenix, "~> 1.7.10"},
      {:phoenix_html, "~> 4.0"},
      {:floki, ">= 0.30.0", only: :test}
    ]
  end
end
"#;
        assert_eq!(mix_exs(content), set(&["phoenix", "phoenix_html", "floki"]));
    }

    #[test]
    fn msbuild_package_reference_extraction() {
        let content = r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.3" />
    <PackageReference Include="Serilog"
                      Version="3.1.1"
                      PrivateAssets="all" />
  </ItemGroup>
</Project>
"#;
        assert_eq!(msbuild_xml(content), set(&["Newtonsoft.Json", "Serilog"]));
    }

    #[test]
    fn directory_dependencies_unions_every_manifest() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path();
        fs::write(
            dir.join("package.json"),
            r#"{"dependencies": {"express": "^4"}}"#,
        )
        .unwrap();
        fs::write(
            dir.join("Gemfile"),
            "source \"https://rubygems.org\"\ngem \"rails\"\n",
        )
        .unwrap();
        fs::write(
            dir.join("App.csproj"),
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <PackageReference Include="Demo" Version="1" />
  </ItemGroup>
</Project>
"#,
        )
        .unwrap();

        let deps = directory_dependencies(dir);
        assert!(deps.contains(&"express".to_string()));
        assert!(deps.contains(&"rails".to_string()));
        assert!(
            deps.contains(&"Demo".to_string()),
            "expected .csproj PackageReference, got {deps:?}"
        );
        // Output must be sorted + de-duplicated.
        let mut sorted = deps.clone();
        sorted.sort();
        assert_eq!(deps, sorted);
    }

    #[test]
    fn parse_manifest_dispatches_by_file_name() {
        assert_eq!(
            parse_manifest("Cargo.toml", "[dependencies]\nserde = \"1\"\n"),
            set(&["serde"])
        );
        assert!(parse_manifest("unknown.file", "anything").is_empty());
    }

    #[test]
    fn directory_dependencies_empty_for_bare_directory() {
        let tmp = tempdir().unwrap();
        assert!(directory_dependencies(tmp.path()).is_empty());
    }
}
