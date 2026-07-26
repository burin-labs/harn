// Contribution wiring for `.harn.prompt` files.
//
// The Rust side proves the generated grammar matches the template engine.
// Nothing there can see whether the extension actually *contributes* that
// grammar, activates on the language, or ships a usable language
// configuration — which is what this file checks.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const extensionRoot = path.join(__dirname, "..");

function readJson(...parts) {
  return JSON.parse(
    fs.readFileSync(path.join(extensionRoot, ...parts), "utf8")
  );
}

const manifest = readJson("package.json");

function promptLanguage() {
  const language = manifest.contributes.languages.find(
    (entry) => entry.id === "harn-prompt"
  );
  assert.ok(language, "package.json contributes a harn-prompt language");
  return language;
}

test("prompt files activate the extension", () => {
  assert.ok(
    manifest.activationEvents.includes("onLanguage:harn-prompt"),
    "opening a prompt file must activate the extension, or none of its " +
      "commands and no language client are available"
  );
});

test("the prompt language claims both prompt extensions", () => {
  assert.deepEqual(promptLanguage().extensions, [".harn.prompt", ".prompt"]);
});

test("the prompt language points at a configuration that exists", () => {
  const configured = promptLanguage().configuration;
  assert.ok(configured, "harn-prompt declares a language configuration");
  const resolved = path.join(extensionRoot, configured);
  assert.ok(
    fs.existsSync(resolved),
    `language configuration ${configured} is missing`
  );
  JSON.parse(fs.readFileSync(resolved, "utf8"));
});

test("toggle comment uses the template comment pair", () => {
  const config = readJson(promptLanguage().configuration);
  assert.deepEqual(config.comments.blockComment, ["{{#", "#}}"]);
});

test("typing {{ closes the directive", () => {
  const config = readJson(promptLanguage().configuration);
  const pair = config.autoClosingPairs.find((p) => p.open === "{{");
  assert.ok(pair, "no auto-closing pair for `{{`");
  assert.equal(pair.close.trim(), "}}");
});

test("folding marks block directives and only block directives", () => {
  const config = readJson(promptLanguage().configuration);
  const start = new RegExp(config.folding.markers.start);
  const end = new RegExp(config.folding.markers.end);

  // [line, expected start marker, expected end marker]
  const cases = [
    ["{{ if ready }}", true, false],
    ["{{ for item in items }}", true, false],
    ['{{ section "task" }}', true, false],
    ["{{ raw }}", true, false],
    ["{{- if ready -}}", true, false],
    ["{{ end }}", false, true],
    ["{{ endsection }}", false, true],
    ["{{ endraw }}", false, true],
    ['{{ endsection "task" }}', false, true],
    // An opener must not fire on a closer that merely contains its spelling.
    // `endsection` contains `section`; `endraw` contains `raw`.
    ["{{ endsection }}", false, true],
    // Interpolations and non-block directives never fold.
    ["{{ name | upper }}", false, false],
    ['{{ include "shared.prompt" }}', false, false],
    ["ordinary prose", false, false],
  ];

  for (const [line, wantStart, wantEnd] of cases) {
    assert.equal(
      start.test(line),
      wantStart,
      `start marker mismatch for ${JSON.stringify(line)}`
    );
    assert.equal(
      end.test(line),
      wantEnd,
      `end marker mismatch for ${JSON.stringify(line)}`
    );
  }
});

test("the contributed grammar is the generated one", () => {
  const grammar = manifest.contributes.grammars.find(
    (entry) => entry.language === "harn-prompt"
  );
  assert.ok(grammar, "package.json contributes a harn-prompt grammar");

  const parsed = readJson(grammar.path);
  assert.equal(parsed.scopeName, grammar.scopeName);
  assert.match(
    parsed._generated ?? "",
    /make gen-prompt-grammar/,
    "grammar is missing its generated-file banner — was it hand-edited?"
  );
});

test("filter highlighting tolerates any spacing after the pipe", () => {
  const grammar = manifest.contributes.grammars.find(
    (entry) => entry.language === "harn-prompt"
  );
  const filter = new RegExp(readJson(grammar.path).repository.filter.match);

  for (const source of ["{{ x|upper }}", "{{ x | upper }}", "{{ x |  upper }}"]) {
    assert.ok(
      filter.test(source),
      `filter went unhighlighted in ${JSON.stringify(source)}`
    );
  }
  assert.ok(
    !filter.test("{{ x | uppercase }}"),
    "a filter the engine does not implement must not highlight as one"
  );
});

test("the language server is asked to handle prompt documents", () => {
  const source = fs.readFileSync(
    path.join(extensionRoot, "src", "extension.ts"),
    "utf8"
  );
  assert.match(
    source,
    /documentSelector:[\s\S]*language:\s*"harn-prompt"/,
    "the LSP document selector no longer includes harn-prompt"
  );
});
