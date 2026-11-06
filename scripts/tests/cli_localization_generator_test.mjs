import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const source = fs.realpathSync(path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../.."));
const output = "crates/harn-stdlib/src/stdlib/cli/localization";
const catalog = "crates/harn-stdlib/src/catalog_entries.rs";

test("every registered CLI renderer preserves localized text, literal arguments and refusals", (t) => {
  assert.ok(process.env.HARN_BIN, "owning Make target supplies the actual CLI");
  const registry = JSON.parse(fs.readFileSync(path.join(source, "crates/harn-cli/src/localization_scripts.json")));
  const directory = path.join(source, "crates/harn-cli/src/localization");
  const catalogs = Object.fromEntries(fs.readdirSync(directory).filter((name) => name.endsWith(".json"))
    .map((name) => [name.slice(0, -5), JSON.parse(fs.readFileSync(path.join(directory, name)))]));
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "harn-cli-renderers-"));
  const render = (template, values) => template.replace(/\{\{|\}\}|\{(\d+)\}/g,
    (token, index) => token === "{{" ? "{" : token === "}}" ? "}" : values[Number(index)]);
  let functions = 0;
  let localized = 0;
  let measured = 0;
  try {
    for (const [relative, surface] of Object.entries(registry.sources)) {
      const namespace = relative.slice("crates/harn-stdlib/src/stdlib/cli/".length, -5);
      const foreign = Object.keys(registry.messages).find((key) => !(key in surface.calls));
      assert.ok(foreign, "namespace refusal needs a real message owned by another surface");
      const foreignValues = Array.from({length: registry.messages[foreign]}, (_, index) => `foreign${index}`);
      const cases = [];
      const refusals = [];
      for (const key of Object.keys(surface.calls).sort()) {
        const values = Array.from({length: registry.messages[key]}, (_, index) => `slot${index}: {0} %@ "\\\n🍒`);
        for (const locale of [...Object.keys(catalogs).sort(), "unknown-locale-control"]) {
          cases.push({locale, key, values});
        }
        refusals.push({locale: "en", key, values: [...values, "unexpected argument"]});
        functions++;
      }
      const program = path.join(temporary, "render.harn");
      fs.writeFileSync(program, `import * as renderer from "std/cli/localization/${namespace}"
type RenderCase = {locale: string, key: string, values: list<string>}
fn main(harness: Harness) {
  const cases: list<RenderCase> = ${JSON.stringify(cases)}
  for item in cases {
    harness.stdio.println(json_stringify(renderer.cli_message(item.locale, item.key, item.values)))
  }
  const refusals: list<RenderCase> = ${JSON.stringify(refusals)}
  for item in refusals {
    const refused = try { renderer.cli_message(item.locale, item.key, item.values); false } catch { true }
    assert(refused, "extra localization argument must refuse")
  }
  const unknown = try { renderer.cli_message("en", "unregistered-message-control", []); false } catch { true }
  assert(unknown, "unregistered localization message must refuse")
  const foreign = try { renderer.cli_message("en", ${JSON.stringify(foreign)}, ${JSON.stringify(foreignValues)}); false } catch { true }
  assert(foreign, "a message from another namespace must refuse")
  harness.stdio.println(json_stringify({arity: len(refusals), unknown: true, foreign: true}))
}
`);
      const result = spawnSync(process.env.HARN_BIN, ["run", program], {
        cwd: source, env: {...process.env, HARN_LLM_CALLS_DISABLED: "1"},
        encoding: "utf8", timeout: 120_000, maxBuffer: 8 * 1024 * 1024,
      });
      assert.equal(result.status, 0, `${namespace}: ${result.stderr || result.error}`);
      const actual = result.stdout.trimEnd().split("\n").map((line) => JSON.parse(line));
      assert.deepEqual(actual.pop(), {arity: refusals.length, unknown: true, foreign: true});
      assert.deepEqual(actual, cases.map(({locale, key, values}) => render((catalogs[locale] ?? catalogs.en)[key], values)), namespace);
      localized += cases.filter(({key, values}, index) => actual[index] !== render(catalogs.en[key], values)).length;
      measured += actual.length;
    }
    assert.deepEqual([...new Set(Object.values(registry.sources).flatMap((surface) => Object.keys(surface.calls)))].sort(), Object.keys(registry.messages).sort());
    assert.ok(functions > 100, "the public renderer census must be nonempty");
    assert.ok(localized > 100, "the probe must reach translations distinct from English");
    assert.equal(measured, functions * (Object.keys(catalogs).length + 1));
    t.diagnostic(`Verified ${functions} renderer paths, ${measured} locale outputs, ${functions} arity refusals, ${Object.keys(registry.sources).length} unknown-message and cross-namespace refusals; ${localized} outputs differ from English`);
  } finally {
    fs.rmSync(temporary, {recursive: true, force: true});
  }
});

test("generator rejects every unexpected output and retires only registered generated files", () => {
  assert.ok(process.env.HARN_BIN, "owning Make target supplies the actual CLI");
  const binary = fs.realpathSync(process.env.HARN_BIN);
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "harn-cli-generator-test-")));
  try {
    const registry = JSON.parse(fs.readFileSync(path.join(source, "crates/harn-cli/src/localization_scripts.json"), "utf8"));
    const files = ["scripts/generate_cli_localization.mjs", catalog,
      "crates/harn-cli/src/localization_scripts.json", "crates/harn-vm/src/environment_registry_names.txt",
      `${output}.harn`, ...Object.keys(registry.sources)];
    for (const relative of files) {
      assert.ok(!relative.includes("..") && !path.isAbsolute(relative));
      const target = path.join(root, relative);
      fs.mkdirSync(path.dirname(target), { recursive: true });
      fs.copyFileSync(path.join(source, relative), target);
    }
    for (const relative of [output, "crates/harn-cli/src/localization"]) {
      fs.cpSync(path.join(source, relative), path.join(root, relative), { recursive: true });
    }
    const home = path.join(root, "control-home");
    fs.mkdirSync(home);
    const env = { PATH: process.env.PATH, HOME: home, HARN_CACHE_DIR: path.join(home, "cache"),
      XDG_CONFIG_HOME: path.join(home, "config"), TMPDIR: root, TMP: root, TEMP: root };
    for (const name of ["SystemRoot", "WINDIR", "COMSPEC"]) {
      if (process.env[name]) env[name] = process.env[name];
    }
    const run = (check = true) => spawnSync(process.execPath,
      [path.join(root, "scripts/generate_cli_localization.mjs"), ...(check ? ["--check"] : []), "--harn-bin", binary],
      { cwd: root, env, encoding: "utf8", timeout: 120_000, maxBuffer: 8 * 1024 * 1024 });
    const positive = run();
    assert.equal(positive.status, 0, positive.stderr);
    const measured = JSON.parse(positive.stdout);
    assert.ok(measured.sources > 10 && measured.expectedCalls > 100 && measured.generated > measured.sources);
    const renderers = fs.readdirSync(path.join(root, output), {recursive: true})
      .filter((name) => name.endsWith(".harn"))
      .flatMap((name) => [...fs.readFileSync(path.join(root, output, name), "utf8").matchAll(/^pub fn (message_\d+)\(/gm)].map((match) => match[1]));
    assert.equal(renderers.length, Object.keys(registry.messages).length, "each canonical message has one renderer");
    assert.equal(new Set(renderers).size, renderers.length, "renderer ownership cannot repeat across modules");
    assert.equal(measured.measuredCalls, measured.expectedCalls);
    assert.equal(measured.pending, 0);
    assert.deepEqual(measured.failed, []);

    const foreign = path.join(root, output, "unregistered-control.harn");
    const foreignText = "// foreign control: preserve this content\n";
    fs.writeFileSync(foreign, foreignText);
    for (const check of [true, false]) {
      const rejected = run(check);
      assert.notEqual(rejected.status, 0, "unexpected output cannot receive a passing projection receipt");
      assert.match(rejected.stderr, /unregistered-control\.harn/);
      assert.equal(fs.readFileSync(foreign, "utf8"), foreignText, "unregistered content must never be deleted");
    }
    fs.unlinkSync(foreign);

    const stale = path.join(root, output, "registered-control.harn");
    const staleText = "/** Generated by scripts/generate_cli_localization.mjs. Do not edit. */\npub const stale = true\n";
    fs.writeFileSync(stale, staleText);
    const catalogPath = path.join(root, catalog);
    fs.writeFileSync(catalogPath, fs.readFileSync(catalogPath, "utf8").replace(
      "    // END GENERATED CLI LOCALIZATION MODULES",
      '    "cli/localization/registered-control" => "stdlib/cli/localization/registered-control.harn",\n    // END GENERATED CLI LOCALIZATION MODULES'));
    const staleCheck = run();
    assert.notEqual(staleCheck.status, 0);
    assert.match(staleCheck.stderr, /registered-control\.harn/);
    assert.equal(fs.readFileSync(stale, "utf8"), staleText);
    fs.writeFileSync(stale, foreignText);
    const unknown = run(false);
    assert.notEqual(unknown.status, 0);
    assert.match(unknown.stderr, /unknown contents/);
    assert.equal(fs.readFileSync(stale, "utf8"), foreignText);
    fs.writeFileSync(stale, staleText);
    const retired = run(false);
    assert.equal(retired.status, 0, retired.stderr);
    assert.equal(fs.existsSync(stale), false, "only the validated obsolete projection was retired");
    const clean = run();
    assert.equal(clean.status, 0, clean.stderr);
    assert.equal(JSON.parse(clean.stdout).measuredCalls, measured.expectedCalls);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});
