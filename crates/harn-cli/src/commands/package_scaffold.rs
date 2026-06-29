use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;
use url::Url;

use crate::cli::PackageScaffoldOpenapiArgs;
use crate::commands::run::{
    execute_run_with_sandbox_options, CliLlmMockMode, RunProfileOptions, RunSandboxOptions,
};
use crate::commands::scaffold_common::{
    harn_identifier, harn_string_literal, pascal_identifier_from_snake, validate_harn_identifier,
    write_bytes, write_file,
};
use crate::package::{
    current_harn_range_example, generate_package_docs_impl, toml_string_literal,
    validate_package_alias, PackageError,
};

const DEFAULT_HARN_OPENAPI_GIT: &str = "https://github.com/burin-labs/harn-openapi";
const DEFAULT_HARN_OPENAPI_REV: &str = "v0.1.1-rc.1";
const DEFAULT_SMOKE_BASE_URL: &str = "https://api.example.test";
const MAX_SPEC_BYTES: u64 = 16 * 1024 * 1024;

pub(crate) async fn run_openapi(args: &PackageScaffoldOpenapiArgs) -> Result<(), PackageError> {
    scaffold_openapi_package(args).await
}

async fn scaffold_openapi_package(args: &PackageScaffoldOpenapiArgs) -> Result<(), PackageError> {
    validate_package_alias(&args.name)?;
    let module_name = args
        .module_name
        .clone()
        .unwrap_or(harn_identifier(&args.name)?);
    validate_harn_identifier(&module_name, "--module-name")?;
    let client_name = args.client_name.clone().unwrap_or_else(|| {
        let base = pascal_identifier_from_snake(&module_name);
        if base.ends_with("Client") {
            base
        } else {
            format!("{base}Client")
        }
    });
    validate_harn_identifier(&client_name, "--client-name")?;

    let dest = args
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from(&args.name));
    prepare_destination(&dest, args.force)?;

    let spec = load_spec(&args.spec).await?;
    let default_base_url = args
        .default_base_url
        .clone()
        .or_else(|| first_server_url(&spec.value))
        .unwrap_or_else(|| DEFAULT_SMOKE_BASE_URL.to_string());
    let smoke = find_smoke_operation(&spec.value);
    let generator = resolve_harn_openapi(args)?;

    for (relative_path, content) in openapi_template_files(
        args,
        &module_name,
        &client_name,
        &spec,
        &default_base_url,
        smoke.as_ref(),
    )? {
        write_file(&dest, relative_path, &content)?;
    }
    write_bytes(&dest, &spec.relative_path, spec.text.as_bytes())?;

    let generated_path = dest.join("src/lib.harn");
    generate_sdk_source(
        &generator.lib_import_path,
        &dest.join(&spec.relative_path),
        &generated_path,
        &module_name,
        &client_name,
        &default_base_url,
    )
    .await?;
    generate_package_docs_impl(Some(&dest), None, false)?;

    println!(
        "Scaffolded OpenAPI SDK package '{}' at {}",
        args.name,
        dest.display()
    );
    println!("  cd {}", dest.display());
    println!("  harn install");
    println!("  harn check src/lib.harn");
    println!("  harn test tests/");
    println!("  harn package check");
    println!("  harn package docs --check");
    println!("  harn package pack --dry-run");
    Ok(())
}

fn prepare_destination(dest: &Path, force: bool) -> Result<(), PackageError> {
    if dest.exists() {
        if !force {
            return Err(format!(
                "{} already exists. Pass --force to overwrite generated files.",
                dest.display()
            )
            .into());
        }
        if !dest.is_dir() {
            return Err(format!("{} exists and is not a directory.", dest.display()).into());
        }
        return Ok(());
    }
    fs::create_dir_all(dest)
        .map_err(|error| format!("failed to create {}: {error}", dest.display()).into())
}

struct LoadedSpec {
    relative_path: String,
    source_label: String,
    text: String,
    value: Value,
}

async fn load_spec(spec: &str) -> Result<LoadedSpec, PackageError> {
    if crate::format::looks_like_windows_drive_path(spec) {
        return loaded_spec_from_bytes(read_spec_path(&expand_tilde(spec))?);
    }
    let (source_label, bytes) = match Url::parse(spec) {
        Ok(url) => match url.scheme() {
            "http" | "https" => fetch_spec_url(&url).await?,
            "file" => {
                let path = url.to_file_path().map_err(|_| {
                    format!("OpenAPI spec file URL {url} does not resolve to a local path")
                })?;
                read_spec_path(&path)?
            }
            scheme => {
                return Err(format!(
                    "unsupported OpenAPI spec URL scheme {scheme:?}; use a file path or http(s) URL"
                )
                .into());
            }
        },
        Err(_) => read_spec_path(&expand_tilde(spec))?,
    };

    loaded_spec_from_bytes((source_label, bytes))
}

fn loaded_spec_from_bytes(
    (source_label, bytes): (String, Vec<u8>),
) -> Result<LoadedSpec, PackageError> {
    if bytes.len() as u64 > MAX_SPEC_BYTES {
        return Err(format!(
            "OpenAPI spec {} is {} bytes; maximum supported size is {} bytes",
            source_label,
            bytes.len(),
            MAX_SPEC_BYTES
        )
        .into());
    }
    let text = String::from_utf8(bytes)
        .map_err(|error| format!("OpenAPI spec {source_label} is not UTF-8: {error}"))?;
    let (value, is_json) = parse_spec_value(&text, &source_label)?;
    validate_openapi_version(&value, &source_label)?;
    let extension = if is_json { "json" } else { "yaml" };
    Ok(LoadedSpec {
        relative_path: format!("openapi/openapi.{extension}"),
        source_label,
        text: ensure_trailing_newline(text),
        value,
    })
}

async fn fetch_spec_url(url: &Url) -> Result<(String, Vec<u8>), PackageError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| format!("failed to initialize HTTP client: {error}"))?;
    let mut response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|error| format!("failed to fetch OpenAPI spec {url}: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("failed to fetch OpenAPI spec {url}: HTTP {status}").into());
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_SPEC_BYTES)
    {
        return Err(
            format!("OpenAPI spec {url} is larger than the {MAX_SPEC_BYTES} byte limit").into(),
        );
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("failed to read OpenAPI spec {url}: {error}"))?
    {
        let next_len = bytes.len().saturating_add(chunk.len());
        if next_len > MAX_SPEC_BYTES as usize {
            return Err(format!(
                "OpenAPI spec {url} is larger than the {MAX_SPEC_BYTES} byte limit"
            )
            .into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((url.to_string(), bytes))
}

fn read_spec_path(path: &Path) -> Result<(String, Vec<u8>), PackageError> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("failed to stat OpenAPI spec {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("OpenAPI spec {} is not a file", path.display()).into());
    }
    if metadata.len() > MAX_SPEC_BYTES {
        return Err(format!(
            "OpenAPI spec {} is {} bytes; maximum supported size is {} bytes",
            path.display(),
            metadata.len(),
            MAX_SPEC_BYTES
        )
        .into());
    }
    let bytes = fs::read(path)
        .map_err(|error| format!("failed to read OpenAPI spec {}: {error}", path.display()))?;
    Ok((path.display().to_string(), bytes))
}

fn parse_spec_value(text: &str, source_label: &str) -> Result<(Value, bool), PackageError> {
    match serde_json::from_str::<Value>(text) {
        Ok(value) => Ok((value, true)),
        Err(json_error) => match serde_yml::from_str::<Value>(text) {
            Ok(value) => Ok((value, false)),
            Err(yaml_error) => Err(format!(
                "failed to parse OpenAPI spec {source_label} as JSON ({json_error}) or YAML ({yaml_error})"
            )
            .into()),
        },
    }
}

fn validate_openapi_version(value: &Value, source_label: &str) -> Result<(), PackageError> {
    let version = value
        .get("openapi")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("OpenAPI spec {source_label} is missing top-level `openapi`"))?;
    if !version.starts_with("3.1.") {
        return Err(
            format!("OpenAPI spec {source_label} must be OpenAPI 3.1.x; got {version}").into(),
        );
    }
    Ok(())
}

fn ensure_trailing_newline(mut text: String) -> String {
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

struct ResolvedHarnOpenapi {
    lib_import_path: String,
    _checkout: Option<TempDir>,
}

fn resolve_harn_openapi(
    args: &PackageScaffoldOpenapiArgs,
) -> Result<ResolvedHarnOpenapi, PackageError> {
    if let Some(path) = args.harn_openapi_path.as_deref() {
        return resolve_local_harn_openapi(path).map(|lib_import_path| ResolvedHarnOpenapi {
            lib_import_path,
            _checkout: None,
        });
    }

    for candidate in harn_openapi_candidates(args) {
        if let Ok(lib_import_path) = resolve_local_harn_openapi(&candidate) {
            return Ok(ResolvedHarnOpenapi {
                lib_import_path,
                _checkout: None,
            });
        }
    }

    clone_harn_openapi(args)
}

fn harn_openapi_candidates(args: &PackageScaffoldOpenapiArgs) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(git) = args.harn_openapi_git.as_deref() {
        let path = expand_tilde(git);
        if path.exists() {
            candidates.push(path);
        }
    }
    if let Ok(raw) = std::env::var("HARN_OPENAPI_PATH") {
        if !raw.trim().is_empty() {
            candidates.push(expand_tilde(raw.trim()));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("../harn-openapi"));
        candidates.push(cwd.join("harn-openapi"));
    }
    if let Some(home) = harn_vm::user_dirs::home_dir() {
        candidates.push(home.join("projects/harn-openapi"));
    }
    candidates
}

fn resolve_local_harn_openapi(path: &Path) -> Result<String, PackageError> {
    let root = expand_tilde_path(path);
    let lib = root.join("src/lib.harn");
    if !lib.is_file() {
        return Err(format!(
            "{} is not a harn-openapi checkout with src/lib.harn",
            root.display()
        )
        .into());
    }
    let import_path = lib.with_extension("");
    Ok(import_path.display().to_string())
}

fn clone_harn_openapi(
    args: &PackageScaffoldOpenapiArgs,
) -> Result<ResolvedHarnOpenapi, PackageError> {
    let git = args
        .harn_openapi_git
        .as_deref()
        .unwrap_or(DEFAULT_HARN_OPENAPI_GIT);
    let tmp = tempfile::tempdir()
        .map_err(|error| format!("failed to create harn-openapi temp checkout: {error}"))?;
    let checkout = tmp.path().join("harn-openapi");
    let checkout_rev = args.harn_openapi_rev.as_deref().or_else(|| {
        args.harn_openapi_branch
            .is_none()
            .then_some(DEFAULT_HARN_OPENAPI_REV)
    });
    let mut command = process::Command::new("git");
    command.arg("clone");
    if let Some(branch) = args.harn_openapi_branch.as_deref() {
        command.arg("--depth").arg("1").arg("--branch").arg(branch);
    }
    command
        .arg(git)
        .arg(&checkout)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    let output = command.output().map_err(|error| {
        format!("failed to invoke git while cloning harn-openapi from {git}: {error}")
    })?;
    if !output.status.success() {
        return Err(format!(
            "failed to clone harn-openapi from {git}: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    if let Some(rev) = checkout_rev {
        let output = process::Command::new("git")
            .arg("checkout")
            .arg(rev)
            .current_dir(&checkout)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .map_err(|error| format!("failed to invoke git checkout {rev}: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "failed to checkout harn-openapi rev {rev}: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
    }
    let lib_import_path = resolve_local_harn_openapi(&checkout)?;
    Ok(ResolvedHarnOpenapi {
        lib_import_path,
        _checkout: Some(tmp),
    })
}

async fn generate_sdk_source(
    harn_openapi_import: &str,
    spec_path: &Path,
    out_path: &Path,
    module_name: &str,
    client_name: &str,
    default_base_url: &str,
) -> Result<(), PackageError> {
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    let package_root = out_path
        .parent()
        .and_then(Path::parent)
        .or_else(|| out_path.parent())
        .unwrap_or_else(|| Path::new("."));
    let tmp = tempfile::tempdir()
        .map_err(|error| format!("failed to create OpenAPI generation temp dir: {error}"))?;
    let script_path = tmp.path().join("generate_openapi_sdk.harn");
    let script = format!(
        r#"import {{ codegen_module, parse }} from {import_path}

fn _load_doc(path: string) {{
  let raw = read_file(path)
  let decoded = try {{
    json_parse(raw)
  }} catch (e) {{
    yaml_parse(raw)
  }}
  return parse(json_stringify(decoded))
}}

pipeline default() {{
  let doc = _load_doc(argv[0])
  var options = {{
    module_name: argv[2],
    client_name: argv[3],
  }}
  if argv[4] != "" {{
    options = {{...options, default_base_url: argv[4]}}
  }}
  write_file(argv[1], codegen_module(doc, options))
}}
"#,
        import_path = harn_string_literal(harn_openapi_import)
    );
    fs::write(&script_path, script)
        .map_err(|error| format!("failed to write {}: {error}", script_path.display()))?;
    let outcome = execute_run_with_sandbox_options(
        &script_path.display().to_string(),
        false,
        HashSet::new(),
        vec![
            spec_path.display().to_string(),
            out_path.display().to_string(),
            module_name.to_string(),
            client_name.to_string(),
            default_base_url.to_string(),
        ],
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
        RunSandboxOptions::default().with_workspace_root(package_root),
    )
    .await;
    if outcome.exit_code != 0 {
        return Err(format!(
            "harn-openapi generation failed with exit code {}\n{}{}",
            outcome.exit_code, outcome.stderr, outcome.stdout
        )
        .into());
    }
    Ok(())
}

fn openapi_template_files(
    args: &PackageScaffoldOpenapiArgs,
    module_name: &str,
    client_name: &str,
    spec: &LoadedSpec,
    default_base_url: &str,
    smoke: Option<&SmokeOperation>,
) -> Result<Vec<(&'static str, String)>, PackageError> {
    let description = args.description.clone().unwrap_or_else(|| {
        format!(
            "Focused Harn SDK package generated from {}.",
            spec_title(&spec.value).unwrap_or("an OpenAPI spec")
        )
    });
    let package_name = toml_string_literal(&args.name)?;
    let description_toml = toml_string_literal(&description)?;
    let repository = toml_string_literal(&format!("https://github.com/OWNER/{}", args.name))?;
    let provenance = toml_string_literal(&format!(
        "https://github.com/OWNER/{}/releases/tag/v0.1.0",
        args.name
    ))?;
    let harn_range = current_harn_range_example();
    let dependency = harn_openapi_dependency(args)?;
    let smoke_test = smoke_test_source(smoke, default_base_url);
    let regen = regen_script_source(
        module_name,
        client_name,
        &spec.relative_path,
        default_base_url,
    );
    let readme = readme_source(
        &args.name,
        &description,
        &spec.source_label,
        module_name,
        client_name,
    );

    Ok(vec![
        (
            "harn.toml",
            format!(
                r#"[package]
name = {package_name}
version = "0.1.0"
description = {description_toml}
license = "MIT OR Apache-2.0"
repository = {repository}
provenance = {provenance}
harn = "{harn_range}"
docs_url = "docs/api.md"

[exports]
{module_name} = "src/lib.harn"

[dependencies]
harn-openapi = {dependency}
"#
            ),
        ),
        ("scripts/regen.harn", regen),
        ("tests/smoke.harn", smoke_test),
        ("README.md", readme),
        ("LICENSE", "MIT OR Apache-2.0\n".to_string()),
        (
            ".github/workflows/harn-package.yml",
            r"name: Harn OpenAPI SDK package

on:
  pull_request:
  push:
    branches: [main]

jobs:
  package:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: taiki-e/install-action@cargo-binstall
      - run: cargo binstall harn-cli --no-confirm
      - run: harn install --locked --offline || harn install
      - run: harn check src/lib.harn
      - run: harn test tests/
      - run: harn package check
      - run: harn package docs --check
      - run: harn package pack --dry-run
"
            .to_string(),
        ),
    ])
}

fn harn_openapi_dependency(args: &PackageScaffoldOpenapiArgs) -> Result<String, PackageError> {
    let git = toml_string_literal(
        args.harn_openapi_git
            .as_deref()
            .unwrap_or(DEFAULT_HARN_OPENAPI_GIT),
    )?;
    let pin = if let Some(rev) = args.harn_openapi_rev.as_deref().or_else(|| {
        args.harn_openapi_branch
            .is_none()
            .then_some(DEFAULT_HARN_OPENAPI_REV)
    }) {
        format!("rev = {}", toml_string_literal(rev)?)
    } else {
        format!(
            "branch = {}",
            toml_string_literal(args.harn_openapi_branch.as_deref().unwrap())?
        )
    };
    Ok(format!("{{ git = {git}, {pin} }}"))
}

fn regen_script_source(
    module_name: &str,
    client_name: &str,
    spec_relative_path: &str,
    default_base_url: &str,
) -> String {
    format!(
        r#"import {{ codegen_module, parse }} from "harn-openapi/default"

fn _load_doc(path: string) {{
  let raw = read_file(path)
  let decoded = try {{
    json_parse(raw)
  }} catch (e) {{
    yaml_parse(raw)
  }}
  return parse(json_stringify(decoded))
}}

pipeline default() {{
  let root = source_dir() + "/.."
  let spec_path = if len(argv) > 0 {{ argv[0] }} else {{ root + "/" + {spec_path} }}
  let out_path = if len(argv) > 1 {{ argv[1] }} else {{ root + "/src/lib.harn" }}
  let doc = _load_doc(spec_path)
  write_file(
    out_path,
    codegen_module(
      doc,
      {{
        module_name: {module_name},
        client_name: {client_name},
        default_base_url: {default_base_url},
      }},
    ),
  )
  __io_println("wrote " + out_path)
}}
"#,
        spec_path = harn_string_literal(spec_relative_path),
        module_name = harn_string_literal(module_name),
        client_name = harn_string_literal(client_name),
        default_base_url = harn_string_literal(default_base_url),
    )
}

fn smoke_test_source(smoke: Option<&SmokeOperation>, default_base_url: &str) -> String {
    if let Some(smoke) = smoke {
        return format!(
            r#"import {{ new_client, {fn_name} }} from "../src/lib"

pipeline test_generated_sdk_smoke(task) {{
  http_mock_clear()
  http_mock({method}, {url}, {{
    status: 200,
    body: "{{}}",
    headers: {{"content-type": "application/json"}},
  }})
  let client = new_client()
  {fn_name}(client)
  let calls = http_mock_calls()
  assert_eq(len(calls), 1)
  assert_eq(calls[0].method, {method})
}}
"#,
            fn_name = smoke.function_name,
            method = harn_string_literal(&smoke.method),
            url = harn_string_literal(&format!(
                "{}{}",
                default_base_url.trim_end_matches('/'),
                smoke.path
            )),
        );
    }
    r#"import { new_client, pagination_plans } from "../src/lib"

pipeline test_generated_sdk_smoke(task) {
  let client = new_client()
  assert_eq(type_of(client), "dict")
  assert_eq(type_of(pagination_plans()), "list")
}
"#
    .to_string()
}

fn readme_source(
    package_name: &str,
    description: &str,
    spec_source: &str,
    module_name: &str,
    client_name: &str,
) -> String {
    format!(
        r#"# {package_name}

{description}

This is a focused Harn SDK package generated from `{spec_source}`. It should
cover the API surface described by that spec; it is not intended to become a
whole-provider clone with hand-written coverage for unrelated products.

## Develop

```bash
harn install
harn check src/lib.harn
harn test tests/
harn package check
harn package docs --check
harn package pack --dry-run
```

## Regenerate

```bash
harn install
harn run scripts/regen.harn
harn package docs
```

`harn-openapi` is declared in `harn.toml` so regeneration works in CI. Override
the reviewed default only when intentionally testing a newer generator:

```bash
harn add github.com/burin-labs/harn-openapi@<rev>
```

## Install Into Another Project

```bash
harn add ../{package_name}
harn install
```

Consumers import the generated module through the package export:

```harn
import {{ new_client }} from "{package_name}/{module_name}"
```

`new_client()` returns a `{client_name}` handle with the default base URL from
the OpenAPI document. Pass an explicit base URL when testing or targeting a
different environment.

## Hand-Written HTTP

Hand-written `harness.net` calls are acceptable for one-off private endpoints,
temporary admin probes, or API gaps that are not present in the OpenAPI
document. Repeated provider API coverage, connector helpers, and user-facing
package exports should live in this generated SDK or a small Harn wrapper over
it so auth, rate-limit metadata, pagination helpers, and tests stay in one
place.
"#
    )
}

fn first_server_url(value: &Value) -> Option<String> {
    value
        .get("servers")?
        .as_array()?
        .iter()
        .find_map(|server| server.get("url")?.as_str().map(ToString::to_string))
        .filter(|url| !url.trim().is_empty())
}

fn spec_title(value: &Value) -> Option<&str> {
    value.get("info")?.get("title")?.as_str()
}

#[derive(Debug, Clone)]
struct SmokeOperation {
    function_name: String,
    method: String,
    path: String,
}

fn find_smoke_operation(value: &Value) -> Option<SmokeOperation> {
    let paths = value.get("paths")?.as_object()?;
    let function_name_counts = operation_function_name_counts(paths);
    let mut path_entries: Vec<_> = paths.iter().collect();
    path_entries.sort_by(|left, right| left.0.cmp(right.0));
    let methods = ["get", "delete", "head", "options", "post", "put", "patch"];
    for (path, item) in path_entries {
        if path.contains('{') {
            continue;
        }
        let Some(item) = item.as_object() else {
            continue;
        };
        for method in methods {
            let Some(operation) = item.get(method).and_then(Value::as_object) else {
                continue;
            };
            if operation.contains_key("requestBody")
                || has_required_parameters(item)
                || has_required_parameters(operation)
            {
                continue;
            }
            let operation_id = operation
                .get("operationId")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .unwrap_or_else(|| synth_operation_id(method, path));
            let function_name = sanitize_openapi_function_name(&operation_id);
            if function_name_counts
                .get(&function_name)
                .copied()
                .unwrap_or(0)
                != 1
            {
                continue;
            }
            return Some(SmokeOperation {
                function_name,
                method: method.to_ascii_uppercase(),
                path: path.clone(),
            });
        }
    }
    None
}

fn operation_function_name_counts(
    paths: &serde_json::Map<String, Value>,
) -> std::collections::HashMap<String, usize> {
    let mut counts = std::collections::HashMap::new();
    let methods = ["get", "delete", "head", "options", "post", "put", "patch"];
    for (path, item) in paths {
        let Some(item) = item.as_object() else {
            continue;
        };
        for method in methods {
            let Some(operation) = item.get(method).and_then(Value::as_object) else {
                continue;
            };
            let operation_id = operation
                .get("operationId")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .unwrap_or_else(|| synth_operation_id(method, path));
            *counts
                .entry(sanitize_openapi_function_name(&operation_id))
                .or_insert(0) += 1;
        }
    }
    counts
}

fn has_required_parameters(operation: &serde_json::Map<String, Value>) -> bool {
    operation
        .get("parameters")
        .and_then(Value::as_array)
        .is_some_and(|parameters| {
            parameters.iter().any(|parameter| {
                parameter
                    .get("required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
        })
}

fn synth_operation_id(method: &str, path: &str) -> String {
    let cleaned = non_alphanumeric_to_underscore(path);
    let trimmed = cleaned.trim_matches('_');
    format!("{method}_{trimmed}")
}

fn sanitize_openapi_function_name(raw: &str) -> String {
    let mut out = non_alphanumeric_to_underscore(raw)
        .trim_matches('_')
        .to_ascii_lowercase();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    if out.is_empty() {
        out = "op".to_string();
    }
    if out.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        out = format!("op_{out}");
    }
    if is_harn_reserved(&out) {
        out.push('_');
    }
    out
}

fn non_alphanumeric_to_underscore(raw: &str) -> String {
    raw.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

fn is_harn_reserved(value: &str) -> bool {
    matches!(
        value,
        "if" | "else"
            | "elif"
            | "for"
            | "in"
            | "while"
            | "match"
            | "let"
            | "var"
            | "fn"
            | "pub"
            | "return"
            | "throw"
            | "try"
            | "catch"
            | "finally"
            | "pipeline"
            | "import"
            | "from"
            | "as"
            | "type"
            | "interface"
            | "enum"
            | "true"
            | "false"
            | "nil"
            | "and"
            | "or"
            | "not"
            | "skill"
            | "tool"
            | "body"
            | "client"
    )
}

fn expand_tilde(raw: &str) -> PathBuf {
    if raw == "~" {
        return harn_vm::user_dirs::home_dir().unwrap_or_else(|| PathBuf::from(raw));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = harn_vm::user_dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

fn expand_tilde_path(path: &Path) -> PathBuf {
    path.to_str()
        .map(expand_tilde)
        .unwrap_or_else(|| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::test_support::{run_git, test_git_command};
    use crate::package::{
        check_package_impl, generate_package_docs_impl, install_packages_in, pack_package_impl,
        PackageWorkspace,
    };

    #[test]
    fn sanitize_openapi_function_names_like_harn_openapi() {
        assert_eq!(sanitize_openapi_function_name("get-widget"), "get_widget");
        assert_eq!(sanitize_openapi_function_name("123 ping"), "op_123_ping");
        assert_eq!(sanitize_openapi_function_name("type"), "type_");
    }

    #[test]
    fn harn_openapi_dependency_defaults_to_reviewed_rev() {
        let args = openapi_args_for_test();
        let dependency = harn_openapi_dependency(&args).unwrap();

        assert_eq!(
            dependency,
            r#"{ git = "https://github.com/burin-labs/harn-openapi", rev = "v0.1.1-rc.1" }"#
        );
    }

    #[test]
    fn harn_openapi_dependency_respects_branch_override() {
        let mut args = openapi_args_for_test();
        args.harn_openapi_branch = Some("main".to_string());
        let dependency = harn_openapi_dependency(&args).unwrap();

        assert_eq!(
            dependency,
            r#"{ git = "https://github.com/burin-labs/harn-openapi", branch = "main" }"#
        );
    }

    #[tokio::test]
    async fn load_spec_accepts_file_url() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = tmp.path().join("openapi.yaml");
        fs::write(
            &spec,
            r"openapi: 3.1.0
info:
  title: Tiny
  version: 1.0.0
paths: {}
",
        )
        .unwrap();

        let url = Url::from_file_path(&spec).unwrap();
        let loaded = load_spec(url.as_str()).await.unwrap();
        assert_eq!(loaded.relative_path, "openapi/openapi.yaml");
        assert_eq!(
            loaded.value.pointer("/info/title").and_then(Value::as_str),
            Some("Tiny")
        );
    }

    #[tokio::test]
    async fn scaffold_openapi_package_passes_local_package_gates() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_openapi = fake_harn_openapi_repo(tmp.path());
        let rev = run_git(&harn_openapi, &["rev-parse", "HEAD"]);
        let spec = tmp.path().join("openapi.json");
        fs::write(
            &spec,
            r#"{
  "openapi": "3.1.0",
  "info": {"title": "Tiny", "version": "1.0.0"},
  "servers": [{"url": "https://api.example.test"}],
  "paths": {
    "/ping": {
      "get": {
        "operationId": "ping",
        "responses": {"200": {"description": "ok"}}
      }
    }
  }
}
"#,
        )
        .unwrap();
        let out = tmp.path().join("tiny-sdk-harn");

        scaffold_openapi_package(&PackageScaffoldOpenapiArgs {
            name: "tiny-sdk-harn".to_string(),
            module_name: Some("tiny_sdk".to_string()),
            client_name: Some("TinyClient".to_string()),
            spec: spec.display().to_string(),
            out: Some(out.clone()),
            description: None,
            default_base_url: None,
            harn_openapi_path: Some(harn_openapi.clone()),
            harn_openapi_git: Some(harn_openapi.display().to_string()),
            harn_openapi_rev: Some(rev),
            harn_openapi_branch: None,
            force: false,
        })
        .await
        .unwrap();

        assert!(out.join("src/lib.harn").exists());
        assert!(out.join("scripts/regen.harn").exists());
        assert!(out.join("docs/api.md").exists());
        let manifest = fs::read_to_string(out.join("harn.toml")).unwrap();
        assert!(manifest.contains("[exports]\ntiny_sdk = \"src/lib.harn\""));
        assert!(manifest.contains("harn-openapi = { git = "));

        install_packages_in(
            &PackageWorkspace::for_test(&out, tmp.path().join(".cache")),
            false,
            None,
            false,
        )
        .unwrap();
        let outcome = execute_run_with_sandbox_options(
            &out.join("tests/smoke.harn").display().to_string(),
            false,
            HashSet::new(),
            Vec::new(),
            Vec::new(),
            CliLlmMockMode::Off,
            None,
            RunProfileOptions::default(),
            RunSandboxOptions::disabled(),
        )
        .await;
        assert_eq!(outcome.exit_code, 0, "{}{}", outcome.stderr, outcome.stdout);

        let report = check_package_impl(Some(&out)).unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        generate_package_docs_impl(Some(&out), None, true).unwrap();
        pack_package_impl(Some(&out), None, true).unwrap();
    }

    #[tokio::test]
    async fn force_preserves_extra_files() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_openapi = fake_harn_openapi_repo(tmp.path());
        let rev = run_git(&harn_openapi, &["rev-parse", "HEAD"]);
        let spec = tmp.path().join("openapi.json");
        fs::write(
            &spec,
            r#"{"openapi":"3.1.0","info":{"title":"Tiny","version":"1.0.0"},"paths":{}}"#,
        )
        .unwrap();
        let out = tmp.path().join("tiny-sdk-harn");
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("keep.txt"), "keep").unwrap();

        scaffold_openapi_package(&PackageScaffoldOpenapiArgs {
            name: "tiny-sdk-harn".to_string(),
            module_name: Some("tiny_sdk".to_string()),
            client_name: Some("TinyClient".to_string()),
            spec: spec.display().to_string(),
            out: Some(out.clone()),
            description: None,
            default_base_url: Some("https://api.example.test".to_string()),
            harn_openapi_path: Some(harn_openapi.clone()),
            harn_openapi_git: Some(harn_openapi.display().to_string()),
            harn_openapi_rev: Some(rev),
            harn_openapi_branch: None,
            force: true,
        })
        .await
        .unwrap();

        assert_eq!(fs::read_to_string(out.join("keep.txt")).unwrap(), "keep");
        assert!(out.join("harn.toml").exists());
    }

    fn fake_harn_openapi_repo(root: &Path) -> PathBuf {
        let repo = root.join(format!("fake-harn-openapi-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(
            repo.join("harn.toml"),
            r#"[package]
name = "harn-openapi"
version = "0.1.0"
description = "Fake harn-openapi for scaffold tests."
license = "MIT OR Apache-2.0"
repository = "https://example.test/harn-openapi"
harn = ">=0.8,<0.9"
docs_url = "docs/api.md"

[exports]
default = "src/lib.harn"

[dependencies]
"#,
        )
        .unwrap();
        fs::create_dir_all(repo.join("docs")).unwrap();
        fs::write(repo.join("docs/api.md"), "# API\n").unwrap();
        fs::write(
            repo.join("src/lib.harn"),
            r#"pub fn parse(raw: string) {
  return json_parse(raw)
}

pub fn codegen_module(doc, options) -> string {
  return "/* generated */\n"
    + "/** Create a new client handle. */\n"
    + "pub fn new_client(base_url: string = \"https://api.example.test\", extra_headers: dict = {}) -> dict {\n"
    + "  return {base_url: base_url, extra_headers: extra_headers}\n"
    + "}\n\n"
    + "/** Pagination plans detected from the OpenAPI operation shapes. */\n"
    + "pub fn pagination_plans() -> list {\n"
    + "  return []\n"
    + "}\n\n"
    + "/** ping */\n"
    + "pub fn ping(client: dict) -> nil {\n"
    + "  let resp = http_get(client.base_url + \"/ping\", {headers: {}})\n"
    + "  if resp.status >= 400 { throw \"ping failed\" }\n"
    + "  return nil\n"
    + "}\n"
}
"#,
        )
        .unwrap();
        let init = test_git_command(&repo)
            .args(["init", "-b", "main"])
            .output()
            .unwrap();
        if !init.status.success() {
            let fallback = test_git_command(&repo).arg("init").output().unwrap();
            assert!(
                fallback.status.success(),
                "git init failed: {}",
                String::from_utf8_lossy(&fallback.stderr)
            );
        }
        run_git(&repo, &["config", "user.email", "tests@example.com"]);
        run_git(&repo, &["config", "user.name", "Harn Tests"]);
        run_git(&repo, &["config", "core.hooksPath", "/dev/null"]);
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-m", "initial"]);
        repo
    }

    fn openapi_args_for_test() -> PackageScaffoldOpenapiArgs {
        PackageScaffoldOpenapiArgs {
            name: "tiny-sdk-harn".to_string(),
            module_name: None,
            client_name: None,
            spec: "openapi.json".to_string(),
            out: None,
            description: None,
            default_base_url: None,
            harn_openapi_path: None,
            harn_openapi_git: None,
            harn_openapi_rev: None,
            harn_openapi_branch: None,
            force: false,
        }
    }
}
