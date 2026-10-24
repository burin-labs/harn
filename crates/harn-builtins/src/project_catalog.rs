#[derive(Debug, Clone, Copy)]
pub(crate) struct ProjectCatalogEntry {
    pub(crate) id: &'static str,
    pub(crate) languages: &'static [&'static str],
    pub(crate) frameworks: &'static [&'static str],
    pub(crate) build_systems: &'static [&'static str],
    pub(crate) anchors: &'static [&'static str],
    pub(crate) lockfiles: &'static [&'static str],
    pub(crate) source_globs: &'static [&'static str],
    pub(crate) commands: ProjectCommands,
    pub(crate) anchor_score: f64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ProjectCommands {
    Shell {
        build: Option<&'static str>,
        test: Option<&'static str>,
    },
    PackageScripts,
}

impl ProjectCatalogEntry {
    pub(crate) fn default_build_cmd(&self) -> Option<String> {
        match self.commands {
            ProjectCommands::Shell { build, .. } => build.map(str::to_string),
            ProjectCommands::PackageScripts => None,
        }
    }

    pub(crate) fn default_test_cmd(&self) -> Option<String> {
        match self.commands {
            ProjectCommands::Shell { test, .. } => test.map(str::to_string),
            ProjectCommands::PackageScripts => None,
        }
    }
}

pub(crate) const PROJECT_CATALOG: &[ProjectCatalogEntry] = &[
    ProjectCatalogEntry {
        id: "rust",
        languages: &["rust"],
        frameworks: &[],
        build_systems: &["cargo"],
        anchors: &["Cargo.toml"],
        lockfiles: &["Cargo.lock"],
        source_globs: &["*.rs", "src/**/*.rs", "crates/**/*.rs", "tests/**/*.rs"],
        commands: ProjectCommands::Shell {
            build: Some("cargo build"),
            test: Some("cargo test"),
        },
        anchor_score: 0.5,
    },
    ProjectCatalogEntry {
        id: "go",
        languages: &["go"],
        frameworks: &[],
        build_systems: &["go"],
        anchors: &["go.mod"],
        lockfiles: &["go.sum"],
        source_globs: &["*.go", "cmd/**/*.go", "internal/**/*.go", "pkg/**/*.go"],
        commands: ProjectCommands::Shell {
            build: Some("go build ./..."),
            test: Some("go test ./..."),
        },
        anchor_score: 0.5,
    },
    ProjectCatalogEntry {
        id: "python",
        languages: &["python"],
        frameworks: &[],
        build_systems: &["python"],
        anchors: &["pyproject.toml", "setup.py", "requirements.txt"],
        lockfiles: &[
            "uv.lock",
            "poetry.lock",
            "Pipfile.lock",
            "requirements.lock",
        ],
        source_globs: &["*.py", "src/**/*.py", "tests/**/*.py"],
        commands: ProjectCommands::Shell {
            build: None,
            test: Some("pytest"),
        },
        anchor_score: 0.5,
    },
    ProjectCatalogEntry {
        id: "php",
        languages: &["php"],
        frameworks: &[],
        build_systems: &["composer"],
        anchors: &["composer.json"],
        lockfiles: &["composer.lock"],
        source_globs: &["*.php", "app/**/*.php", "src/**/*.php", "tests/**/*.php"],
        commands: ProjectCommands::Shell {
            build: None,
            test: Some("composer test"),
        },
        anchor_score: 0.5,
    },
    ProjectCatalogEntry {
        id: "typescript",
        languages: &["typescript"],
        frameworks: &[],
        build_systems: &["npm"],
        anchors: &["tsconfig.json", "package.json"],
        lockfiles: &harn_vm::package_scripts::LOCKFILE_NAMES,
        source_globs: &[
            "*.ts",
            "*.tsx",
            "src/**/*.ts",
            "src/**/*.tsx",
            "tests/**/*.ts",
        ],
        commands: ProjectCommands::PackageScripts,
        anchor_score: 0.2,
    },
    ProjectCatalogEntry {
        id: "javascript",
        languages: &["javascript"],
        frameworks: &[],
        build_systems: &["npm"],
        anchors: &["package.json"],
        lockfiles: &harn_vm::package_scripts::LOCKFILE_NAMES,
        source_globs: &[
            "*.js",
            "*.jsx",
            "*.mjs",
            "*.cjs",
            "src/**/*.js",
            "src/**/*.jsx",
        ],
        commands: ProjectCommands::PackageScripts,
        anchor_score: 0.1,
    },
    ProjectCatalogEntry {
        id: "nextjs",
        languages: &["javascript", "typescript"],
        frameworks: &["nextjs"],
        build_systems: &["npm"],
        anchors: &["next.config.js", "next.config.mjs", "next.config.ts"],
        lockfiles: &[],
        source_globs: &[
            "pages/**/*.tsx",
            "app/**/*.tsx",
            "pages/**/*.js",
            "app/**/*.js",
        ],
        commands: ProjectCommands::PackageScripts,
        anchor_score: 0.7,
    },
    ProjectCatalogEntry {
        id: "ruby",
        languages: &["ruby"],
        frameworks: &[],
        build_systems: &["bundler"],
        anchors: &["Gemfile"],
        lockfiles: &["Gemfile.lock"],
        source_globs: &["*.rb", "app/**/*.rb", "lib/**/*.rb", "spec/**/*.rb"],
        commands: ProjectCommands::Shell {
            build: None,
            test: Some("bundle exec rspec"),
        },
        anchor_score: 0.5,
    },
];

pub(crate) fn project_catalog() -> &'static [ProjectCatalogEntry] {
    PROJECT_CATALOG
}
