use super::*;

#[test]
fn test_parses_crystallize_args() {
    let cli = Cli::parse_from([
        "harn",
        "crystallize",
        "--from",
        "fixtures/crystallize",
        "--out",
        "workflows/version_bump.harn",
        "--report",
        "reports/version_bump.json",
        "--eval-pack",
        "harn.eval.toml",
        "--min-examples",
        "5",
        "--shadow-from",
        "fixtures/crystallize/holdout",
        "--promotion-min-confidence",
        "0.9",
        "--workflow-name",
        "version_bump",
    ]);

    let Command::Crystallize(args) = cli.command.unwrap() else {
        panic!("expected crystallize command");
    };
    assert!(args.command.is_none());
    assert_eq!(args.from.as_deref(), Some("fixtures/crystallize"));
    assert_eq!(args.out.as_deref(), Some("workflows/version_bump.harn"));
    assert_eq!(args.report.as_deref(), Some("reports/version_bump.json"));
    assert_eq!(args.eval_pack.as_deref(), Some("harn.eval.toml"));
    assert_eq!(args.min_examples, 5);
    assert_eq!(
        args.shadow_from,
        vec!["fixtures/crystallize/holdout".to_string()]
    );
    assert_eq!(args.promotion_min_confidence, 0.9);
    assert_eq!(args.workflow_name.as_deref(), Some("version_bump"));
}

#[test]
fn test_parses_crystallize_validate_subcommand() {
    let cli = Cli::parse_from(["harn", "crystallize", "validate", "bundles/version-bump"]);

    let Command::Crystallize(args) = cli.command.unwrap() else {
        panic!("expected crystallize command");
    };
    let Some(CrystallizeCommand::Validate(validate)) = args.command else {
        panic!("expected validate subcommand");
    };
    assert_eq!(validate.bundle_dir, "bundles/version-bump");
}

#[test]
fn test_parses_crystallize_bundle_flag() {
    let cli = Cli::parse_from([
        "harn",
        "crystallize",
        "--from",
        "fixtures/crystallize",
        "--out",
        "workflows/version_bump.harn",
        "--report",
        "reports/version_bump.json",
        "--bundle",
        "bundles/version-bump",
        "--bundle-team",
        "platform",
        "--bundle-risk-level",
        "medium",
    ]);

    let Command::Crystallize(args) = cli.command.unwrap() else {
        panic!("expected crystallize command");
    };
    assert!(args.command.is_none());
    assert_eq!(args.bundle.as_deref(), Some("bundles/version-bump"));
    assert_eq!(args.bundle_team.as_deref(), Some("platform"));
    assert_eq!(args.bundle_risk_level.as_deref(), Some("medium"));
}

#[test]
fn test_parses_package_evals_flag() {
    let cli = Cli::parse_from(["harn", "test", "package", "--evals"]);

    let Command::Test(args) = cli.command.unwrap() else {
        panic!("expected test command");
    };
    assert_eq!(args.target.as_deref(), Some("package"));
    assert!(args.evals);
}

#[test]
fn test_parses_trust_query_flags() {
    let cli = Cli::parse_from([
        "harn",
        "trust",
        "query",
        "--agent",
        "github-triage-bot",
        "--action",
        "github.issue.opened",
        "--since",
        "2026-04-19T18:00:00Z",
        "--until",
        "2026-04-19T19:00:00Z",
        "--tier",
        "act-auto",
        "--outcome",
        "success",
        "--limit",
        "500",
        "--grouped-by-trace",
        "--json",
        "--summary",
    ]);

    let Command::Trust(args) = cli.command.unwrap() else {
        panic!("expected trust command");
    };
    let TrustCommand::Query(query) = args.command else {
        panic!("expected trust query");
    };
    assert_eq!(query.agent.as_deref(), Some("github-triage-bot"));
    assert_eq!(query.action.as_deref(), Some("github.issue.opened"));
    assert_eq!(query.since.as_deref(), Some("2026-04-19T18:00:00Z"));
    assert_eq!(query.until.as_deref(), Some("2026-04-19T19:00:00Z"));
    assert!(matches!(query.tier, Some(TrustTierArg::ActAuto)));
    assert!(matches!(query.outcome, Some(TrustOutcomeArg::Success)));
    assert_eq!(query.limit, Some(500));
    assert!(query.grouped_by_trace);
    assert!(query.json);
    assert!(query.summary);
}

#[test]
fn test_parses_trust_demote_flags() {
    let cli = Cli::parse_from([
        "harn",
        "trust",
        "demote",
        "github-triage-bot",
        "--to",
        "shadow",
        "--reason",
        "unexpected mutation",
    ]);

    let Command::Trust(args) = cli.command.unwrap() else {
        panic!("expected trust command");
    };
    let TrustCommand::Demote(demote) = args.command else {
        panic!("expected trust demote");
    };
    assert_eq!(demote.agent, "github-triage-bot");
    assert!(matches!(demote.to, TrustTierArg::Shadow));
    assert_eq!(demote.reason, "unexpected mutation");
}

#[test]
fn test_parses_trust_graph_verify_chain() {
    let cli = Cli::parse_from(["harn", "trust-graph", "verify-chain", "--json"]);

    let Command::TrustGraph(args) = cli.command.unwrap() else {
        panic!("expected trust-graph command");
    };
    let TrustCommand::VerifyChain(verify) = args.command else {
        panic!("expected trust-graph verify-chain");
    };
    assert!(verify.json);
}

#[test]
fn test_parses_pack_signing_flags() {
    let cli = Cli::parse_from([
        "harn",
        "pack",
        "examples/hello.harn",
        "--sign",
        "--key",
        "release.pem",
        "--out",
        "hello.harnpack",
    ]);

    let Command::Pack(args) = cli.command.unwrap() else {
        panic!("expected pack command");
    };
    assert!(args.command.is_none(), "build path takes no subcommand");
    assert_eq!(args.entrypoint, Some(PathBuf::from("examples/hello.harn")));
    assert_eq!(args.key, Some(PathBuf::from("release.pem")));
    assert_eq!(args.out, Some(PathBuf::from("hello.harnpack")));
    assert!(args.sign);
    assert!(!args.unsigned);
}

#[test]
fn test_parses_pack_exclude_secrets_flag() {
    let cli = Cli::parse_from([
        "harn",
        "pack",
        "examples/hello.harn",
        "--unsigned",
        "--exclude-secrets",
    ]);
    let Command::Pack(args) = cli.command.unwrap() else {
        panic!("expected pack command");
    };
    assert!(args.exclude_secrets);
    assert!(!args.include_secrets);
}

#[test]
fn test_parses_pack_verify_subcommand() {
    use crate::cli::PackCommand;
    let cli = Cli::parse_from([
        "harn",
        "pack",
        "verify",
        "bundle.harnpack",
        "--allow-unsigned",
        "--trust-policy",
        "policy.json",
        "--require-trusted-signer",
        "--strict",
        "--json",
    ]);
    let Command::Pack(args) = cli.command.unwrap() else {
        panic!("expected pack command");
    };
    let Some(PackCommand::Verify(verify)) = args.command else {
        panic!("expected pack verify subcommand");
    };
    assert_eq!(verify.bundle, PathBuf::from("bundle.harnpack"));
    assert!(verify.allow_unsigned);
    assert_eq!(verify.trust_policy, Some(PathBuf::from("policy.json")));
    assert!(verify.require_trusted_signer);
    assert!(verify.strict);
    assert!(verify.json);
}

#[test]
fn test_parses_install_integrity_flags() {
    let cli = Cli::parse_from(["harn", "install", "--locked", "--offline"]);

    let Command::Install(args) = cli.command.unwrap() else {
        panic!("expected install command");
    };
    assert!(!args.frozen);
    assert!(args.locked);
    assert!(args.offline);
}

#[test]
fn test_parses_package_cache_subcommands() {
    let cli = Cli::parse_from(["harn", "package", "list", "--json"]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::List(list) = args.command else {
        panic!("expected package list");
    };
    assert!(list.json);

    let cli = Cli::parse_from(["harn", "package", "doctor", "--json"]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Doctor(doctor) = args.command else {
        panic!("expected package doctor");
    };
    assert!(doctor.json);

    let cli = Cli::parse_from([
        "harn",
        "package",
        "search",
        "notion",
        "--registry",
        "index.toml",
        "--json",
    ]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Search(search) = args.command else {
        panic!("expected package search");
    };
    assert_eq!(search.query.as_deref(), Some("notion"));
    assert_eq!(search.registry.as_deref(), Some("index.toml"));
    assert!(search.json);

    let cli = Cli::parse_from([
        "harn",
        "package",
        "info",
        "@burin/notion-sdk@1.2.3",
        "--registry",
        "index.toml",
    ]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Info(info) = args.command else {
        panic!("expected package info");
    };
    assert_eq!(info.name, "@burin/notion-sdk@1.2.3");
    assert_eq!(info.registry.as_deref(), Some("index.toml"));

    let cli = Cli::parse_from(["harn", "package", "check", "pkg", "--json"]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Check(check) = args.command else {
        panic!("expected package check");
    };
    assert_eq!(check.package, Some(PathBuf::from("pkg")));
    assert!(check.json);

    let cli = Cli::parse_from(["harn", "package", "pack", "pkg", "--dry-run"]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Pack(pack) = args.command else {
        panic!("expected package pack");
    };
    assert_eq!(pack.package, Some(PathBuf::from("pkg")));
    assert!(pack.dry_run);

    let cli = Cli::parse_from(["harn", "package", "docs", "pkg", "--check"]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Docs(docs) = args.command else {
        panic!("expected package docs");
    };
    assert_eq!(docs.package, Some(PathBuf::from("pkg")));
    assert!(docs.check);

    let cli = Cli::parse_from(["harn", "package", "cache", "list"]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Cache(cache) = args.command else {
        panic!("expected package cache");
    };
    assert!(matches!(cache.command, PackageCacheCommand::List));

    let cli = Cli::parse_from(["harn", "package", "cache", "clean", "--all"]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Cache(cache) = args.command else {
        panic!("expected package cache");
    };
    let PackageCacheCommand::Clean(clean) = cache.command else {
        panic!("expected package cache clean");
    };
    assert!(clean.all);

    let cli = Cli::parse_from(["harn", "package", "cache", "verify", "--materialized"]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Cache(cache) = args.command else {
        panic!("expected package cache");
    };
    let PackageCacheCommand::Verify(verify) = cache.command else {
        panic!("expected package cache verify");
    };
    assert!(verify.materialized);
}

#[test]
fn test_parses_publish_git_tag_flow_options() {
    let cli = Cli::parse_from([
        "harn",
        "publish",
        "pkg",
        "--dry-run",
        "--remote",
        "upstream",
        "--index-repo",
        "burin-labs/harn-packages",
        "--index-path",
        "harn-package-index.toml",
        "--registry-name",
        "@burin/acme-lib",
        "--skip-index-pr",
        "--json",
    ]);
    let Command::Publish(PublishArgs {
        package,
        dry_run,
        remote,
        index_repo,
        index_path,
        registry_name,
        skip_index_pr,
        json,
        ..
    }) = cli.command.unwrap()
    else {
        panic!("expected publish command");
    };

    assert_eq!(package, Some(PathBuf::from("pkg")));
    assert!(dry_run);
    assert_eq!(remote, "upstream");
    assert_eq!(index_repo, "burin-labs/harn-packages");
    assert_eq!(index_path, PathBuf::from("harn-package-index.toml"));
    assert_eq!(registry_name.as_deref(), Some("@burin/acme-lib"));
    assert!(skip_index_pr);
    assert!(json);
}

#[test]
fn test_parses_rule_publish_and_search() {
    let cli = Cli::parse_from([
        "harn",
        "rule",
        "publish",
        "pkg",
        "--dry-run",
        "--registry-name",
        "@acme/rules",
        "--skip-index-pr",
        "--json",
    ]);
    let Command::Rule(args) = cli.command.unwrap() else {
        panic!("expected rule command");
    };
    let RuleCommand::Publish(publish) = args.command else {
        panic!("expected rule publish");
    };
    assert_eq!(publish.package, Some(PathBuf::from("pkg")));
    assert!(publish.dry_run);
    assert_eq!(publish.registry_name.as_deref(), Some("@acme/rules"));
    assert!(publish.skip_index_pr);
    assert!(publish.json);

    let cli = Cli::parse_from([
        "harn",
        "rule",
        "search",
        "typescript",
        "--registry",
        "index.toml",
        "--json",
    ]);
    let Command::Rule(args) = cli.command.unwrap() else {
        panic!("expected rule command");
    };
    let RuleCommand::Search(search) = args.command else {
        panic!("expected rule search");
    };
    assert_eq!(search.query.as_deref(), Some("typescript"));
    assert_eq!(search.registry.as_deref(), Some("index.toml"));
    assert!(search.json);
}

#[test]
fn test_parses_package_scaffold_openapi() {
    let cli = Cli::parse_from([
        "harn",
        "package",
        "scaffold",
        "openapi",
        "--name",
        "acme-sdk-harn",
        "--module-name",
        "acme_sdk",
        "--client-name",
        "AcmeClient",
        "--spec",
        "./openapi.json",
        "--out",
        "./acme-sdk-harn",
        "--default-base-url",
        "https://api.example.test",
        "--harn-openapi-path",
        "../harn-openapi",
        "--harn-openapi-git",
        "https://github.com/burin-labs/harn-openapi",
        "--harn-openapi-rev",
        "abc123",
        "--force",
    ]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Scaffold(scaffold) = args.command else {
        panic!("expected package scaffold");
    };
    let PackageScaffoldCommand::Openapi(openapi) = scaffold.command;
    assert_eq!(openapi.name, "acme-sdk-harn");
    assert_eq!(openapi.module_name.as_deref(), Some("acme_sdk"));
    assert_eq!(openapi.client_name.as_deref(), Some("AcmeClient"));
    assert_eq!(openapi.spec, "./openapi.json");
    assert_eq!(openapi.out, Some(PathBuf::from("./acme-sdk-harn")));
    assert_eq!(
        openapi.default_base_url.as_deref(),
        Some("https://api.example.test")
    );
    assert_eq!(
        openapi.harn_openapi_path,
        Some(PathBuf::from("../harn-openapi"))
    );
    assert_eq!(
        openapi.harn_openapi_git.as_deref(),
        Some("https://github.com/burin-labs/harn-openapi")
    );
    assert_eq!(openapi.harn_openapi_rev.as_deref(), Some("abc123"));
    assert!(openapi.force);
}

#[test]
fn test_parses_package_outdated_audit_artifacts() {
    let cli = Cli::parse_from([
        "harn",
        "package",
        "outdated",
        "--remote",
        "--refresh",
        "--registry",
        "index.toml",
        "--json",
    ]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Outdated(outdated) = args.command else {
        panic!("expected package outdated");
    };
    assert!(outdated.remote);
    assert!(outdated.refresh);
    assert_eq!(outdated.registry.as_deref(), Some("index.toml"));
    assert!(outdated.json);

    let cli = Cli::parse_from([
        "harn",
        "package",
        "audit",
        "--registry",
        "index.toml",
        "--skip-materialized",
        "--json",
    ]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Audit(audit) = args.command else {
        panic!("expected package audit");
    };
    assert_eq!(audit.registry.as_deref(), Some("index.toml"));
    assert!(audit.skip_materialized);
    assert!(audit.json);

    let cli = Cli::parse_from([
        "harn",
        "package",
        "artifacts",
        "manifest",
        "--output",
        "vendor/manifest.json",
    ]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Artifacts(artifacts) = args.command else {
        panic!("expected package artifacts");
    };
    let PackageArtifactsCommand::Manifest(manifest) = artifacts.command else {
        panic!("expected artifacts manifest");
    };
    assert_eq!(manifest.output, Some(PathBuf::from("vendor/manifest.json")));

    let cli = Cli::parse_from([
        "harn",
        "package",
        "artifacts",
        "check",
        "vendor/manifest.json",
        "--json",
    ]);
    let Command::Package(args) = cli.command.unwrap() else {
        panic!("expected package command");
    };
    let PackageCommand::Artifacts(artifacts) = args.command else {
        panic!("expected package artifacts");
    };
    let PackageArtifactsCommand::Check(check) = artifacts.command else {
        panic!("expected artifacts check");
    };
    assert_eq!(check.manifest, PathBuf::from("vendor/manifest.json"));
    assert!(check.json);
}

#[test]
fn test_install_and_update_accept_json_flag() {
    let cli = Cli::parse_from(["harn", "install", "--frozen", "--json"]);
    let Command::Install(install) = cli.command.unwrap() else {
        panic!("expected install command");
    };
    assert!(install.frozen);
    assert!(install.json);

    let cli = Cli::parse_from(["harn", "update", "--all", "--json"]);
    let Command::Update(update) = cli.command.unwrap() else {
        panic!("expected update command");
    };
    assert!(update.all);
    assert!(update.json);
}

#[test]
fn test_parses_skills_subcommands() {
    let cli = Cli::parse_from(["harn", "skills", "list", "--json"]);
    let Command::Skills(args) = cli.command.unwrap() else {
        panic!("expected skills command");
    };
    let SkillsCommand::List(list) = args.command else {
        panic!("expected skills list");
    };
    assert!(list.json);

    let cli = Cli::parse_from(["harn", "skills", "get", "harn-language", "--full", "--json"]);
    let Command::Skills(args) = cli.command.unwrap() else {
        panic!("expected skills command");
    };
    let SkillsCommand::Get(get) = args.command else {
        panic!("expected skills get");
    };
    assert_eq!(get.name, "harn-language");
    assert!(get.full);
    assert!(get.json);

    let cli = Cli::parse_from([
        "harn",
        "skills",
        "dump",
        "--all",
        "--out",
        "/tmp/skills",
        "--force",
    ]);
    let Command::Skills(args) = cli.command.unwrap() else {
        panic!("expected skills command");
    };
    let SkillsCommand::Dump(dump) = args.command else {
        panic!("expected skills dump");
    };
    assert!(dump.all);
    assert_eq!(dump.out.as_deref(), Some("/tmp/skills"));
    assert!(dump.force);

    let cli = Cli::parse_from(["harn", "skills", "resolved", "--json", "--all"]);
    let Command::Skills(args) = cli.command.unwrap() else {
        panic!("expected skills command");
    };
    let SkillsCommand::Resolved(resolved) = args.command else {
        panic!("expected skills resolved");
    };
    assert!(resolved.json);
    assert!(resolved.all);

    let cli = Cli::parse_from(["harn", "skills", "match", "deploy the app", "--top-n", "3"]);
    let Command::Skills(args) = cli.command.unwrap() else {
        panic!("expected skills command");
    };
    let SkillsCommand::Match(matcher) = args.command else {
        panic!("expected skills match");
    };
    assert_eq!(matcher.query, "deploy the app");
    assert_eq!(matcher.top_n, 3);

    let cli = Cli::parse_from([
        "harn",
        "skills",
        "install",
        "https://example.com/acme/harn-skills.git",
        "--tag",
        "v1.0",
        "--namespace",
        "acme",
    ]);
    let Command::Skills(args) = cli.command.unwrap() else {
        panic!("expected skills command");
    };
    let SkillsCommand::Install(install) = args.command else {
        panic!("expected skills install");
    };
    assert_eq!(install.tag.as_deref(), Some("v1.0"));
    assert_eq!(install.namespace.as_deref(), Some("acme"));

    let cli = Cli::parse_from([
        "harn",
        "skills",
        "new",
        "deploy",
        "--description",
        "Ship things",
    ]);
    let Command::Skills(args) = cli.command.unwrap() else {
        panic!("expected skills command");
    };
    let SkillsCommand::New(new_args) = args.command else {
        panic!("expected skills new");
    };
    assert_eq!(new_args.name, "deploy");
    assert_eq!(new_args.description.as_deref(), Some("Ship things"));
}

#[test]
fn test_parses_skill_provenance_subcommands() {
    let cli = Cli::parse_from(["harn", "skill", "key", "generate", "--out", "signer.pem"]);
    let Command::Skill(args) = cli.command.unwrap() else {
        panic!("expected skill command");
    };
    let SkillCommand::Key(key_args) = args.command else {
        panic!("expected skill key");
    };
    let SkillKeyCommand::Generate(generate) = key_args.command;
    assert_eq!(generate.out, "signer.pem");

    let cli = Cli::parse_from(["harn", "skill", "sign", "SKILL.md", "--key", "signer.pem"]);
    let Command::Skill(args) = cli.command.unwrap() else {
        panic!("expected skill command");
    };
    let SkillCommand::Sign(sign) = args.command else {
        panic!("expected skill sign");
    };
    assert_eq!(sign.skill, "SKILL.md");
    assert_eq!(sign.key, "signer.pem");

    let cli = Cli::parse_from([
        "harn",
        "skill",
        "endorse",
        "SKILL.md",
        "--key",
        "auditor.pem",
    ]);
    let Command::Skill(args) = cli.command.unwrap() else {
        panic!("expected skill command");
    };
    let SkillCommand::Endorse(endorse) = args.command else {
        panic!("expected skill endorse");
    };
    assert_eq!(endorse.skill, "SKILL.md");
    assert_eq!(endorse.key, "auditor.pem");

    let cli = Cli::parse_from(["harn", "skill", "verify", "SKILL.md", "--json"]);
    let Command::Skill(args) = cli.command.unwrap() else {
        panic!("expected skill command");
    };
    let SkillCommand::Verify(verify) = args.command else {
        panic!("expected skill verify");
    };
    assert_eq!(verify.skill, "SKILL.md");
    assert!(verify.json);

    let cli = Cli::parse_from(["harn", "skill", "who-signed", "SKILL.md", "--json"]);
    let Command::Skill(args) = cli.command.unwrap() else {
        panic!("expected skill command");
    };
    let SkillCommand::WhoSigned(who_signed) = args.command else {
        panic!("expected skill who-signed");
    };
    assert_eq!(who_signed.skill, "SKILL.md");
    assert!(who_signed.json);

    let cli = Cli::parse_from([
        "harn",
        "skill",
        "trust",
        "add",
        "--from",
        "https://example.com/signer.pub",
    ]);
    let Command::Skill(args) = cli.command.unwrap() else {
        panic!("expected skill command");
    };
    let SkillCommand::Trust(trust) = args.command else {
        panic!("expected skill trust");
    };
    let SkillTrustCommand::Add(add) = trust.command else {
        panic!("expected skill trust add");
    };
    assert_eq!(add.from, "https://example.com/signer.pub");

    let cli = Cli::parse_from(["harn", "skill", "trust", "list"]);
    let Command::Skill(args) = cli.command.unwrap() else {
        panic!("expected skill command");
    };
    let SkillCommand::Trust(trust) = args.command else {
        panic!("expected skill trust");
    };
    assert!(matches!(trust.command, SkillTrustCommand::List(_)));
}
