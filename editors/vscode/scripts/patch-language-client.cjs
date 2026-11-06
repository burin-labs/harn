// The language client only uses makeRe() as an empty-pattern check. Minimatch
// 11 removed that method; its parsed set supplies the same check. Keep this
// patch pinned to the measured upstream files until the client supports v11.
const { createHash } = require("node:crypto");
const { readFileSync, writeFileSync } = require("node:fs");
const { join } = require("node:path");

const root = join(__dirname, "..", "node_modules", "vscode-languageclient");
const version = JSON.parse(readFileSync(join(root, "package.json"), "utf8")).version;
if (version !== "11.0.0") throw new Error(`Review the matcher patch for vscode-languageclient ${version}`);
const files = [
  ["lib/common/utils/globPattern.js", "ec8f8ed49366dc71c63a6908f0de06568086058cef3e59d6665df4c721826129", "6bab870a3edd1d991af511e2e808afbaac10e236cb36422716b551403db75b7b"],
  ["lib/common/fileOperations.js", "5d38e3f825f80f4e72fd931c50d014cc86f636218b724740377fc7bc9023f907", "9187d6e9b8090916eaa82ad4cb0b61de6fa78dd2908f14852fe4f103e69e7030"],
];
const digest = (text) => createHash("sha256").update(text).digest("hex");
const replacements = files.map(([relative, original, patched]) => {
  const path = join(root, relative);
  const source = readFileSync(path, "utf8");
  const actual = digest(source);
  if (actual === patched) return null;
  if (actual !== original) throw new Error(`Unrecognized language-client source: ${relative}`);
  const replacement = source.replace("!matcher.makeRe()", "matcher.set.length === 0");
  if (digest(replacement) !== patched) throw new Error(`Language-client patch mismatch: ${relative}`);
  return { path, replacement };
});
for (const item of replacements) if (item) writeFileSync(item.path, item.replacement);
