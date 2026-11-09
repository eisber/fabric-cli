// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

// This module consumes data only. Neither metadata nor the impact graph can
// supply commands; callers execute the resulting structured selection.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const {execFileSync} = require('node:child_process');

const PLATFORMS = ['linux', 'macos', 'windows'];
const SCRIPT_DIGEST = crypto.createHash('sha256').update(fs.readFileSync(__filename)).digest('hex');
const sorted = values => [...new Set(values)].sort();
const compare = (left, right) => left < right ? -1 : left > right ? 1 : 0;
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const stable = value => JSON.stringify(normalize(value));
function normalize(value) {
  if (Array.isArray(value)) return value.map(normalize);
  if (value && typeof value === 'object') return Object.fromEntries(
    Object.keys(value).sort().map(key => [key, normalize(value[key])]));
  return value;
}
const hash = value => crypto.createHash('sha256').update(stable(value)).digest('hex');
const parseJson = value => JSON.parse(String(value).replace(/^\uFEFF/, ''));
function metadataSnapshot(value) {
  if (Array.isArray(value)) return value.map(metadataSnapshot).sort((a, b) => compare(stable(a), stable(b)));
  if (value && typeof value === 'object') return Object.fromEntries(
    Object.keys(value).sort().map(key => [key, metadataSnapshot(value[key])]));
  return value;
}
function object(value, label, keys) {
  assert(value && typeof value === 'object' && !Array.isArray(value), `${label} must be an object`);
  assert(Object.keys(value).every(key => keys.includes(key)), `${label} has unknown fields`);
}
function array(value, label) {
  assert(Array.isArray(value), `${label} must be an array`);
  return value;
}
function strings(value, label) {
  const result = array(value === undefined ? [] : value, label);
  assert(result.every(item => typeof item === 'string' && item.length > 0 &&
    !/[\0\r\n]/.test(item)), `${label} must contain nonempty strings`);
  assert(new Set(result).size === result.length, `${label} has duplicates`);
  return sorted(result);
}
function identifier(value, label) {
  assert(typeof value === 'string' && /^[a-zA-Z0-9][a-zA-Z0-9_.-]*$/.test(value),
    `Invalid ${label}`);
  return value;
}
function relative(value, label = 'repository path', glob = false) {
  assert(typeof value === 'string' && value.length > 0 && !/[\0\r\n\\:]/.test(value) &&
    !value.startsWith('/') && !value.endsWith('/') &&
    value.split('/').every(part => part && part !== '.' && part !== '..'),
  `Noncanonical ${label}: ${value}`);
  if (glob) assert(!/[[\]{}!]/.test(value), `Unsupported glob in ${label}: ${value}`);
  return value;
}
function rootPath(root) {
  assert(typeof root === 'string' && path.isAbsolute(root), 'root must be absolute');
  return fs.existsSync(root) ? fs.realpathSync(root) : path.resolve(root);
}
function ownedPath(root, input, label) {
  assert(typeof input === 'string' && path.isAbsolute(input), `${label} must be absolute`);
  // Deleted/new inputs still need to resolve existing symlinked ancestors.
  let ancestor = path.resolve(input);
  const suffix = [];
  while (!fs.existsSync(ancestor) && path.dirname(ancestor) !== ancestor) {
    suffix.unshift(path.basename(ancestor));
    ancestor = path.dirname(ancestor);
  }
  const resolved = path.join(fs.existsSync(ancestor) ? fs.realpathSync(ancestor) : ancestor, ...suffix);
  const rel = path.relative(root, resolved).split(path.sep).join('/');
  return relative(rel, label);
}
function matcher(pattern) {
  relative(pattern, 'input glob', true);
  let expression = '^';
  for (let i = 0; i < pattern.length; i++) {
    const ch = pattern[i];
    if (ch === '*' && pattern[i + 1] === '*') {
      assert(i === 0 || pattern[i - 1] === '/', `Noncanonical glob: ${pattern}`);
      i++;
      assert(i === pattern.length - 1 || pattern[i + 1] === '/', `Noncanonical glob: ${pattern}`);
      if (pattern[i + 1] === '/') { expression += '(?:.*/)?'; i++; }
      else expression += '.*';
    } else if (ch === '*') expression += '[^/]*';
    else if (ch === '?') expression += '[^/]';
    else expression += ch.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  }
  return new RegExp(`${expression}$`);
}
function acyclic(nodes, dependencies, label) {
  const visiting = new Set(), visited = new Set();
  function visit(id) {
    assert(!visiting.has(id), `${label} cycle at ${id}`);
    if (visited.has(id)) return;
    visiting.add(id);
    for (const dep of dependencies(id)) {
      assert(nodes.has(dep), `Dangling ${label} reference: ${dep}`);
      visit(dep);
    }
    visiting.delete(id);
    visited.add(id);
  }
  for (const id of nodes.keys()) visit(id);
}
function closure(initial, edges) {
  const result = new Set(initial), queue = [...initial];
  for (let i = 0; i < queue.length; i++) for (const next of edges(queue[i]) || []) {
    if (!result.has(next)) { result.add(next); queue.push(next); }
  }
  return result;
}
function reverse(nodes, edges) {
  const result = new Map([...nodes.keys()].map(id => [id, new Set()]));
  for (const id of nodes.keys()) for (const dep of edges(id)) result.get(dep).add(id);
  return result;
}
/**
 * Return the reverse dependency closure within an explicitly supplied package
 * domain. Cargo paths resolve actual package names, not dependency aliases.
 * Unlisted dependencies never expand the caller's historical lane boundary.
 * Normalized dependencies may also be strings or source-less {name} records.
 */
function reverseConsumers(packages, seedNames) {
  const nodes = new Map(), directories = new Map();
  for (const pkg of array(packages, 'consumer packages')) {
    identifier(pkg.name, 'consumer package name');
    assert(!nodes.has(pkg.name), `Duplicate consumer package: ${pkg.name}`);
    nodes.set(pkg.name, {pkg, deps: new Set()});
    if (pkg.manifest_path !== undefined) {
      assert(path.isAbsolute(pkg.manifest_path), 'Consumer manifest must be absolute');
      const directory = path.resolve(path.dirname(pkg.manifest_path));
      assert(!directories.has(directory), `Duplicate consumer package path: ${directory}`);
      directories.set(directory, pkg.name);
    }
  }
  for (const [name, node] of nodes) for (const dependency of array(node.pkg.dependencies, 'consumer dependencies')) {
    const dep = typeof dependency === 'string' ? {name: dependency} : dependency;
    assert(dep && typeof dep === 'object', 'Invalid consumer dependency');
    let actual;
    if (dep.path !== undefined && dep.path !== null) {
      assert(typeof dep.path === 'string' && path.isAbsolute(dep.path), 'Consumer dependency path must be absolute');
      actual = directories.get(path.resolve(dep.path));
      if (actual) assert(dep.name === actual, `Consumer path dependency name mismatch: ${name}/${dep.name}`);
    } else if (!dep.source && nodes.has(dep.name)) actual = dep.name;
    if (actual) node.deps.add(actual);
  }
  const seeds = strings(seedNames, 'consumer seed names');
  for (const name of seeds) assert(nodes.has(name), `Unknown consumer seed: ${name}`);
  acyclic(nodes, name => nodes.get(name).deps, 'consumer dependency');
  const consumers = reverse(nodes, name => nodes.get(name).deps);
  return sorted(closure(seeds, name => consumers.get(name)));
}
function fileSnapshot(root, files) {
  return files.map(file => {
    relative(file);
    const absolute = path.join(root, ...file.split('/'));
    ownedPath(root, absolute, 'inventory input');
    let stat;
    try { stat = fs.lstatSync(absolute); } catch (error) {
      if (['ENOENT', 'ENOTDIR'].includes(error.code)) return {path: file, state: 'missing'};
      throw error;
    }
    if (stat.isSymbolicLink()) return {path: file, state: 'symlink',
      digest: hash(fs.readlinkSync(absolute))};
    assert(stat.isFile(), `Changed input is not a regular file: ${file}`);
    const digest = crypto.createHash('sha256'), buffer = Buffer.alloc(65536);
    const descriptor = fs.openSync(absolute, 'r');
    try {
      let count;
      while ((count = fs.readSync(descriptor, buffer, 0, buffer.length, null)) > 0) {
        digest.update(buffer.subarray(0, count));
      }
      const after = fs.fstatSync(descriptor);
      assert(after.size === stat.size && after.mtimeMs === stat.mtimeMs &&
        after.ctimeMs === stat.ctimeMs, `Input changed while hashing: ${file}`);
    } finally { fs.closeSync(descriptor); }
    return {path: file, state: 'file', size: stat.size, mode: stat.mode & 0o777, digest: digest.digest('hex')};
  });
}
function fingerprinted(value) {
  const {fingerprint: ignored, ...body} = value;
  return {...body, fingerprint: hash(body)};
}
function inventory(root, files, scope) {
  const snapshots = fileSnapshot(root, sorted(files));
  return {scope, files: snapshots, digest: hash(snapshots)};
}
function gitFileInventory(git) {
  return sorted([
    ...git(['ls-files', '--cached', '-z', '--']).split('\0').filter(Boolean),
    ...git(['ls-files', '--others', '--exclude-standard', '-z']).split('\0').filter(Boolean),
  ]);
}
function targetName(target) {
  const kinds = target.kind || [];
  if (kinds.some(kind => ['lib', 'rlib', 'dylib', 'cdylib', 'staticlib', 'proc-macro'].includes(kind))) return 'lib';
  for (const kind of ['bin', 'test', 'example', 'bench']) if (kinds.includes(kind)) return `${kind}:${target.name}`;
  return undefined;
}
function metadataModel(metadata, root) {
  const documents = Array.isArray(metadata) ? metadata : [metadata];
  assert(documents.length > 0, 'Metadata must not be empty');
  const packages = new Map(), ids = new Map();
  for (const document of documents) {
    assert(document && document.version === 1, 'Expected Cargo metadata format version 1');
    const members = new Set(strings(document.workspace_members, 'workspace_members'));
    const seen = new Set();
    for (const pkg of array(document.packages, 'metadata packages')) {
      if (!members.has(pkg.id)) continue;
      assert(!seen.has(pkg.id), `Duplicate metadata package: ${pkg.id}`);
      seen.add(pkg.id);
      identifier(pkg.name, 'package name');
      const manifest = ownedPath(root, pkg.manifest_path, 'package manifest');
      assert(manifest.endsWith('/Cargo.toml'), 'Expected package Cargo.toml');
      const directory = manifest.slice(0, -'/Cargo.toml'.length);
      const dependencies = array(pkg.dependencies, `dependencies of ${pkg.name}`);
      const features = pkg.features === undefined ? {} : pkg.features;
      assert(features && typeof features === 'object' && !Array.isArray(features), 'Invalid metadata features');
      for (const [feature, entries] of Object.entries(features)) {
        assert(feature.length > 0 && !/[\0\r\n/]/.test(feature), 'Invalid metadata feature name');
        strings(entries, `feature ${pkg.name}/${feature}`);
      }
      const targets = new Map();
      for (const target of array(pkg.targets, `targets of ${pkg.name}`)) {
        const name = targetName(target);
        if (!name) continue;
        assert(!targets.has(name), `Duplicate target: ${pkg.name}/${name}`);
        const src = ownedPath(root, target.src_path, 'target source');
        const required = strings(target['required-features'] ?? target.required_features,
          `required features of ${pkg.name}/${name}`);
        for (const feature of required) assert(Object.hasOwn(features, feature),
          `Unknown required feature: ${pkg.name}/${feature}`);
        targets.set(name, {name, src, required, test: target.test !== false});
      }
      const current = {name: pkg.name, id: pkg.id, directory, manifest, features,
        targets, dependencies: [...dependencies], deps: new Set(), aliases: new Map()};
      const previous = packages.get(pkg.name);
      if (previous) {
        assert(previous.id === pkg.id && previous.manifest === manifest &&
          stable(previous.features) === stable(features) &&
          stable([...previous.targets]) === stable([...targets]), `Conflicting metadata package: ${pkg.name}`);
        previous.dependencies.push(...dependencies);
      } else {
        assert(!ids.has(pkg.id), `Duplicate metadata id: ${pkg.id}`);
        assert(![...packages.values()].some(item => item.directory === directory),
          `Duplicate package path: ${directory}`);
        packages.set(pkg.name, current);
        ids.set(pkg.id, pkg.name);
      }
    }
    for (const member of members) assert(seen.has(member), `Missing workspace member: ${member}`);
  }
  assert(packages.size > 0, 'No workspace packages in metadata');
  const directories = new Map([...packages.values()].map(pkg => [pkg.directory, pkg.name]));
  for (const pkg of packages.values()) {
    for (const dependency of pkg.dependencies) {
      const alias = dependency.rename || dependency.name;
      identifier(alias, 'dependency name');
      let actual;
      if (dependency.path != null) {
        const directory = ownedPath(root, dependency.path, 'path dependency');
        actual = directories.get(directory);
        assert(actual, `Dangling workspace path dependency: ${directory}`);
        assert(dependency.name === actual, `Path dependency name mismatch: ${dependency.name}`);
        pkg.deps.add(actual);
      }
      const previous = pkg.aliases.get(alias);
      assert(!previous || previous.name === dependency.name, `Conflicting dependency alias: ${alias}`);
      const features = strings(dependency.features, `dependency features of ${pkg.name}/${alias}`);
      pkg.aliases.set(alias, {name: dependency.name, internal: actual || previous?.internal,
        optional: dependency.optional === true && (!previous || previous.optional),
        defaultFeatures: dependency.uses_default_features !== false || !!previous?.defaultFeatures,
        features: sorted([...features, ...(previous?.features || [])])});
    }
    for (const entries of Object.values(pkg.features)) for (const entry of entries) {
      if (entry.startsWith('dep:')) {
        assert(pkg.aliases.has(entry.slice(4)), `Unknown feature dependency: ${pkg.name}/${entry}`);
      } else if (entry.includes('/')) {
        const [alias, feature, ...rest] = entry.split('/');
        assert(rest.length === 0 && feature && pkg.aliases.has(alias.replace(/\?$/, '')),
          `Unknown dependency feature: ${pkg.name}/${entry}`);
        const dependency = pkg.aliases.get(alias.replace(/\?$/, ''));
        if (dependency.internal) assert(Object.hasOwn(packages.get(dependency.internal).features, feature),
          `Unknown internal dependency feature: ${pkg.name}/${entry}`);
      } else assert(Object.hasOwn(pkg.features, entry), `Unknown local feature: ${pkg.name}/${entry}`);
    }
    for (const dependency of pkg.aliases.values()) if (dependency.internal) {
      for (const feature of dependency.features) assert(
        Object.hasOwn(packages.get(dependency.internal).features, feature),
        `Unknown declared dependency feature: ${pkg.name}/${feature}`);
    }
  }
  acyclic(packages, name => packages.get(name).deps, 'Cargo dependency');
  const consumers = reverse(packages, name => packages.get(name).deps);
  return {packages, consumers};
}
function graphModel(graph, packages) {
  object(graph, 'graph', ['schema', 'copyright', 'components', 'profiles', 'checks']);
  assert(graph.schema === 1, 'Unknown impact graph schema');
  assert(graph.copyright === undefined ||
    graph.copyright === 'Copyright (c) Microsoft Corporation. Licensed under the MIT License.',
  'Invalid impact graph copyright');
  function named(items, label, keys, convert) {
    const result = new Map();
    for (const item of array(items === undefined ? [] : items, label)) {
      object(item, label, ['id', ...keys]);
      const id = identifier(item.id, `${label} id`);
      assert(!result.has(id), `Duplicate ${label} id: ${id}`);
      result.set(id, {id, ...convert(item)});
    }
    return result;
  }
  function packageNames(value, label) {
    const names = strings(value, label);
    for (const name of names) assert(packages.has(name), `Unknown package: ${name}`);
    return names;
  }
  function inputs(value) {
    return strings(value, 'inputs').map(pattern => ({pattern, match: matcher(pattern)}));
  }
  function platforms(value) {
    const result = value === undefined ? PLATFORMS : strings(value, 'platforms');
    assert(result.length > 0 && result.every(item => PLATFORMS.includes(item)), 'Unknown or empty platforms');
    return result;
  }
  const checks = named(graph.checks, 'check', ['packages', 'components', 'inputs', 'platforms', 'depends_on'],
    item => ({packages: packageNames(item.packages, 'check packages'),
      components: strings(item.components, 'check components'), inputs: inputs(item.inputs),
      platforms: platforms(item.platforms), depends_on: strings(item.depends_on, 'check dependencies')}));
  const components = named(graph.components, 'component', ['package', 'inputs', 'depends_on', 'tests', 'checks', 'coverage'],
    item => {
      assert(packages.has(item.package), `Unknown component package: ${item.package}`);
      const coverage = item.coverage === undefined ? 'runtime' : item.coverage;
      assert(['runtime', 'structural'].includes(coverage), `Unknown component coverage: ${coverage}`);
      const tests = [], seen = new Set();
      for (const test of array(item.tests === undefined ? [] : item.tests, 'component tests')) {
        object(test, 'component test', ['package', 'target', 'filter']);
        assert(packages.has(test.package), `Unknown test package: ${test.package}`);
        assert(typeof test.target === 'string' && /^(lib|(?:bin|test|example):[A-Za-z0-9_.-]+)$/.test(test.target) &&
          packages.get(test.package).targets.has(test.target),
        `Unknown test target: ${test.package}/${test.target}`);
        assert(test.filter === undefined || (typeof test.filter === 'string' && test.filter.trim() &&
          !/[\0\r\n]/.test(test.filter)), 'Invalid test filter');
        const key = stable(test);
        assert(!seen.has(key), 'Duplicate component test');
        seen.add(key);
        tests.push({...test});
      }
      return {package: item.package, coverage, inputs: inputs(item.inputs),
        depends_on: strings(item.depends_on, 'component dependencies'), tests,
        checks: strings(item.checks, 'component checks')};
    });
  const profiles = named(graph.profiles, 'profile', ['packages', 'features', 'platforms', 'checks'],
    item => {
      const names = packageNames(item.packages, 'profile packages');
      const features = strings(item.features, 'profile features');
      for (const feature of features) {
        const parts = feature.split('/');
        assert(parts.length === 2 && packages.has(parts[0]) &&
          Object.hasOwn(packages.get(parts[0]).features, parts[1]),
        `Unknown profile feature: ${feature}`);
        assert(names.includes(parts[0]), `Feature package missing from profile: ${feature}`);
      }
      assert(names.length > 0, 'Profile packages must not be empty');
      return {packages: names, features, platforms: platforms(item.platforms),
        checks: strings(item.checks, 'profile checks')};
    });
  acyclic(components, id => components.get(id).depends_on, 'component');
  acyclic(checks, id => checks.get(id).depends_on, 'check');
  for (const check of checks.values()) for (const dependency of check.depends_on) {
    assert(check.platforms.every(platform => checks.get(dependency).platforms.includes(platform)),
      `Check dependency unavailable on a dependent platform: ${check.id}/${dependency}`);
  }
  for (const item of [...components.values(), ...profiles.values()]) for (const check of item.checks) {
    assert(checks.has(check), `Unknown check: ${check}`);
  }
  for (const check of checks.values()) for (const component of check.components) {
    assert(components.has(component), `Unknown check component: ${component}`);
  }
  return {components, profiles, checks, consumers: reverse(components, id => components.get(id).depends_on)};
}
function featureSet(packages, features = [], roots = [...packages.keys()]) {
  const active = new Set(), initialized = new Set(), enabled = new Set(), deferred = new Map(), queue = [];
  function activatePackage(name, defaults) {
    if (defaults) queue.push(`${name}/default`);
    if (initialized.has(name)) return;
    initialized.add(name);
    for (const [alias, dependency] of packages.get(name).aliases) {
      if (!dependency.optional) enableDependency(name, alias);
    }
  }
  function enableDependency(name, alias) {
    const key = `${name}:${alias}`;
    if (enabled.has(key)) return;
    enabled.add(key);
    const dependency = packages.get(name).aliases.get(alias);
    if (dependency.internal) {
      activatePackage(dependency.internal, dependency.defaultFeatures);
      for (const feature of dependency.features) queue.push(`${dependency.internal}/${feature}`);
      queue.push(...(deferred.get(key) || []));
    }
  }
  for (const name of roots) activatePackage(name, true);
  queue.push(...features);
  for (let i = 0; i < queue.length; i++) {
    const key = queue[i];
    if (active.has(key)) continue;
    const [name, feature] = key.split('/');
    const pkg = packages.get(name);
    if (!pkg || !Object.hasOwn(pkg.features, feature)) continue;
    active.add(key);
    for (const entry of pkg.features[feature]) {
      if (entry.startsWith('dep:')) { enableDependency(name, entry.slice(4)); continue; }
      if (entry.includes('/')) {
        const [alias, next] = entry.split('/');
        const dependency = pkg.aliases.get(alias.replace(/\?$/, ''));
        if (alias.endsWith('?')) {
          const edge = `${name}:${alias.slice(0, -1)}`;
          if (dependency.internal) {
            const feature = `${dependency.internal}/${next}`;
            if (enabled.has(edge)) queue.push(feature);
            else deferred.set(edge, [...(deferred.get(edge) || []), feature]);
          }
        } else {
          enableDependency(name, alias);
          if (dependency.internal) queue.push(`${dependency.internal}/${next}`);
        }
      } else queue.push(`${name}/${entry}`);
    }
  }
  return active;
}
function fullInput(file) {
  return /(^|\/)Cargo\.(toml|lock)$/.test(file) ||
    /(^|\/)rust-toolchain(?:\.toml)?$/.test(file) ||
    /(^|\/)\.cargo\//.test(file) || file.startsWith('.github/') ||
    /^rust\/scripts\/(?:impact-plan(?:\.test)?|impact-runner(?:\.test)?|rust-fast-check(?:\.test)?)\.cjs$/.test(file) ||
    file.startsWith('rust/validation/') ||
    ['Cargo.toml', 'Cargo.lock', 'rustfmt.toml', 'clippy.toml', '.clippy.toml',
      'rust/rustfmt.toml', 'rust/clippy.toml', 'rust/.clippy.toml'].includes(file);
}
function documentation(file) {
  // Deleted legacy paths must remain documentation-only during directory renames.
  return /^(README(?:\.[^/]+)?|AGENTS\.md)$/i.test(file) ||
    /^rust\/(?:authoring-docs|docs)\/.+\.md$/i.test(file) ||
    file === 'rust/AGENTS.md' || file === 'rust/README.md';
}

/**
 * Compute a platform-independent validation selection from Cargo metadata and
 * the declarative schema-1 impact graph. Unknown inputs conservatively go full.
 */
function plan({metadata, graph, files, root, full = false, reason}) {
  root = rootPath(root);
  const model = metadataModel(metadata, root), packages = model.packages;
  const rules = graphModel(graph, packages);
  files = sorted(array(files, 'changed files').map(file => {
    relative(file);
    ownedPath(root, path.join(root, ...file.split('/')), 'changed input');
    return file;
  }));
  const reasons = new Set(), explanations = [], changed = new Set(), affected = new Set();
  const selectedComponents = new Set(), selectedChecks = new Set(), unscoped = new Set();
  const runtimeChecks = new Set(), profileAffected = new Set();
  const testSelections = new Map();
  function addTest(test, why) {
    const key = stable({package: test.package, target: test.target, ...(test.filter === undefined ? {} : {filter: test.filter})});
    if (!testSelections.has(key)) testSelections.set(key, {...test, reasons: new Set()});
    testSelections.get(key).reasons.add(why);
  }
  function packageTests(name, why) { addTest({package: name, target: 'all'}, why); }
  function markFull(why) { full = true; reasons.add(why); }
  function selectCheck(id, runtime = true) {
    selectedChecks.add(id);
    if (runtime) runtimeChecks.add(id);
  }
  if (full) reasons.add(reason || 'Explicit full validation requested');
  else if (reason) reasons.add(reason);
  const owners = [...packages.values()].sort((a, b) => b.directory.length - a.directory.length);
  for (const file of files) {
    if (fullInput(file)) { markFull(`Shared validation or Cargo input: ${file}`); continue; }
    const matched = [...rules.components.values()].filter(item => item.inputs.some(input => input.match.test(file)));
    const inputChecks = [...rules.checks.values()].filter(item => item.inputs.some(input => input.match.test(file)));
    for (const check of inputChecks) {
      selectCheck(check.id);
      for (const name of check.packages) { affected.add(name); profileAffected.add(name); }
      explanations.push({file, check: check.id, reason: 'Declared check input'});
    }
    if (matched.length) {
      for (const component of matched) {
        selectedComponents.add(component.id);
        if (component.coverage === 'runtime') changed.add(component.package);
      }
      explanations.push({file, components: sorted(matched.map(item => item.id)), reason: 'Declared component input'});
      if (matched.some(component => component.coverage === 'runtime')) continue;
    }
    if (documentation(file)) {
      explanations.push({file, reason: 'Documentation only'});
      continue;
    }
    const owner = owners.find(pkg => file.startsWith(`${pkg.directory}/`));
    if (owner) {
      changed.add(owner.name);
      const target = [...owner.targets.values()].find(item =>
        item.src === file && /^(test|example|bin):/.test(item.name));
      if (target) {
        addTest({package: owner.name, target: target.name}, `Direct target source: ${file}`);
        // A consumer still needs its own tests; a top-level target does not
        // establish coverage of other packages in the Cargo reverse closure.
        unscoped.add(owner.name);
        explanations.push({file, package: owner.name, target: target.name, reason: 'Direct Cargo target source'});
      } else {
        unscoped.add(owner.name);
        packageTests(owner.name, `Unmapped package input: ${file}`);
        explanations.push({file, package: owner.name, reason: 'Unmapped package input; full package fallback'});
      }
    } else if (!inputChecks.length) markFull(`Unknown shared input: ${file}`);
  }
  const scopedSeeds = [...selectedComponents].filter(id => rules.components.get(id).coverage === 'runtime')
    .map(id => rules.components.get(id).package);
  // Structural contracts protect catalog additions too, but do not claim that
  // a runtime consumer's behavioral tests have been covered.
  if (changed.size || full) for (const component of rules.components.values()) {
    if (component.coverage === 'structural') selectedComponents.add(component.id);
  }
  const componentClosure = closure(selectedComponents, id => rules.consumers.get(id));
  for (const id of componentClosure) {
    const component = rules.components.get(id);
    const runtime = component.coverage === 'runtime';
    for (const check of component.checks) selectCheck(check, runtime);
    for (const test of component.tests) {
      addTest(test, `Component closure: ${id}`);
      affected.add(test.package);
      if (runtime) profileAffected.add(test.package);
    }
    affected.add(component.package);
    if (runtime) profileAffected.add(component.package);
  }
  for (const check of rules.checks.values()) {
    if (check.components.some(id => componentClosure.has(id))) selectCheck(check.id,
      check.components.some(id => componentClosure.has(id) && rules.components.get(id).coverage === 'runtime'));
  }
  for (const name of changed) { affected.add(name); profileAffected.add(name); }
  const compileClosure = closure(affected, name => model.consumers.get(name));
  for (const name of compileClosure) affected.add(name);
  const unscopedClosure = closure(unscoped, name => model.consumers.get(name));
  const scopedCoverage = new Set();
  for (const id of componentClosure) {
    const component = rules.components.get(id);
    if (component.coverage === 'runtime') for (const test of component.tests) scopedCoverage.add(test.package);
  }
  const scopedCargo = closure(scopedSeeds, name => model.consumers.get(name));
  for (const name of scopedCargo) if (!scopedCoverage.has(name)) {
    const why = `Cargo consumer ${name} has no explicit component test coverage; full package fallback`;
    packageTests(name, why);
    explanations.push({package: name, reason: why});
  }
  for (const name of unscopedClosure) {
    // Keep an exact direct top-level target unless another unmapped file or
    // changed Cargo dependency requires the whole package.
    const ownExactOnly = unscoped.has(name) &&
      [...testSelections.values()].some(test => test.package === name) &&
      ![...unscoped].some(other => other !== name &&
        closure([other], pkg => model.consumers.get(pkg)).has(name));
    if (!ownExactOnly) packageTests(name, 'Unscoped Cargo reverse consumer');
  }
  for (const check of rules.checks.values()) {
    if (check.packages.some(name => unscopedClosure.has(name))) selectCheck(check.id);
  }
  if (full) {
    for (const name of packages.keys()) { affected.add(name); packageTests(name, 'Full validation'); }
    for (const id of rules.components.keys()) componentClosure.add(id);
    for (const id of rules.checks.keys()) selectCheck(id);
  }
  // Profiles and checks can introduce additional compile inputs. Stabilize the
  // union before deciding feature coverage, rather than relying on graph order.
  let selectedProfiles = [], checkClosure = new Set(), previous;
  do {
    previous = stable([sorted(affected), sorted(selectedChecks), sorted(profileAffected), sorted(runtimeChecks)]);
    for (const name of closure(profileAffected, name => model.consumers.get(name))) profileAffected.add(name);
    selectedProfiles = [...rules.profiles.values()].filter(profile =>
      full || profile.packages.some(name => profileAffected.has(name)));
    for (const profile of selectedProfiles) for (const check of profile.checks) selectCheck(check);
    checkClosure = closure(selectedChecks, id => rules.checks.get(id).depends_on);
    for (const id of checkClosure) for (const name of rules.checks.get(id).packages) affected.add(name);
    for (const id of closure(runtimeChecks, id => rules.checks.get(id).depends_on)) {
      for (const name of rules.checks.get(id).packages) profileAffected.add(name);
    }
    for (const name of closure(affected, name => model.consumers.get(name))) affected.add(name);
  } while (previous !== stable([sorted(affected), sorted(selectedChecks), sorted(profileAffected), sorted(runtimeChecks)]));
  const defaults = new Map([...packages.keys()].map(name => [name, featureSet(packages, [], [name])]));
  const profileFeatures = new Map(selectedProfiles.map(profile =>
    [profile.id, featureSet(packages, profile.features, profile.packages)]));
  let tests = [...testSelections.values()].filter(test => test.target === 'all' ||
    ![...testSelections.values()].some(other => other.package === test.package && other.target === 'all'));
  tests = tests.map(test => {
    const pkg = packages.get(test.package);
    const targets = test.target === 'all' ? [...pkg.targets.values()].filter(target => target.test) :
      [pkg.targets.get(test.target)];
    const required = sorted(targets.flatMap(target => target.required));
    const missing = targets.filter(target =>
      target.required.some(feature => !defaults.get(pkg.name).has(`${pkg.name}/${feature}`)));
    const applicable = selectedProfiles.filter(profile => profile.packages.includes(pkg.name));
    for (const target of missing) {
      assert(applicable.some(profile => target.required.every(feature =>
        profileFeatures.get(profile.id).has(`${pkg.name}/${feature}`))),
      `Required-feature target has no applicable profile: ${pkg.name}/${target.name}`);
    }
    return {...test, reasons: sorted(test.reasons), required_features: required,
      default_available: test.target === 'all' || missing.length === 0,
      default_skipped_targets: missing.map(target => target.name).sort(),
      profiles: sorted(applicable.filter(profile => targets.some(target =>
        target.required.every(feature => profileFeatures.get(profile.id).has(`${pkg.name}/${feature}`))))
        .map(profile => profile.id))};
  }).sort((a, b) => compare(stable([a.package, a.target, a.filter]), stable([b.package, b.target, b.filter])));
  const profiles = selectedProfiles.map(profile => ({id: profile.id, packages: profile.packages,
    features: profile.features, platforms: profile.platforms, checks: profile.checks,
    tests: tests.filter(test => profile.packages.includes(test.package) && test.profiles.includes(profile.id))}))
    .sort((a, b) => compare(a.id, b.id));
  const mode = full ? 'full' : affected.size || tests.length || checkClosure.size ? 'affected' : 'none';
  if (!reasons.size) reasons.add(mode === 'none' ? 'No executable inputs changed' : 'Dependency impact selection');
  const metadata_digest = hash([...packages.values()].map(pkg => ({name: pkg.name, directory: pkg.directory,
    dependencies: sorted(pkg.deps), features: pkg.features,
    dependency_declarations: sorted(pkg.dependencies.map(dep => stable({...dep,
      ...(dep.path ? {path: ownedPath(root, dep.path, 'dependency path')} : {})}))),
    targets: [...pkg.targets.values()].sort((a, b) => compare(a.name, b.name))}))
    .sort((a, b) => compare(a.name, b.name)));
  const checkOrder = [], orderedChecks = new Set();
  function orderCheck(id) {
    if (orderedChecks.has(id)) return;
    for (const dependency of rules.checks.get(id).depends_on) orderCheck(dependency);
    orderedChecks.add(id);
    checkOrder.push(id);
  }
  for (const id of sorted(checkClosure)) orderCheck(id);
  const snapshots = fileSnapshot(root, files);
  const targetCatalog = sorted(affected).flatMap(name => [...packages.get(name).targets.values()]
    .map(target => ({package: name, target: target.name, source: target.src,
      required_features: target.required, test: target.test,
      default_available: target.required.every(feature => defaults.get(name).has(`${name}/${feature}`)),
      profiles: sorted(selectedProfiles.filter(profile => profile.packages.includes(name) &&
        target.required.every(feature => profileFeatures.get(profile.id).has(`${name}/${feature}`)))
        .map(profile => profile.id))})))
    .sort((a, b) => compare(stable([a.package, a.target]), stable([b.package, b.target])));
  const metadataInputDigest = hash(metadataSnapshot(metadata));
  const inputs = {schema: 1, root, base: null, head: null, files,
    graph_digest: hash(graph), script_digest: SCRIPT_DIGEST, metadata_digest: metadataInputDigest,
    inventory: {scope: 'explicit-files', files: snapshots, digest: hash(snapshots)}};
  const result = {schema: 1, root, mode, reason: sorted(reasons).join('; '), reasons: sorted(reasons), files,
    changed_packages: sorted(changed), packages: sorted(affected), components: sorted(componentClosure),
    tests, profiles, checks: sorted(checkClosure), check_order: checkOrder,
    check_details: sorted(checkClosure).map(id => {
      const check = rules.checks.get(id);
      return {id, packages: check.packages, components: check.components,
        platforms: check.platforms, depends_on: check.depends_on};
    }),
    explanations: explanations.sort((a, b) => compare(stable(a), stable(b))),
    graph_digest: hash(graph), metadata_digest, metadata_input_digest: metadataInputDigest,
    script_digest: SCRIPT_DIGEST, file_snapshot: snapshots, target_catalog: targetCatalog, inputs,
    synthetic_scope: true, head_complete: false, evidence_scope: 'synthetic-inputs'};
  return fingerprinted(result);
}

/** Generate shell-free Cargo command descriptors; additional checks are registry IDs. */
function commands(selection, platform) {
  platform = ({win32: 'windows', darwin: 'macos'}[platform] || platform);
  assert(PLATFORMS.includes(platform), 'Unknown execution platform');
  const result = [], skipped = [];
  if (selection.mode === 'none') return {commands: [], checks: [], skipped: []};
  const cargo = (kind, args, extra = {}) => result.push({program: 'cargo', args, kind, ...extra});
  const packages = selection.packages.flatMap(name => ['--package', name]);
  if (packages.length) {
    cargo('check', ['check', '--offline', '--locked', '--all-targets', ...packages]);
    cargo('clippy', ['clippy', '--offline', '--locked', '--all-targets', ...packages, '--', '-D', 'warnings']);
  }
  function testCommand(test, features, profile) {
    const args = ['test', '--offline', '--locked', '--package', test.package];
    if (test.target === 'all') args.push('--all-targets');
    else if (test.target === 'lib') args.push('--lib');
    else { const colon = test.target.indexOf(':'); args.push(`--${test.target.slice(0, colon)}`, test.target.slice(colon + 1)); }
    if (features?.length) args.push('--features', features.join(','));
    if (test.filter !== undefined) args.push('--', test.filter);
    cargo('test', args, {package: test.package, target: test.target, ...(profile ? {profile} : {})});
  }
  for (const test of selection.tests) {
    if (test.default_available) testCommand(test);
    else skipped.push({package: test.package, target: test.target, reason: 'Requires feature profile'});
    if (test.target === 'all') for (const target of test.default_skipped_targets) {
      skipped.push({package: test.package, target, reason: 'Requires feature profile'});
    }
  }
  for (const profile of selection.profiles) {
    if (!profile.platforms.includes(platform)) {
      skipped.push({profile: profile.id, reason: `Unavailable on ${platform}`});
      continue;
    }
    const args = profile.packages.flatMap(name => ['--package', name]);
    const features = profile.features.length ? ['--features', profile.features.join(',')] : [];
    cargo('check', ['check', '--offline', '--locked', '--all-targets', ...args, ...features], {profile: profile.id});
    cargo('clippy', ['clippy', '--offline', '--locked', '--all-targets', ...args, ...features, '--', '-D', 'warnings'],
      {profile: profile.id});
    for (const test of profile.tests) testCommand(test, profile.features, profile.id);
  }
  const checks = [];
  const details = new Map(selection.check_details.map(check => [check.id, check]));
  for (const id of selection.check_order) {
    const check = details.get(id);
    if (check.platforms.includes(platform)) checks.push(check.id);
    else skipped.push({check: check.id, reason: `Unavailable on ${platform}`});
  }
  return {commands: result, checks, skipped};
}

function options(argv) {
  const result = {};
  const values = new Set(['root', 'base', 'head', 'files-json', 'metadata', 'graph', 'output', 'github-output', 'summary']);
  for (let i = 0; i < argv.length; i++) {
    assert(argv[i].startsWith('--'), 'Expected a named option');
    const name = argv[i].slice(2);
    assert((values.has(name) || name === 'full') && !Object.hasOwn(result, name), `Unknown or duplicate option: ${argv[i]}`);
    if (name === 'full') result.full = true;
    else {
      assert(i + 1 < argv.length && !argv[i + 1].startsWith('--'), `Missing value: ${argv[i]}`);
      result[name] = argv[++i];
    }
  }
  for (const key of ['base', 'head']) if (result[key] !== undefined) {
    assert(/^[a-fA-F0-9]{40}$/.test(result[key]), `${key} must be a full 40-hex commit`);
    result[key] = result[key].toLowerCase();
  }
  for (const key of ['root', 'files-json', 'metadata', 'graph', 'output', 'github-output', 'summary']) {
    if (result[key] !== undefined) assert(path.isAbsolute(result[key]), `${key} must be absolute`);
  }
  assert(result.output, '--output is required');
  return result;
}

/**
 * CLI entry point. The injected exec/read/write hooks make local and CI discovery
 * testable without invoking Git or Cargo in unit tests.
 */
function main(argv = process.argv.slice(2), hooks = {}) {
  const opts = options(argv), root = rootPath(opts.root || path.resolve(__dirname, '../..'));
  const exec = hooks.exec || ((program, args, cwd) => execFileSync(program, args,
    {cwd, encoding: 'utf8', timeout: program === 'git' ? 15000 : 120000,
      maxBuffer: 64 * 1024 * 1024, windowsHide: true}));
  const read = hooks.read || (file => fs.readFileSync(file, 'utf8'));
  const write = hooks.write || ((file, content, append = false) => {
    fs.mkdirSync(path.dirname(file), {recursive: true});
    if (append) fs.appendFileSync(file, content); else fs.writeFileSync(file, content);
  });
  const env = hooks.env || process.env;
  const git = args => String(exec('git', args, root));
  const split = output => output.split('\0').filter(Boolean);
  let full = !!opts.full, reason, base = opts.base || null;
  const actualHead = git(['rev-parse', '--verify', 'HEAD']).trim();
  let head = actualHead;
  if (opts.head) {
    git(['cat-file', '-e', `${opts.head}^{commit}`]);
    assert(opts.head === actualHead, 'Requested head differs from the checked-out HEAD');
    head = opts.head;
  }
  assert(/^[a-f0-9]{40}$/.test(head), 'Git HEAD must be a full commit');
  if (base) {
    try {
      assert(!/^0+$/.test(base), 'Zero base');
      git(['cat-file', '-e', `${base}^{commit}`]);
    } catch {
      full = true;
      reason = 'Base commit is zero or unavailable; full validation required';
    }
  } else if (env.CI || env.GITHUB_ACTIONS) {
    full = true;
    reason = 'CI base commit missing; full validation required';
  }
  const dirtyFiles = sorted([
    ...split(git(['diff', '--name-only', '--no-renames', '-z', 'HEAD', '--'])),
    ...split(git(['ls-files', '--others', '--exclude-standard', '-z'])),
  ]);
  const committedFiles = !base || reason ? [] :
    split(git(['diff', '--name-only', '--no-renames', '-z', base, head, '--']));
  const providedFiles = opts['files-json'] ?
    array(parseJson(read(opts['files-json'])), 'files JSON') : [];
  const files = sorted([...dirtyFiles, ...committedFiles, ...providedFiles]);
  const metadata = opts.metadata ? parseJson(read(opts.metadata)) : parseJson(exec('cargo',
    ['metadata', '--offline', '--locked', '--no-deps', '--format-version', '1'], path.join(root, 'rust')));
  const graph = parseJson(read(opts.graph || path.join(root, 'rust', 'validation', 'impact-graph.json')));
  const selection = plan({metadata, graph, files, root, full, reason});
  const provenance = {base, head, dirty: dirtyFiles.length > 0, dirty_files: dirtyFiles,
    dirty_snapshot: selection.file_snapshot.filter(item => dirtyFiles.includes(item.path)),
    synthetic_scope: !!opts['files-json'], head_complete: !opts['files-json'],
    evidence_scope: opts['files-json'] ? 'synthetic-inputs' : 'checkout'};
  const inputs = {...selection.inputs, base, head, ci: !!(env.CI || env.GITHUB_ACTIONS),
    full: !!opts.full, graph_path: opts.graph || path.join(root, 'rust', 'validation', 'impact-graph.json'),
    metadata_path: opts.metadata || null, files_json: opts['files-json'] || null,
    evidence_scope: provenance.evidence_scope,
    inventory: inventory(root, gitFileInventory(git), 'git-tracked-and-untracked')};
  const result = fingerprinted({...selection, ...provenance, inputs});
  write(opts.output, `${JSON.stringify(result, null, 2)}\n`);
  if (opts['github-output']) {
    const fields = {mode: result.mode, fingerprint: result.fingerprint,
      packages: JSON.stringify(result.packages), changed_packages: JSON.stringify(result.changed_packages),
      checks: JSON.stringify(result.checks), components: JSON.stringify(result.components),
      profiles: JSON.stringify(result.profiles.map(profile => profile.id))};
    write(opts['github-output'], Object.entries(fields).map(([key, value]) => `${key}=${value}\n`).join(''), true);
  }
  if (opts.summary) write(opts.summary, `## Rust dependency impact\n\nMode: **${result.mode}**\n\n` +
    `${result.reasons.join('; ')}\n\nPackages: ${result.packages.join(', ') || '(none)'}\n\n` +
    `Components: ${result.components.join(', ') || '(none)'}\n\n` +
    `Checks: ${result.checks.join(', ') || '(none)'}\n\nFingerprint: \`${result.fingerprint}\`\n`);
  return result;
}

/**
 * Verify receipt integrity and source freshness without running Cargo. Git-free
 * synthetic API plans validate only their explicit inputs and cannot establish
 * HEAD-complete evidence. This is a freshness check, not an approval signature.
 */
function verifyPlan(selection, {root = selection.inputs?.root, exec, read} = {}) {
  assert(selection?.schema === 1 && selection.inputs?.schema === 1, 'Unknown plan/input schema');
  assert(fingerprinted(selection).fingerprint === selection.fingerprint, 'Plan fingerprint mismatch');
  root = rootPath(root);
  assert(root === selection.inputs.root && root === selection.root, 'Plan root differs from verification root');
  const readFile = read || (file => fs.readFileSync(file, 'utf8'));
  const currentScript = crypto.createHash('sha256').update(fs.readFileSync(__filename)).digest('hex');
  assert(selection.script_digest === SCRIPT_DIGEST && selection.script_digest === currentScript &&
    selection.inputs.script_digest === currentScript, 'Planner source changed');
  assert(selection.inputs.graph_digest === selection.graph_digest &&
    selection.inputs.metadata_digest === selection.metadata_input_digest, 'Input digest provenance mismatch');
  assert(stable(selection.inputs.files) === stable(selection.files), 'Changed-file provenance mismatch');
  if (selection.inputs.graph_path) assert(hash(parseJson(readFile(selection.inputs.graph_path))) ===
    selection.graph_digest, 'Impact graph changed');
  if (selection.inputs.metadata_path) assert(
    hash(metadataSnapshot(parseJson(readFile(selection.inputs.metadata_path)))) === selection.metadata_input_digest,
    'Cargo metadata input changed');
  const previous = selection.inputs.inventory;
  assert(previous && ['explicit-files', 'git-tracked-and-untracked'].includes(previous.scope) &&
    previous.digest === hash(previous.files), 'Invalid inventory provenance');
  let names;
  if (previous.scope === 'git-tracked-and-untracked') {
    const execute = exec || ((program, args, cwd) => execFileSync(program, args,
      {cwd, encoding: 'utf8', timeout: 15000, maxBuffer: 64 * 1024 * 1024, windowsHide: true}));
    const git = args => String(execute('git', args, root));
    assert(git(['rev-parse', '--verify', 'HEAD']).trim() === selection.inputs.head, 'Checkout HEAD changed');
    names = gitFileInventory(git);
  } else {
    assert(selection.synthetic_scope && !selection.head_complete, 'Explicit inputs cannot prove HEAD completeness');
    names = previous.files.map(item => item.path);
  }
  const current = inventory(root, names, previous.scope);
  assert(current.digest === previous.digest, 'Candidate source inventory changed');
  assert(stable(fileSnapshot(root, selection.files)) === stable(selection.file_snapshot), 'Changed-file snapshot changed');
  return {valid: true, fingerprint: selection.fingerprint,
    synthetic_scope: selection.synthetic_scope, head_complete: selection.head_complete,
    evidence_scope: selection.evidence_scope};
}

module.exports = {plan, commands, main, reverseConsumers, verifyPlan};
if (require.main === module) {
  try { main(); } catch (error) {
    console.error(`Impact planning failed: ${error.message}`);
    process.exitCode = 1;
  }
}
