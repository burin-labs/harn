// Packaging adapter for cargo-about's locked dependency/license inventory.
// No compiler is invoked. License policy and target coverage retain their owners.
import { readFileSync, writeFileSync, readdirSync, lstatSync, mkdtempSync, rmSync, mkdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { parse } from 'smol-toml';
import { createHash } from 'node:crypto';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const label = pkg => `${pkg.name} ${pkg.version}`;
function required(value, context) {
  if (typeof value !== 'string' || !value.trim()) throw new Error(`Missing ${context}`);
  return value;
}

function noticeFiles(directory, prefix = '') {
  const files = [];
  for (const name of readdirSync(join(directory, prefix)).sort()) {
    const relative = join(prefix, name);
    const info = lstatSync(join(directory, relative));
    if (info.isDirectory()) files.push(...noticeFiles(directory, relative));
    else if (/^NOTICES?(?:[._-].*)?$/i.test(name)) {
      if (!info.isFile()) throw new Error(`NOTICE must be a regular file: ${relative}`);
      files.push(relative);
    }
  }
  return files;
}

export function normalizeInventory(about) {
  if (!Array.isArray(about.crates) || !about.crates.length ||
      !Array.isArray(about.licenses) || !about.licenses.length) throw new Error('Empty dependency or license inventory');
  return {
    crates: about.crates.map(entry => {
      const pkg = entry.package;
      const directory = dirname(required(pkg.manifest_path, `manifest path for ${label(pkg)}`));
      return { license: entry.license, package: { name: pkg.name, version: pkg.version, source: pkg.source ?? null,
        notices: noticeFiles(directory).map(name => ({ name: name.split('\\').join('/'),
          text: required(readFileSync(join(directory, name), 'utf8'), `NOTICE for ${label(pkg)}`) })) } };
    }),
    licenses: about.licenses.map(license => ({ id: license.id, text: license.text,
      used_by: license.used_by?.map(use => ({ crate: { name: use.crate.name, version: use.crate.version } })) })),
  };
}

export function sourceFingerprint(repository) {
  const workspace = parse(readFileSync(join(repository, 'Cargo.toml'), 'utf8')).workspace;
  if (!Array.isArray(workspace?.members) || !workspace.members.length ||
      workspace.members.some(member => typeof member !== 'string' || /[?*]|^\/|\.\./.test(member))) {
    throw new Error('Expected explicit workspace members for release license fingerprint');
  }
  const files = ['Cargo.toml', 'Cargo.lock', 'deny.toml', '.github/release-runner-policy.json',
    'package.json', 'package-lock.json', 'scripts/release_third_party_notices.mjs', 'LICENSE-MIT', 'LICENSE-APACHE',
    ...workspace.members.map(member => `${member}/Cargo.toml`),
    ...workspace.members.flatMap(member => noticeFiles(join(repository, member))
      .map(file => `${member}/${file.split('\\').join('/')}`))].sort();
  return createHash('sha256').update(JSON.stringify(files.map(file => [file,
    createHash('sha256').update(readFileSync(join(repository, file))).digest('hex')]))).digest('hex');
}

export function configuration(repository) {
  const deny = parse(readFileSync(join(repository, 'deny.toml'), 'utf8'));
  const accepted = deny.licenses?.allow;
  const targets = JSON.parse(readFileSync(join(repository, '.github/release-runner-policy.json'), 'utf8'))
    .targets.map(entry => required(entry.target, 'release target'));
  if (!Array.isArray(accepted) || !accepted.length ||
      accepted.some(value => typeof value !== 'string' || !value.trim()) ||
      new Set(accepted).size !== accepted.length || !targets.length || new Set(targets).size !== targets.length) {
    throw new Error('Empty or duplicate release license policy/target coverage');
  }
  // Preserve the owning deny policy's choices; cargo-about resolves alternatives.
  return `accepted = ${JSON.stringify(accepted)}\ntargets = ${JSON.stringify(targets)}\n` +
    'ignore-dev-dependencies = true\nignore-build-dependencies = false\n' +
    'private = { ignore = true }\nworkarounds = ["bitvec", "chrono", "ring", "rustix", "rustls"]\n';
}

export function render(about) {
  if (!Array.isArray(about.crates) || !about.crates.length ||
      !Array.isArray(about.licenses) || !about.licenses.length) {
    throw new Error('Empty dependency or license inventory');
  }
  const crates = new Map();
  for (const entry of about.crates) {
    const pkg = entry.package;
    required(pkg?.name, 'crate name'); required(pkg?.version, 'crate version');
    required(entry.license, `license expression for ${label(pkg)}`);
    if (crates.has(label(pkg))) throw new Error(`Duplicate crate ${label(pkg)}`);
    crates.set(label(pkg), pkg);
  }
  const covered = new Set();
  const sections = [];
  for (const license of about.licenses) {
    required(license.id, 'license identifier'); required(license.text, `full ${license.id} license text`);
    if (!Array.isArray(license.used_by) || !license.used_by.length) throw new Error('License has no users');
    const users = license.used_by.map(use => {
      const name = label(use.crate);
      if (!crates.has(name)) throw new Error(`License names unknown crate ${name}`);
      covered.add(name);
      return name;
    }).sort();
    sections.push(`${license.id}\nUsed by: ${users.join(', ')}\n\n${license.text.trim()}\n`);
  }
  const missing = [...crates.keys()].filter(name => !covered.has(name));
  if (missing.length) throw new Error(`Crates without full license text: ${missing.join(', ')}`);
  const lines = ['Harn CLI third-party notices', '',
    'Generated from the locked default-feature CLI dependency graph for every release target.',
    'Includes build dependencies as a conservative superset; excludes development dependencies.', '',
    'Dependency sources and notices', ''];
  for (const [name, pkg] of [...crates].sort(([a], [b]) => a.localeCompare(b))) {
    const source = pkg.source;
    if (source != null && !source.startsWith('registry+https://github.com/rust-lang/crates.io-index')) {
      throw new Error(`Unsupported dependency source for ${name}: ${source}`);
    }
    if (!Array.isArray(pkg.notices)) throw new Error(`Missing NOTICE inventory for ${name}`);
    // Workspace components may contain modified third-party source. Their
    // actual notices survive even though they have no registry source URL.
    if (source == null && !pkg.notices.length) continue;
    lines.push(name, source == null ? 'Source: packaged workspace component' :
      `Source: https://crates.io/api/v1/crates/${pkg.name}/${pkg.version}/download`);
    for (const notice of pkg.notices) {
      lines.push(`${required(notice.name, `NOTICE name for ${name}`)}:`, required(notice.text, `NOTICE for ${name}`));
    }
    lines.push('');
  }
  const mpl = about.licenses.filter(entry => entry.id === 'MPL-2.0').flatMap(entry => entry.used_by.map(use => label(use.crate)));
  if (mpl.length) lines.push('MPL-2.0 source availability', '',
    'The following components are used without modification. Their complete source archives',
    'are available at the exact-version source links above. MPL-2.0 license text follows.',
    ...[...new Set(mpl)].sort(), '');
  lines.push('Full license texts', '', ...sections.sort());
  return lines.join('\n') + '\n';
}

export function generate(repository, output, executable) {
  const scratch = mkdtempSync(join(tmpdir(), 'harn-release-notices-'));
  try {
    const config = join(scratch, 'about.toml');
    const inventory = join(scratch, 'inventory.json');
    writeFileSync(config, configuration(repository));
    const result = spawnSync(executable, ['generate', '--locked', '--fail', '--format', 'json',
      '--config', config, '--manifest-path', join(repository, 'crates/harn-cli/Cargo.toml'),
      '--output-file', inventory], { cwd: repository, stdio: 'inherit', timeout: 900000 });
    if (result.error || result.status !== 0) throw new Error(`cargo-about failed: ${result.error ?? result.status}`);
    const about = normalizeInventory(JSON.parse(readFileSync(inventory, 'utf8')));
    const text = render(about);
    mkdirSync(output, { recursive: true });
    for (const file of ['LICENSE-MIT', 'LICENSE-APACHE']) {
      writeFileSync(join(output, file), required(readFileSync(join(repository, file), 'utf8'), file));
    }
    writeFileSync(join(output, 'THIRD-PARTY-NOTICES.txt'), text);
    writeFileSync(join(output, 'release-license-inventory.json'), JSON.stringify({ schemaVersion: 1,
      sourceFingerprint: sourceFingerprint(repository), about }) + '\n');
    console.log(`Release notices: ${about.crates.length} crates, ${about.licenses.length} full license texts, 0 uncovered crates; ${text.length} characters`);
  } finally { rmSync(scratch, { recursive: true, force: true }); }
}

export function verify(repository, output) {
  const inventory = JSON.parse(readFileSync(join(output, 'release-license-inventory.json'), 'utf8'));
  if (inventory.schemaVersion !== 1 || inventory.sourceFingerprint !== sourceFingerprint(repository)) {
    throw new Error('Release license inventory differs from current source');
  }
  // The producer validated the collected graph once; this check re-renders its
  // complete retained material without fetching packages or trusting host paths.
  const text = render(inventory.about);
  for (const file of ['LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt']) {
    const expected = file === 'THIRD-PARTY-NOTICES.txt' ? text : required(readFileSync(join(repository, file), 'utf8'), file);
    if (readFileSync(join(output, file), 'utf8') !== expected) throw new Error(`Release license material differs: ${file}`);
  }
  console.log(`Verified retained release inventory: ${inventory.about.crates.length} crates, 0 uncovered; current source and exact output bytes`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [command, output] = process.argv.slice(2);
  if (!['generate', 'verify'].includes(command) || !output) throw new Error('Usage: release_third_party_notices.mjs generate|verify <output-directory>');
  if (command === 'generate') generate(root, resolve(output), process.env.CARGO_ABOUT ?? 'cargo-about');
  else verify(root, resolve(output));
}
