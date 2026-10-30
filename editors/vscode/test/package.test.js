const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { createRequire } = require("node:module");
const { pipeline } = require("node:stream/promises");
const test = require("node:test");
const { createVSIX } = require("@vscode/vsce");
const yauzl = require("yauzl");

const extensionRoot = path.join(__dirname, "..");

function extractExtension(archive, destination) {
  return new Promise((resolve, reject) => {
    yauzl.open(archive, { lazyEntries: true }, (error, zip) => {
      if (error) return reject(error);
      zip.on("error", reject);
      zip.on("end", resolve);
      zip.on("entry", (entry) => {
        if (!entry.fileName.startsWith("extension/") || entry.fileName.endsWith("/")) {
          zip.readEntry();
          return;
        }
        const target = path.resolve(destination, entry.fileName);
        if (!target.startsWith(destination + path.sep)) {
          zip.close();
          reject(new Error(`Archive entry escapes destination: ${entry.fileName}`));
          return;
        }
        fs.mkdirSync(path.dirname(target), { recursive: true });
        zip.openReadStream(entry, (streamError, stream) => {
          if (streamError) {
            zip.close();
            reject(streamError);
            return;
          }
          pipeline(stream, fs.createWriteStream(target)).then(
            () => zip.readEntry(),
            (writeError) => {
              zip.close();
              reject(writeError);
            },
          );
        });
      });
      zip.readEntry();
    });
  });
}

test("the actual VSIX resolves runtime dependencies without packaging tools", async (t) => {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "harn-vsix-test-"));
  t.after(() => fs.rmSync(temporary, { recursive: true, force: true }));
  const archive = path.join(temporary, "harn.vsix");
  await createVSIX({
    cwd: extensionRoot,
    packagePath: archive,
    gitTagVersion: false,
    updatePackageJson: false,
  });
  const extracted = path.join(temporary, "unpacked");
  await extractExtension(archive, extracted);
  const packagedRoot = fs.realpathSync(path.join(extracted, "extension"));
  const manifest = JSON.parse(fs.readFileSync(path.join(packagedRoot, "package.json")));
  const entry = path.resolve(packagedRoot, manifest.main);
  assert.ok(fs.existsSync(entry), "the packaged activation entry exists");
  const english = JSON.parse(fs.readFileSync(path.join(packagedRoot, "package.nls.json")));
  const runtime = JSON.parse(fs.readFileSync(path.join(packagedRoot, "l10n/bundle.l10n.json")));
  for (const locale of ["es", "fr", "de", "pt-br", "it", "ja", "ko", "zh-cn"]) {
    const labels = JSON.parse(fs.readFileSync(path.join(packagedRoot, `package.nls.${locale}.json`)));
    const messages = JSON.parse(fs.readFileSync(path.join(packagedRoot, `l10n/bundle.l10n.${locale}.json`)));
    assert.deepEqual(Object.keys(labels).sort(), Object.keys(english).sort());
    assert.deepEqual(Object.keys(messages).sort(), Object.keys(runtime).sort());
    assert.notEqual(messages["Open a .harn file first"], runtime["Open a .harn file first"]);
    assert.equal(manifest.l10n, "./l10n");
  }
  const packagedRequire = createRequire(entry);
  assert.match(fs.readFileSync(entry, "utf8"), /require\("vscode-languageclient\/node"\)/);
  assert.ok(
    packagedRequire.resolve("vscode-languageclient/node").startsWith(packagedRoot + path.sep),
    "the activation import resolves inside the actual package",
  );

  const lock = JSON.parse(fs.readFileSync(path.join(extensionRoot, "package-lock.json")));
  let productionPackages = 0;
  for (const [location, dependency] of Object.entries(lock.packages)) {
    if (!location || dependency.dev) continue;
    const packagePath = path.join(packagedRoot, location, "package.json");
    assert.ok(fs.existsSync(packagePath), `missing production package: ${location}`);
    const packageManifest = JSON.parse(fs.readFileSync(packagePath));
    const dependencyRequire = createRequire(packagePath);
    for (const name of Object.keys(packageManifest.dependencies ?? {})) {
      assert.ok(
        dependencyRequire.resolve(name).startsWith(packagedRoot + path.sep),
        `${location} resolves ${name} inside the package`,
      );
    }
    productionPackages += 1;
  }
  assert.ok(productionPackages > 0, "the archive check reached production dependencies");
  t.diagnostic(`Verified ${productionPackages} packaged production dependencies`);
  for (const name of Object.keys(manifest.devDependencies)) {
    assert.equal(
      fs.existsSync(path.join(packagedRoot, "node_modules", name)),
      false,
      `development tool must not ship: ${name}`,
    );
  }
});
