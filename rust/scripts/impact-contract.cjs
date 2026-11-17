// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

// Inspection only: use the existing planner/executor for actual validation.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const {execFileSync} = require('node:child_process');
const {plan, commands, dependencyInventory} = require('./impact-plan.cjs');
const {sourceInventory} = require('./validate-impact.cjs');
const PLATFORMS = ['linux', 'macos', 'windows'];
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const sorted = values => [...new Set(values)].sort();
const digest = value => crypto.createHash('sha256').update(JSON.stringify(value)).digest('hex');
const parse = value => JSON.parse(String(value).replace(/^\uFEFF/, ''));

function discoverManifests(root) {
  const result = [];
  function visit(directory) {
    for (const entry of fs.readdirSync(directory, {withFileTypes: true})) {
      assert(!entry.isSymbolicLink(), 'Package source roots must not contain symlinks');
      const file = path.join(directory, entry.name);
      if (entry.isDirectory()) visit(file);
      else if (entry.name === 'Cargo.toml') result.push(path.relative(root, file).split(path.sep).join('/'));
    }
  }
  for (const name of ['engine', 'clients']) visit(path.join(root, 'rust', name));
  return result.sort();
}

function selectionInventory(selection) {
  const targets = tests => sorted(tests.flatMap(test => selection.target_catalog
    .filter(target => target.package === test.package &&
      (test.target === 'all' || test.target === target.target))
    .map(target => `${target.package}/${target.target}`)));
  const defaults = targets(selection.tests.filter(test => test.default_available))
    .filter(key => selection.target_catalog.some(target =>
      `${target.package}/${target.target}` === key && target.default_available));
  const profiles = selection.profiles.map(profile => ({id: profile.id,
    platforms: profile.platforms, features: profile.features, test_features: profile.test_features,
    targets: targets(profile.tests).filter(key => selection.target_catalog.some(target =>
      `${target.package}/${target.target}` === key && target.profiles.includes(profile.id)))}));
  return {
    mode: selection.mode, packages: selection.packages, components: selection.components,
    checks: selection.checks, default_targets: defaults, profiles,
    platforms: Object.fromEntries(PLATFORMS.map(platform => {
      const executable = commands(selection, platform);
      const compositions = profiles.filter(profile => profile.platforms.includes(platform));
      return [platform, {
        default_targets: defaults.length,
        profile_targets: compositions.reduce((count, profile) => count + profile.targets.length, 0),
        profiles: compositions.length, checks: executable.checks.length,
        skipped_obligations: executable.skipped.length,
      }];
    })),
    counts: {packages: selection.packages.length, test_selectors: selection.tests.length,
      default_targets: defaults.length, profiles: profiles.length, checks: selection.checks.length},
  };
}

function foundationContract(inventory, foundations, policy, sourceCommit) {
  const packages = new Map(inventory.packages.map(pkg => [pkg.name, pkg]));
  const enrolled = new Set();
  if (policy !== null) {
    assert(policy && policy.schema === 1 && Object.keys(policy).every(key =>
      ['schema', 'source_commit', 'packages'].includes(key)), 'Invalid foundation policy schema');
    assert(foundations.length && /^[a-f0-9]{40}$/.test(sourceCommit) &&
      policy.source_commit === sourceCommit, 'Foundation policy requires assertions at its exact source commit');
    assert(Array.isArray(policy.packages) && policy.packages.length, 'Foundation policy requires exact package roles');
    for (const entry of policy.packages) {
      assert(entry && Object.keys(entry).every(key => ['name', 'manifest', 'layer', 'role'].includes(key)) &&
        entry.role === 'neutral-foundation', 'Invalid neutral foundation role');
      const pkg = packages.get(entry.name);
      assert(pkg && pkg.manifest === entry.manifest && pkg.layer === entry.layer &&
        ['core', 'sdk'].includes(entry.layer), `Foundation policy identity mismatch: ${entry.name}`);
      assert(!enrolled.has(entry.name), `Duplicate foundation policy package: ${entry.name}`);
      enrolled.add(entry.name);
    }
  }
  const reachable = new Set();
  for (const name of foundations) {
    const pkg = packages.get(name);
    assert(pkg, `Unknown foundation package: ${name}`);
    for (const id of [name, ...pkg.forward_dependencies]) reachable.add(id);
    const forbidden = [name, ...pkg.forward_dependencies].filter(id =>
      packages.get(id).layer !== 'core' && !enrolled.has(id));
    assert(!forbidden.length, `Foundation ${name} requires non-core packages: ${forbidden.join(', ')}`);
  }
  assert([...enrolled].every(name => reachable.has(name)), 'Foundation policy contains unused package roles');
  return policy === null ? null : {digest: digest(policy), ...policy};
}

/** Produce reproducible selection counts, never execution or performance evidence. */
function contractReport({metadata, graph, root, files, manifests, foundations = [],
  foundationPolicy = null, sourceCommit = null}) {
  const inventory = dependencyInventory({metadata, graph, root, files, manifests});
  const foundationPolicyRecord = foundationContract(inventory, foundations, foundationPolicy, sourceCommit);
  const select = files => selectionInventory(plan({metadata, graph, root, files}));
  const scenarios = [
    ...inventory.packages.map(pkg => {
      assert(pkg.probe, `Package has no supported Cargo source probe: ${pkg.name}`);
      return {id: `package:${pkg.name}`, files: [pkg.probe], selection: select([pkg.probe])};
    }),
    ...inventory.components.filter(component => component.files.length).map(component =>
      ({id: `component:${component.id}`, files: component.files, selection: select(component.files)})),
  ];
  inventory.counts = {
    packages: inventory.packages.length,
    default_members: inventory.packages.filter(pkg => pkg.default_member).length,
    dependency_declarations: inventory.packages.reduce((count, pkg) => count + pkg.dependencies.length, 0),
    internal_declarations: inventory.packages.reduce((count, pkg) =>
      count + pkg.dependencies.filter(dep => dep.internal).length, 0),
    internal_edges: inventory.packages.reduce((count, pkg) =>
      count + new Set(pkg.dependencies.filter(dep => dep.internal).map(dep => dep.package)).size, 0),
    cargo_targets: inventory.packages.reduce((count, pkg) => count + pkg.cargo_targets, 0),
  };
  return {
    schema: 1, kind: 'capability-selection-contract',
    evidence_scope: 'metadata-and-hypothetical-source-selections',
    build_executed: false, latency_claim: false, native_qualification: false,
    asserted_foundations: sorted(foundations),
    foundation_policy: foundationPolicyRecord,
    inventory, full: selectionInventory(plan({metadata, graph, root, files: [], full: true})),
    scenarios,
  };
}

function compareReports(baseline, current) {
  for (const report of [baseline, current]) assert(report?.schema === 1 &&
    report.kind === 'capability-selection-contract' && Array.isArray(report.scenarios),
  'Expected a schema-1 capability selection contract baseline');
  const before = new Map(baseline.scenarios.map(scenario => [scenario.id, scenario]));
  const after = new Map(current.scenarios.map(scenario => [scenario.id, scenario]));
  const membership = (previous = [], next = []) => ({
    added: next.filter(value => !previous.includes(value)),
    removed: previous.filter(value => !next.includes(value)),
  });
  const deltas = (previous, next) => previous && next ?
    Object.fromEntries(Object.keys(previous).map(key => [key, next[key] - previous[key]])) : null;
  return {
    baseline_head: baseline.source?.head ?? null, current_head: current.source?.head ?? null,
    graph: {before: baseline.inventory.counts, after: current.inventory.counts},
    scenarios: sorted([...before.keys(), ...after.keys()]).map(id => {
      const previous = before.get(id), next = after.get(id);
      const oldProfiles = new Map(previous?.selection.profiles.map(profile => [profile.id, profile]) || []);
      const newProfiles = new Map(next?.selection.profiles.map(profile => [profile.id, profile]) || []);
      return {id, status: !previous ? 'added' : !next ? 'removed' : 'matched',
        comparable_inputs: !!previous && !!next && JSON.stringify(previous.files) === JSON.stringify(next.files),
        before: previous?.selection.counts ?? null, after: next?.selection.counts ?? null,
        delta: deltas(previous?.selection.counts, next?.selection.counts),
        platforms: Object.fromEntries(PLATFORMS.map(platform => {
          const oldCounts = previous?.selection.platforms[platform] ?? null;
          const newCounts = next?.selection.platforms[platform] ?? null;
          return [platform, {before: oldCounts, after: newCounts, delta: deltas(oldCounts, newCounts)}];
        })),
        membership: Object.fromEntries(['packages', 'components', 'checks', 'default_targets'].map(key =>
          [key, membership(previous?.selection[key], next?.selection[key])])),
        profile_changes: sorted([...oldProfiles.keys(), ...newProfiles.keys()]).filter(id =>
          JSON.stringify(oldProfiles.get(id)) !== JSON.stringify(newProfiles.get(id))).map(id =>
          ({id, before: oldProfiles.get(id) ?? null, after: newProfiles.get(id) ?? null})),
      };
    }),
  };
}

function options(argv) {
  const result = {foundations: []};
  for (let i = 0; i < argv.length; i++) {
    const key = argv[i].slice(2);
    assert(argv[i].startsWith('--') &&
      ['root', 'output', 'baseline', 'cargo', 'assert-foundation', 'foundation-policy'].includes(key),
    `Unknown option: ${argv[i]}`);
    assert(i + 1 < argv.length && !argv[i + 1].startsWith('--'), `Missing value: ${argv[i]}`);
    const value = argv[++i];
    if (key === 'assert-foundation') {
      assert(!result.foundations.includes(value), `Duplicate foundation: ${value}`);
      result.foundations.push(value);
    } else {
      assert(!Object.hasOwn(result, key), `Duplicate option: --${key}`);
      assert(path.isAbsolute(value), `--${key} must be absolute`);
      result[key] = value;
    }
  }
  assert(result.output, '--output is required');
  return result;
}

function main(argv = process.argv.slice(2), hooks = {}) {
  const opts = options(argv);
  const root = fs.realpathSync(opts.root || path.resolve(__dirname, '../..'));
  let ancestor = path.resolve(opts.output);
  const suffix = [];
  while (!fs.existsSync(ancestor)) {
    suffix.unshift(path.basename(ancestor));
    const parent = path.dirname(ancestor);
    assert(parent !== ancestor, 'Artifact path has no existing ancestor');
    ancestor = parent;
  }
  const output = path.join(fs.realpathSync(ancestor), ...suffix);
  // Keep the new report out of the source inventory it describes.
  const relative = path.relative(root, output);
  assert(relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative),
    'Contract artifacts must be outside the checkout');
  const exec = hooks.exec || ((program, args, cwd) => execFileSync(program, args, {
    cwd, encoding: 'utf8', windowsHide: true, timeout: 120000, maxBuffer: 64 * 1024 * 1024,
  }));
  const git = args => String(exec('git', args, root)).trim();
  const read = hooks.read || (file => fs.readFileSync(file, 'utf8'));
  const before = (hooks.sourceInventory || sourceInventory)(root);
  const head = git(['rev-parse', '--verify', 'HEAD']);
  const tree = git(['rev-parse', 'HEAD^{tree}']);
  const dirty = git(['status', '--porcelain=v1', '--untracked-files=all']).length > 0;
  const cargo = opts.cargo || 'cargo';
  const metadataArgs = ['metadata', '--offline', '--locked', '--no-deps', '--format-version', '1'];
  const metadata = parse(exec(cargo, metadataArgs, path.join(root, 'rust')));
  const graph = parse(read(path.join(root, 'rust/validation/impact-graph.json')));
  const foundationPolicy = opts['foundation-policy'] ? parse(read(opts['foundation-policy'])) : null;
  assert(!foundationPolicy || !dirty, 'Source-bound foundation policies require a clean checkout');
  const report = contractReport({root, metadata, graph, files: before.files.map(file => file.path),
    manifests: (hooks.discoverManifests || discoverManifests)(root), foundations: opts.foundations,
    foundationPolicy, sourceCommit: head});
  report.source = {head, tree, dirty, inventory_digest: before.digest};
  report.inputs = {metadata_command: {program: cargo, args: metadataArgs},
    cargo_version: String(exec(cargo, ['--version'], path.join(root, 'rust'))).trim(),
    metadata_digest: digest(metadata), graph_digest: digest(graph),
    foundation_policy_digest: foundationPolicy === null ? null : digest(foundationPolicy),
    tool_digest: digest(fs.readFileSync(__filename, 'utf8')),
    planner_digest: digest(fs.readFileSync(path.join(__dirname, 'impact-plan.cjs'), 'utf8'))};
  if (opts.baseline) report.comparison = compareReports(parse(read(opts.baseline)), report);
  const after = (hooks.sourceInventory || sourceInventory)(root);
  assert(before.digest === after.digest && head === git(['rev-parse', '--verify', 'HEAD']) &&
    tree === git(['rev-parse', 'HEAD^{tree}']), 'Source changed while measuring capability selections');
  if (foundationPolicy !== null) assert(digest(parse(read(opts['foundation-policy']))) === digest(foundationPolicy),
    'Foundation policy changed while measuring capability selections');
  const write = hooks.write || ((file, text) => {
    fs.mkdirSync(path.dirname(file), {recursive: true});
    fs.writeFileSync(file, text, {flag: 'wx'});
  });
  write(output, `${JSON.stringify(report, null, 2)}\n`);
  return report;
}

module.exports = {discoverManifests, selectionInventory, contractReport, compareReports, main};
if (require.main === module) {
  try { main(); } catch (error) {
    console.error(`Capability contract failed: ${error.message}`);
    process.exitCode = 1;
  }
}
