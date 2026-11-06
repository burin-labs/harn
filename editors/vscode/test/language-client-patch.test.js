const assert = require("node:assert/strict");
const { execFileSync, spawnSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

test("the dependency patch refuses unknown source before changing either file", (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "harn-language-client-patch-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const script = path.join(root, "scripts", "patch-language-client.cjs");
  const client = path.join(root, "node_modules", "vscode-languageclient");
  fs.mkdirSync(path.dirname(script), { recursive: true });
  fs.mkdirSync(path.join(client, "lib/common/utils"), { recursive: true });
  fs.copyFileSync(path.join(__dirname, "../scripts/patch-language-client.cjs"), script);
  fs.copyFileSync(path.join(__dirname, "../node_modules/vscode-languageclient/package.json"), path.join(client, "package.json"));
  const names = ["lib/common/utils/globPattern.js", "lib/common/fileOperations.js"];
  const originals = names.map((name) => fs.readFileSync(path.join(__dirname, "../node_modules/vscode-languageclient", name), "utf8")
    .replace("matcher.set.length === 0", "!matcher.makeRe()"));
  for (const [index, name] of names.entries()) fs.writeFileSync(path.join(client, name), originals[index]);
  fs.appendFileSync(path.join(client, names[1]), "\n// unknown upstream change\n");
  const refused = spawnSync(process.execPath, [script], { encoding: "utf8" });
  assert.notEqual(refused.status, 0);
  assert.match(refused.stderr, /Unrecognized language-client source/);
  assert.equal(fs.readFileSync(path.join(client, names[0]), "utf8"), originals[0]);
  fs.writeFileSync(path.join(client, names[1]), originals[1]);
  execFileSync(process.execPath, [script]);
  execFileSync(process.execPath, [script]);
  for (const name of names) assert.match(fs.readFileSync(path.join(client, name), "utf8"), /matcher\.set\.length === 0/);
});
