const fs = require('node:fs');
const path = require('node:path');

// Read only hash markers and file sizes. Fingerprint JSON can contain watched
// environment values: its contents must never be opened by this collector.
function snapshot(target) {
  const root = path.resolve(target);
  const units = [];
  const files = [];
  const fingerprints = path.join(root, 'debug/.fingerprint');
  if (fs.existsSync(fingerprints)) {
    for (const directory of fs.readdirSync(fingerprints, {withFileTypes:true})) {
      if (!directory.isDirectory() || !/^[\w-]+-[a-f0-9]{16}$/.test(directory.name)) continue;
      const folder = path.join(fingerprints, directory.name);
      for (const file of fs.readdirSync(folder, {withFileTypes:true})) {
        if (!file.isFile() || !/^(?:test-)?(?:lib|bin|example|integration-test)-[\w-]+\.json$/.test(file.name)) continue;
        const marker = path.join(folder, file.name.slice(0, -5));
        let fingerprint = null;
        if (fs.existsSync(marker)) {
          const stat = fs.lstatSync(marker);
          if (!stat.isFile() || stat.size > 64) throw new Error('invalid fingerprint marker');
          fingerprint = fs.readFileSync(marker, 'utf8').trim();
          if (!/^[a-f0-9]{16}$/.test(fingerprint)) throw new Error('invalid fingerprint hash');
        }
        units.push({id:`${directory.name}/${file.name.slice(0, -5)}`, fingerprint});
      }
    }
  }
  const deps = path.join(root, 'debug/deps');
  if (fs.existsSync(deps)) {
    for (const file of fs.readdirSync(deps, {withFileTypes:true})) {
      if (!file.isFile() || !/^[\w.-]+$/.test(file.name)) continue;
      files.push({name:file.name, bytes:fs.statSync(path.join(deps, file.name)).size});
    }
  }
  units.sort((a,b) => a.id.localeCompare(b.id));
  files.sort((a,b) => a.name.localeCompare(b.name));
  return {schema:'harn.workspace_build_snapshot.v1', target:root, target_present:fs.existsSync(root), units, files};
}

function report(before, after, inventory) {
  if (before.schema !== 'harn.workspace_build_snapshot.v1' || before.target !== after.target
      || inventory['rust-build-meta']?.['target-directory'] !== after.target) {
    throw new Error('build evidence target does not match the nextest inventory');
  }
  const previousUnits = new Map(before.units.map(u => [u.id,u]));
  const previousFiles = new Map(before.files.map(f => [f.name,f.bytes]));
  const files = new Map(after.files.map(f => [f.name,f.bytes]));
  const binaries = Object.values(inventory['rust-suites']).filter(suite => suite.status === 'listed').map(suite => {
    const file = suite['binary-path'];
    if (typeof file !== 'string' || path.dirname(file) !== path.join(after.target,'debug/deps')) {
      throw new Error('test binary is outside the measured dependency directory');
    }
    const name = path.basename(file);
    if (!files.has(name)) throw new Error('test binary was not measured');
    return {name, bytes:files.get(name), present_before:previousFiles.has(name)};
  });
  if (binaries.length === 0) throw new Error('no test binaries measured');
  return {schema:'harn.workspace_build_evidence.v1', complete:true, before, after,
    unit_changes:after.units.flatMap(unit => {
      const previous = previousUnits.get(unit.id);
      if (!previous) return [{id:unit.id, observation:'new_fingerprint_key'}];
      return previous.fingerprint !== unit.fingerprint
        ? [{id:unit.id, observation:'changed_fingerprint_hash'}] : [];
    }), test_binaries:binaries,
    limits:['File sizes are logical bytes, not compressed cache size.',
      'Fingerprint changes and missing binaries are observations, not complete Cargo dirty-reason attribution.']};
}

if (require.main === module) {
  const [mode,target,output,beforeFile,inventoryFile] = process.argv.slice(2);
  try {
    if (!['snapshot','report'].includes(mode) || !target || !output
        || (mode === 'report' && (!beforeFile || !inventoryFile))) throw new Error('invalid build evidence arguments');
    const current = snapshot(target);
    const value = mode === 'snapshot' ? current : report(
      JSON.parse(fs.readFileSync(beforeFile,'utf8')), current,
      JSON.parse(fs.readFileSync(inventoryFile,'utf8')));
    fs.writeFileSync(output,JSON.stringify(value)+'\n');
  } catch (error) {
    console.error(`workspace build evidence: ${error.message}`);
    process.exitCode = 1;
  }
}
module.exports = {snapshot,report};
