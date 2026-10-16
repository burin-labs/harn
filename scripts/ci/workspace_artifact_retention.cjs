const fs = require('node:fs');
const path = require('node:path');

function requireValue(condition, message) {
  if (!condition) throw new Error(`workspace artifact retention: ${message}`);
}

function inside(root, value) {
  requireValue(typeof value === 'string' && path.isAbsolute(value), 'artifact path is not absolute');
  const normalized = path.join(fs.realpathSync(path.dirname(value)), path.basename(value));
  const relative = path.relative(root, normalized);
  requireValue(relative !== '' && relative !== '..' && !relative.startsWith(`..${path.sep}`)
    && !path.isAbsolute(relative), `artifact escapes target: ${value}`);
  return relative.split(path.sep);
}

function directory(value) {
  const stat = fs.lstatSync(value);
  requireValue(stat.isDirectory() && !stat.isSymbolicLink(), `not a regular build directory: ${value}`);
}

function escape(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function targetKey(id, target) {
  return JSON.stringify([id, target.name, target.kind]);
}

function context(metadata, profile = 'debug') {
  requireValue(Array.isArray(metadata.workspace_members) && metadata.workspace_members.length > 0,
    'no workspace members measured');
  requireValue(Array.isArray(metadata.packages), 'no package metadata');
  const packages = new Map(metadata.packages.map(p => [p.id, p]));
  const members = new Map(metadata.workspace_members.map(id => {
    const pkg = packages.get(id);
    requireValue(pkg && typeof pkg.name === 'string' && Array.isArray(pkg.targets), `missing member ${id}`);
    return [id, pkg];
  }));
  const root = fs.realpathSync(metadata.target_directory);
  const workspace = fs.realpathSync(metadata.workspace_root);
  inside(workspace, root);
  for (const name of [profile, `${profile}/deps`, `${profile}/build`]) directory(path.join(root, name));
  const features = new Map((metadata.resolve?.nodes || []).map(n => [n.id, new Set(n.features)]));
  const required = new Set();
  for (const [id, pkg] of members) {
    for (const target of pkg.targets) {
      if (target.test && target.kind.some(k => ['lib', 'rlib', 'proc-macro', 'bin', 'test'].includes(k))
        && (target['required-features'] || []).every(f => features.get(id)?.has(f))) {
        required.add(targetKey(id, target));
      }
    }
  }
  return {root, profile, members, required};
}

function qualifyGraph(ctx, text, name, role) {
  requireValue(typeof text === 'string' && text.trim() !== '', `${name} has no Cargo records`);
  const records = text.split(/\r?\n/).filter(line => line.startsWith('{')).map(line => JSON.parse(line));
  const terminal = records.filter(r => r.reason === 'build-finished');
  requireValue(terminal.length === 1 && terminal[0].success === true
    && records.at(-1) === terminal[0], `${name} has no successful terminal build`);
  const keep = new Set();
  const files = new Set();
  const native = new Set();
  let nativeUnits = 0;
  let nativeLibraries = 0;
  const cli = new Map([['harn-cli','harn'],['harn-cli-aot-gen','harn-cli-aot-gen']]);
  const cliExecutables = new Set();
  const dependencyFiles = new Set();
  const listed = new Set();
  let count = 0;
  function addRecord(record) {
    const pkg = ctx.members.get(record.package_id);
    if (!pkg) {
      // A dependency can use the same target name as a workspace target.
      // Cargo's current output paths take precedence over filename ownership.
      if (record.reason === 'compiler-artifact') {
        requireValue(Array.isArray(record.filenames) && record.filenames.length > 0,
          `${name} has no dependency artifact filenames`);
        for (const file of record.filenames) {
          const parts = inside(ctx.root, file);
          requireValue(fs.lstatSync(file).isFile(), `${name} dependency artifact is not a regular file: ${file}`);
          const canonical = path.join(ctx.root,...parts);
          files.add(canonical);
          dependencyFiles.add(canonical);
          if (parts[0] === ctx.profile && parts[1] === 'build' && parts.length === 4) keep.add(parts[2]);
        }
      } else if (record.reason === 'build-script-executed') {
        const parts = inside(ctx.root, record.out_dir);
        requireValue(parts.length === 4 && parts[0] === ctx.profile && parts[1] === 'build' && parts[3] === 'out',
          `${name} has an unsupported dependency build output`);
        directory(record.out_dir);
        keep.add(parts[2]);
      }
      return;
    }
    if (record.reason === 'build-script-executed') {
      const parts = inside(ctx.root, record.out_dir);
      requireValue(parts.length === 4 && parts[0] === ctx.profile && parts[1] === 'build'
        && parts[3] === 'out' && new RegExp(`^${escape(pkg.name)}-[a-f0-9]{16}$`).test(parts[2]),
      `${name} has an unsupported build output: ${record.out_dir}`);
      directory(record.out_dir);
      keep.add(parts[2]);
      return;
    }
    if (record.reason !== 'compiler-artifact') return;
    count += 1;
    requireValue(record.manifest_path === pkg.manifest_path, `${name} has a foreign package manifest`);
    const target = pkg.targets.find(t => t.name === record.target?.name
      && JSON.stringify(t.kind) === JSON.stringify(record.target.kind));
    requireValue(target && typeof record.profile?.test === 'boolean'
      && typeof record.fresh === 'boolean', `${name} has an unknown compiler target`);
    requireValue(Array.isArray(record.filenames) && record.filenames.length > 0,
      `${name} has no artifact filenames`);
    if (record.profile.test) listed.add(targetKey(pkg.id, target));
    if (pkg.name === 'harn-vm' && target.name === 'harn_vm' && target.kind.includes('lib')
      && !record.profile.test && record.filenames.some(file => file.endsWith('.rlib'))) nativeLibraries += 1;
    if (target.kind.includes('bin') && !record.profile.test) {
      requireValue(record.filenames.includes(record.executable), `${name} named executable is not bound to its artifact`);
      inside(ctx.root, record.executable);
      const identity = fs.lstatSync(record.executable,{bigint:true});
      requireValue(identity.isFile() && (identity.mode & 0o111n) !== 0n, `${name} named artifact is not executable`);
      // Cargo links the named binary to its hashed dependency output. Keep
      // that exact inode and its dep-info, so a warm build stays fully fresh.
      const pattern = new RegExp(`^${escape(target.name.replace(/-/g, '_'))}-([a-f0-9]{16})$`);
      let matches = 0;
      for (const entry of fs.readdirSync(path.join(ctx.root,ctx.profile,'deps'))) {
        const match = pattern.exec(entry);
        if (!match) continue;
        const stat = fs.lstatSync(path.join(ctx.root,ctx.profile,'deps',entry),{bigint:true});
        if (stat.isFile() && stat.dev === identity.dev && stat.ino === identity.ino) {
          keep.add(`${pkg.name}-${match[1]}`);
          matches += 1;
        }
      }
      requireValue(matches > 0, `${name} has no matching hashed Cargo executable for ${target.name}`);
      if (role === 'cli' && cli.get(pkg.name) === target.name) {
        cliExecutables.add(record.executable);
        cli.delete(pkg.name);
      }
    }
    for (const file of record.filenames) {
      const parts = inside(ctx.root, file);
      requireValue(fs.lstatSync(file).isFile(), `${name} artifact is not a regular file: ${file}`);
      requireValue(parts[0] === ctx.profile, `${name} used an unwarmed Cargo profile`);
      const canonical = path.join(ctx.root, ...parts);
      files.add(canonical);
      if (parts[1] === 'deps') {
        const match = new RegExp(`^(?:lib)?${escape(target.name.replace(/-/g, '_'))}-([a-f0-9]{16})(?:\\..+)?$`)
          .exec(parts[2]);
        // Mixed cdylib/rlib targets also emit unhashed library outputs.
        // Exact current filenames are protected above; only hashed outputs
        // identify build directories or obsolete dependency generations.
        const stem = target.name.replace(/-/g, '_');
        const unhashed = (target.crate_types || []).some(kind =>
          (kind === 'rlib' && parts[2] === `lib${stem}.rlib`)
          || (['cdylib', 'dylib'].includes(kind)
            && [`lib${stem}.so`, `lib${stem}.dylib`, `${stem}.dll`].includes(parts[2])));
        requireValue(parts.length === 3 && (match || unhashed), `${name} has an unsupported dependency artifact: ${file}`);
        if (match) keep.add(`${pkg.name}-${match[1]}`);
        if (pkg.name === 'harn-vm' && target.name === 'harn_vm' && record.profile.test) {
          requireValue(record.executable === file, `${name} Native executable is not bound to its artifact`);
          requireValue((fs.lstatSync(file).mode & 0o111) !== 0, `${name} Native artifact is not executable: ${file}`);
          native.add(canonical);
          if (target.kind.includes('lib')) nativeUnits += 1;
        }
      } else if (parts[1] === 'build') {
        requireValue(parts.length === 4 && new RegExp(`^${escape(pkg.name)}-[a-f0-9]{16}$`).test(parts[2]),
          `${name} has an unsupported build-script artifact: ${file}`);
        keep.add(parts[2]);
      } else if (parts[1] === 'examples') {
        requireValue(parts.length === 3 && target.kind.includes('example'),
          `${name} has an unsupported example artifact: ${file}`);
      } else {
        requireValue(parts.length === 2, `${name} has an unsupported top-level artifact: ${file}`);
      }
    }
  }
  const failures = [];
  for (const record of records) {
    try { addRecord(record); }
    catch (error) { failures.push(error.message.replace(/^workspace artifact retention: /,'')); }
  }
  if (failures.length > 0) {
    throw Object.assign(new Error(`workspace artifact retention: ${name} invalid Cargo artifacts=${failures.length}: ${failures.join('; ')}`),{failures});
  }
  const missing = role === 'workspace' ? [...ctx.required].filter(id => !listed.has(id)) : [];
  requireValue(missing.length === 0, `${name} missing ${missing.length} test targets: ${missing.join(', ')}`);
  if (role === 'cli') {
    requireValue(count > 0 && cli.size === 0 && nativeLibraries > 0, `${name} missing CLI targets: ${[...cli.keys()].join(', ')}; Native libraries=${nativeLibraries}`);
  } else {
    requireValue(count > 0 && nativeUnits > 0, `${name} did not measure current Native executables`);
  }
  return {keep, files, native, nativeUnits, cliExecutables, dependencyFiles, count, role};
}

function measuredSize(value) {
  const stat = fs.lstatSync(value);
  requireValue(!stat.isSymbolicLink(), `refusing to traverse artifact symlink: ${value}`);
  if (stat.isFile()) return stat.size;
  requireValue(stat.isDirectory(), `unsupported artifact type: ${value}`);
  return fs.readdirSync(value).reduce((sum, child) => sum + measuredSize(path.join(value, child)), 0);
}

function planRetention(metadata, workspaceText, securityText) {
  const ctx = context(metadata);
  return planGraphs(ctx,[qualifyGraph(ctx, workspaceText, 'workspace-tests', 'workspace'),
    qualifyGraph(ctx, securityText, 'security-tests', 'security')]);
}

function planCliRetention(metadata, text) {
  const ctx = context(metadata,'ci-cli');
  return planGraphs(ctx,[qualifyGraph(ctx,text,'shared-cli','cli')]);
}

function planGraphs(ctx, graphs) {
  const keep = new Set(graphs.flatMap(g => [...g.keep]));
  const native = new Set(graphs.flatMap(g => [...g.native]));
  const files = new Set(graphs.flatMap(g => [...g.files]));
  const targets = new Map();
  for (const pkg of ctx.members.values()) {
    for (const target of pkg.targets) {
      const stem = target.name.replace(/-/g, '_');
      for (const prefix of [stem, `lib${stem}`]) {
        const owners = targets.get(prefix) || new Set();
        owners.add(pkg.name);
        targets.set(prefix, owners);
      }
    }
  }
  const removed = [];
  for (const name of fs.readdirSync(path.join(ctx.root,ctx.profile,'deps'))) {
    const match = /^(.*)-([a-f0-9]{16})(?:\..+)?$/.exec(name);
    const owners = match && targets.get(match[1]);
    if (!owners || [...owners].some(owner => keep.has(`${owner}-${match[2]}`))) continue;
    const file = path.join(ctx.root,ctx.profile,'deps',name);
    if (files.has(file)) continue;
    removed.push({path:file, bytes:measuredSize(file)});
  }
  for (const name of fs.readdirSync(path.join(ctx.root,ctx.profile,'build'))) {
    if (keep.has(name) || ![...ctx.members.values()].some(p =>
      new RegExp(`^${escape(p.name)}-[a-f0-9]{16}$`).test(name))) continue;
    const file = path.join(ctx.root,ctx.profile,'build',name);
    removed.push({path:file, bytes:measuredSize(file)});
  }
  requireValue(!removed.some(r => [...files].some(file => file === r.path || file.startsWith(`${r.path}${path.sep}`))),
    'plan removes a current compiler artifact');
  return {schema:'harn.workspace_artifact_retention.v1', ok:true, target:ctx.root, profile:ctx.profile,
    workspace_artifacts:graphs.find(g => g.role === 'workspace')?.count || 0,
    security_artifacts:graphs.find(g => g.role === 'security')?.count || 0,
    cli_artifacts:graphs.find(g => g.role === 'cli')?.count || 0,
    workspace_native_unit_artifacts:graphs.find(g => g.role === 'workspace')?.nativeUnits || 0,
    security_native_unit_artifacts:graphs.find(g => g.role === 'security')?.nativeUnits || 0,
    protected_dependency_files:new Set(graphs.flatMap(g => [...g.dependencyFiles])).size,
    current_files:files.size, retained_native_executables:[...native].sort(),
    retained_cli_executables:graphs.flatMap(g => [...g.cliExecutables]).sort(),
    pending:0, bad:0, apply_started:false, applied:false, deleted_count:0,
    removed, removed_bytes:removed.reduce((sum, entry) => sum + entry.bytes, 0)};
}

function applyRetention(plan) {
  // Validate the entire deletion set before the first mutation. The producer
  // owns this private target and has finished both compiler phases.
  for (const entry of plan.removed) {
    const parts = inside(plan.target, entry.path);
    requireValue(['debug','ci-cli'].includes(plan.profile) && parts.length === 3 && parts[0] === plan.profile
      && ['deps','build'].includes(parts[1]), 'plan has an unsupported deletion path');
    requireValue(measuredSize(entry.path) === entry.bytes, `artifact changed after census: ${entry.path}`);
  }
  plan.apply_started = true;
  for (const entry of plan.removed) {
    fs.rmSync(entry.path, {recursive:true});
    plan.deleted_count += 1;
  }
  for (const file of [...plan.retained_native_executables,...plan.retained_cli_executables]) requireValue(fs.lstatSync(file).isFile(), `lost executable ${file}`);
  return {...plan, applied:true};
}

if (require.main === module) {
  const args = process.argv.slice(2);
  let plan;
  try {
    requireValue(args.length === 4 || args.length === 5 && args[4] === '--apply',
      'usage: metadata.json workspace.jsonl security.jsonl report.json [--apply]; or --cli metadata.json cli.jsonl report.json [--apply]');
    const metadata = JSON.parse(fs.readFileSync(args[args[0] === '--cli' ? 1 : 0], 'utf8'));
    plan = args[0] === '--cli' ? planCliRetention(metadata,fs.readFileSync(args[2],'utf8'))
      : planRetention(metadata,fs.readFileSync(args[1],'utf8'),fs.readFileSync(args[2],'utf8'));
    if (args[4] === '--apply') plan = applyRetention(plan);
    fs.writeFileSync(args[3], `${JSON.stringify(plan, null, 2)}\n`);
    console.log(JSON.stringify({...plan, removed:plan.removed.length}));
  } catch (error) {
    if (args[3]) fs.writeFileSync(args[3], `${JSON.stringify({schema:'harn.workspace_artifact_retention.v1',
      ok:false,pending:0,bad:error.failures?.length || 1,failures:error.failures || [error.message],apply_started:plan?.apply_started || false,
      deleted_count:plan?.deleted_count || 0})}\n`);
    console.error(error.message);
    process.exitCode = 1;
  }
}

module.exports = {planRetention, planCliRetention, applyRetention};
