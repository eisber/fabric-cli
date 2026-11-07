// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const {test} = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const fs = require('node:fs');
const crypto = require('node:crypto');
const {plan, commands, main, reverseConsumers, verifyPlan} = require('./impact-plan.cjs');

// Pure data fixtures and injected I/O: this suite never runs Git/Cargo, creates
// a scratch checkout, reads credentials, or uses the host temporary directory.
const ROOT = path.resolve(__dirname, '..', '..', 'impact-fixture-root');
const absolute = value => path.join(ROOT, ...value.split('/'));
const clone = value => JSON.parse(JSON.stringify(value));
const target = (name, kind, src, features = []) => ({
  name, kind: [kind], src_path: absolute(src), test: true, 'required-features': features,
});
const packageDirectory = name => ({deploy: 'rust/sdk/deployment',
  adapter: 'rust/frontends/cli/commands/transfer', cli: 'rust/frontends/cli/app'}[name] || `rust/sdk/${name}`);
function pkg(name, dependencies = [], targets = [], features = {default: []}) {
  const directory = packageDirectory(name);
  return {id: `path+${name}#0.1.0`, name, manifest_path: absolute(`${directory}/Cargo.toml`),
    dependencies: dependencies.map(dependency => typeof dependency === 'string' ? {
      name: dependency, path: absolute(packageDirectory(dependency)), kind: null,
      optional: false, target: null, rename: null,
    } : dependency),
    features, targets: [target(name, 'lib', `${directory}/src/lib.rs`), ...targets]};
}
function metadata(packages) {
  return {version: 1, workspace_members: packages.map(item => item.id),
    workspace_default_members: packages.map(item => item.id), packages};
}
function fixture() {
  const packages = [
    pkg('models'),
    pkg('deploy', ['models'], [
      target('notebook', 'test', 'rust/sdk/deployment/tests/notebook.rs'),
      target('spark', 'test', 'rust/sdk/deployment/tests/spark.rs'),
      target('shared', 'test', 'rust/sdk/deployment/tests/shared.rs'),
    ]),
    pkg('adapter', ['deploy'], [target('publish', 'test', 'rust/frontends/cli/commands/transfer/tests/publish.rs')]),
    pkg('cli', ['adapter'], [target('mount', 'test', 'rust/frontends/cli/app/tests/mount.rs')]),
    pkg('unrelated'),
  ];
  const graph = {schema: 1, components: [
    {id: 'notebook', package: 'deploy', inputs: ['rust/sdk/deployment/src/notebook.rs'],
      tests: [{package: 'deploy', target: 'test:notebook'}], checks: ['notebook-oracle']},
    {id: 'spark', package: 'deploy', inputs: ['rust/sdk/deployment/src/spark.rs'],
      tests: [{package: 'deploy', target: 'test:spark'}], checks: ['spark-oracle']},
    {id: 'shared', package: 'deploy', inputs: ['rust/sdk/deployment/src/shared.rs'],
      tests: [{package: 'deploy', target: 'test:shared'}]},
    {id: 'notebook-adapter', package: 'adapter', depends_on: ['notebook', 'shared'],
      tests: [{package: 'adapter', target: 'test:publish'}]},
    {id: 'notebook-mount', package: 'cli', depends_on: ['notebook-adapter'],
      tests: [{package: 'cli', target: 'test:mount'}]},
    {id: 'spark-mount', package: 'cli', depends_on: ['spark', 'shared'],
      tests: [{package: 'cli', target: 'lib'}]},
  ], profiles: [], checks: [
    {id: 'notebook-oracle', components: ['notebook'], packages: ['cli']},
    {id: 'spark-oracle', components: ['spark'], packages: ['cli']},
    {id: 'all-cli-oracles', packages: ['cli']},
    {id: 'fixture-contract', inputs: ['rust/fixtures/schema/**'], packages: ['models'],
      depends_on: ['fixture-setup']},
    {id: 'fixture-setup'},
  ]};
  return {root: ROOT, metadata: metadata(packages), graph, files: []};
}
const select = (files, change = () => {}) => {
  const input = fixture();
  input.files = files;
  change(input);
  return plan(input);
};
const testKeys = result => result.tests.map(item => `${item.package}/${item.target}`);

test('Notebook selects component consumers, not Spark or broad CLI oracles', () => {
  const result = select(['rust/sdk/deployment/src/notebook.rs']);
  assert.equal(result.mode, 'affected');
  assert.deepEqual(result.changed_packages, ['deploy']);
  assert.deepEqual(result.packages, ['adapter', 'cli', 'deploy']);
  assert.deepEqual(result.components, ['notebook', 'notebook-adapter', 'notebook-mount']);
  assert.deepEqual(testKeys(result), ['adapter/test:publish', 'cli/test:mount', 'deploy/test:notebook']);
  assert.deepEqual(result.checks, ['notebook-oracle']);
  const executable = commands(result, 'linux');
  assert.equal(executable.commands.filter(item => item.kind === 'test').length, 3);
  for (const item of executable.commands.filter(item => item.kind !== 'test')) {
    assert.ok(item.args.includes('--all-targets'));
    for (const name of result.packages) assert.ok(item.args.includes(name));
  }
});

test('Spark excludes Notebook and safely covers the unmapped Cargo adapter', () => {
  const result = select(['rust/sdk/deployment/src/spark.rs']);
  assert.ok(testKeys(result).includes('deploy/test:spark'));
  assert.ok(testKeys(result).includes('adapter/all'));
  assert.ok(!testKeys(result).some(key => /notebook|publish|mount/.test(key)));
  assert.deepEqual(result.checks, ['spark-oracle']);
  assert.ok(result.explanations.some(item => item.package === 'adapter' && /fallback/.test(item.reason)));
});

test('Shared component propagates a diamond without duplicate test targets', () => {
  const result = select(['rust/sdk/deployment/src/shared.rs']);
  assert.deepEqual(result.components, ['notebook-adapter', 'notebook-mount', 'shared', 'spark-mount']);
  assert.deepEqual(testKeys(result), ['adapter/test:publish', 'cli/lib', 'cli/test:mount', 'deploy/test:shared']);
});

test('New Cargo consumer absent from component graph always receives full tests', () => {
  const result = select(['rust/sdk/deployment/src/notebook.rs'], input => {
    const consumer = pkg('new-consumer', ['deploy']);
    input.metadata.packages.push(consumer);
    input.metadata.workspace_members.push(consumer.id);
  });
  assert.ok(result.packages.includes('new-consumer'));
  assert.ok(testKeys(result).includes('new-consumer/all'));
});

function structuralContract(input, dependsOn = []) {
  input.metadata.packages.find(item => item.name === 'cli').targets.push(
    target('workspace_contract', 'test', 'rust/frontends/cli/app/tests/workspace_contract.rs'));
  input.graph.components.push({id: 'workspace-architecture', package: 'cli', coverage: 'structural',
    depends_on: dependsOn, tests: [{package: 'cli', target: 'test:workspace_contract'}]});
}

test('Structural CLI workspace contracts cannot claim runtime coverage of changed SDK Cargo consumers', () => {
  const result = select(['rust/sdk/models/src/lib.rs'], input => {
    input.graph.components.push({id: 'models-api', package: 'models',
      inputs: ['rust/sdk/models/src/lib.rs'], tests: [{package: 'models', target: 'lib'}]});
    structuralContract(input, ['models-api']);
  });
  assert.ok(result.components.includes('workspace-architecture'));
  assert.ok(testKeys(result).includes('cli/all'));
  assert.ok(result.tests.find(item => item.package === 'cli').reasons.some(reason => /no explicit component test coverage/.test(reason)));
  assert.ok(result.target_catalog.some(item => item.package === 'cli' && item.target === 'test:workspace_contract'));
});

test('Structural contracts also run for new unmapped Cargo packages without inventing CLI runtime/auth suites', () => {
  const result = select(['rust/sdk/isolated/src/lib.rs'], input => {
    const isolated = pkg('isolated');
    input.metadata.packages.push(isolated);
    input.metadata.workspace_members.push(isolated.id);
    structuralContract(input, ['notebook', 'spark', 'shared']);
    input.metadata.packages.find(item => item.name === 'cli').features.auth = [];
    input.graph.profiles.push({id: 'cli-auth', packages: ['cli'], features: ['cli/auth'],
      checks: ['native-contract']});
    input.graph.checks.push({id: 'native-contract', platforms: ['windows']},
      {id: 'structural-gate', packages: ['cli'], components: ['workspace-architecture']});
  });
  assert.deepEqual(result.changed_packages, ['isolated']);
  assert.deepEqual(result.packages, ['cli', 'isolated']);
  assert.deepEqual(result.components, ['workspace-architecture']);
  assert.deepEqual(testKeys(result), ['cli/test:workspace_contract', 'isolated/all']);
  assert.deepEqual(result.profiles, []);
  assert.deepEqual(result.checks, ['structural-gate']);
  for (const command of commands(result, 'linux').commands.filter(item => item.kind !== 'test')) {
    assert.ok(command.args.includes('cli'));
    assert.ok(command.args.includes('isolated'));
  }
});

test('Structural-only source mappings cannot suppress the actual source owner fallback', () => {
  const result = select(['rust/sdk/unrelated/src/lib.rs'], input => {
    structuralContract(input);
    input.graph.components.at(-1).inputs = ['rust/sdk/unrelated/src/lib.rs'];
  });
  assert.deepEqual(result.changed_packages, ['unrelated']);
  assert.deepEqual(testKeys(result), ['cli/test:workspace_contract', 'unrelated/all']);
});

test('Structural contracts augment rather than broaden explicitly covered runtime targets', () => {
  const result = select(['rust/sdk/deployment/src/notebook.rs'], input => structuralContract(input, ['notebook']));
  assert.deepEqual(testKeys(result), ['adapter/test:publish', 'cli/test:mount',
    'cli/test:workspace_contract', 'deploy/test:notebook']);
  assert.ok(!testKeys(result).includes('cli/all'));
});

test('Unmapped library source and shared test helpers select owner plus Cargo reverse closure', () => {
  for (const file of ['rust/sdk/deployment/src/new.rs', 'rust/sdk/deployment/tests/common/mod.rs',
    'rust/sdk/deployment/README.md', 'rust/sdk/deployment/data/schema.json']) {
    const result = select([file]);
    assert.deepEqual(testKeys(result), ['adapter/all', 'cli/all', 'deploy/all']);
    assert.deepEqual(result.checks, ['all-cli-oracles', 'notebook-oracle', 'spark-oracle']);
  }
});

test('Direct integration/bin/example targets stay exact, but consumers do not disappear', () => {
  const result = select(['rust/sdk/deployment/tests/notebook.rs']);
  assert.deepEqual(testKeys(result), ['adapter/all', 'cli/all', 'deploy/test:notebook']);
  const bin = select(['rust/frontends/cli/app/src/main.rs'], input => input.metadata.packages
    .find(item => item.name === 'cli').targets.push(target('fab', 'bin', 'rust/frontends/cli/app/src/main.rs')));
  assert.deepEqual(testKeys(bin), ['cli/bin:fab']);
  const example = select(['rust/frontends/cli/app/examples/demo.rs'], input => input.metadata.packages
    .find(item => item.name === 'cli').targets.push(target('demo', 'example', 'rust/frontends/cli/app/examples/demo.rs')));
  assert.deepEqual(testKeys(example), ['cli/example:demo']);
});

test('Unmapped files override exact target narrowing and scoped coverage in the same package', () => {
  const result = select(['rust/sdk/deployment/tests/notebook.rs', 'rust/sdk/deployment/tests/common.rs',
    'rust/sdk/deployment/src/notebook.rs']);
  assert.deepEqual(testKeys(result), ['adapter/all', 'cli/all', 'deploy/all']);
});

test('Deleted and renamed old/new paths remain impact inputs', () => {
  const deleted = select(['rust/sdk/deployment/src/deleted.rs']);
  assert.deepEqual(deleted.packages, ['adapter', 'cli', 'deploy']);
  const rename = select(['rust/sdk/deployment/src/notebook.rs', 'rust/sdk/deployment/src/spark.rs']);
  assert.ok(testKeys(rename).includes('deploy/test:notebook'));
  assert.ok(testKeys(rename).includes('deploy/test:spark'));
});

test('Renamed normal, dev, build, target-conditional and optional path dependencies are all edges', () => {
  const input = fixture();
  const declarations = ['normal', 'dev', 'build', 'conditional', 'optional'].map((name, i) =>
    pkg(name, [{name: 'models', rename: `alias_${i}`, path: absolute('rust/sdk/models'),
      kind: i === 1 ? 'dev' : i === 2 ? 'build' : null,
      target: i === 3 ? 'cfg(windows)' : null, optional: i === 4}]));
  input.metadata = metadata([pkg('models'), ...declarations]);
  input.graph = {schema: 1};
  input.files = ['rust/sdk/models/src/lib.rs'];
  assert.deepEqual(plan(input).packages, ['build', 'conditional', 'dev', 'models', 'normal', 'optional']);
});

test('Platform metadata documents union edges without resolving registry crates', () => {
  const base = metadata([pkg('models'), pkg('app')]);
  const other = clone(base);
  other.packages.find(item => item.name === 'app').dependencies.push({
    name: 'models', rename: 'renamed', path: absolute('rust/sdk/models'), target: 'cfg(windows)',
  });
  const result = plan({root: ROOT, metadata: [base, other], graph: {schema: 1},
    files: ['rust/sdk/models/src/lib.rs']});
  assert.deepEqual(result.packages, ['app', 'models']);
  assert.deepEqual(plan({root: ROOT, metadata: [other, base], graph: {schema: 1},
    files: ['rust/sdk/models/src/lib.rs']}).fingerprint, result.fingerprint);
});

test('Registry dependencies with a workspace-like name are not mistaken for path edges', () => {
  const input = fixture();
  input.metadata = metadata([pkg('models'), pkg('external-user', [{name: 'models', source: 'registry+x'}])]);
  input.graph = {schema: 1};
  input.files = ['rust/sdk/models/src/lib.rs'];
  assert.deepEqual(plan(input).packages, ['models']);
});

test('Shared reverseConsumers supports renamed path dependencies and every Cargo dependency kind', () => {
  const packages = [pkg('root'), ...['normal', 'dev', 'build', 'optional'].map((name, index) =>
    pkg(name, [{name: 'root', rename: `alias_${index}`, path: absolute('rust/sdk/root'),
      kind: index === 1 ? 'dev' : index === 2 ? 'build' : null,
      target: 'cfg(windows)', optional: index === 3}]))];
  assert.deepEqual(reverseConsumers(packages, ['root']), ['build', 'dev', 'normal', 'optional', 'root']);
});

test('Shared reverseConsumers never expands a caller-filtered historical package/lane domain', () => {
  const packages = [pkg('core'), pkg('default-consumer', ['core', 'excluded-native'])];
  assert.deepEqual(reverseConsumers(packages, ['core']), ['core', 'default-consumer']);
  assert.deepEqual(reverseConsumers(packages, ['default-consumer']), ['default-consumer']);
  assert.deepEqual(reverseConsumers(packages, []), []);
});

test('Shared reverseConsumers accepts normalized dependency names and excludes registry collisions', () => {
  const packages = [{name: 'core', dependencies: []}, {name: 'middle', dependencies: ['core']},
    {name: 'last', dependencies: [{name: 'middle', rename: 'alias'}]},
    {name: 'registry', dependencies: [{name: 'core', source: 'registry+example'}]}];
  assert.deepEqual(reverseConsumers(packages, ['core']), ['core', 'last', 'middle']);
});

test('Shared reverseConsumers rejects cycles, duplicates, unknown seeds and path/name mismatches', () => {
  assert.throws(() => reverseConsumers([{name: 'cycle', dependencies: ['cycle']}], ['cycle']), /cycle/);
  assert.throws(() => reverseConsumers([pkg('core'), pkg('core')], []), /Duplicate consumer/);
  assert.throws(() => reverseConsumers([pkg('core')], ['missing']), /Unknown consumer seed/);
  assert.throws(() => reverseConsumers([pkg('core'), pkg('consumer', [{
    name: 'wrong', path: absolute('rust/sdk/core'),
  }])], ['core']), /path dependency name mismatch/);
});

test('Workspace/root/any nested Cargo, toolchain, workflow and planner changes require full', () => {
  for (const file of ['rust/Cargo.toml', 'rust/Cargo.lock', 'Cargo.toml',
    'rust/sdk/deployment/Cargo.toml', 'rust/rust-toolchain.toml', '.cargo/config.toml',
    '.github/workflows/check.yml', 'rust/validation/impact-graph.json',
    'rust/scripts/impact-plan.test.cjs', 'rust/scripts/rust-fast-check.cjs']) {
    const result = select([file]);
    assert.equal(result.mode, 'full', file);
    assert.equal(result.packages.length, 5);
    assert.equal(result.tests.length, 5);
    assert.equal(result.checks.length, 5);
  }
});

test('Unknown source, fixtures, templates, scripts and even non-Rust shared inputs fail closed', () => {
  for (const file of ['rust/new-crate/src/lib.rs', 'rust/tests/new.rs', 'rust/fixtures/unknown.json',
    'rust/templates/example.txt', 'rust/scripts/unknown.py', 'src/fabric_cli/new.py']) {
    const result = select([file]);
    assert.equal(result.mode, 'full', file);
    assert.match(result.reason, /Unknown shared input/);
  }
});

test('Documentation-only changes produce no commands, profiles or checks', () => {
  const result = select(['README.md', 'AGENTS.md', 'rust/README.md', 'rust/AGENTS.md',
    'rust/docs/architecture.md', 'rust/docs/decisions/new.md']);
  assert.equal(result.mode, 'none');
  assert.deepEqual(result.packages, []);
  assert.deepEqual(commands(result, 'win32'), {commands: [], checks: [], skipped: []});
});

test('Declared noncode inputs run component closures; check inputs run their own dependency closure', () => {
  const component = select(['rust/templates/notebook.json'], input => input.graph.components[0]
    .inputs.push('rust/templates/*.json'));
  assert.deepEqual(testKeys(component), ['adapter/test:publish', 'cli/test:mount', 'deploy/test:notebook']);
  const check = select(['rust/fixtures/schema/changed.json']);
  assert.equal(check.mode, 'affected');
  assert.deepEqual(check.checks, ['fixture-contract', 'fixture-setup']);
  assert.deepEqual(check.tests, []);
  assert.deepEqual(check.packages, ['adapter', 'cli', 'deploy', 'models']);
  const script = select(['rust/scripts/source_contract.py'], input => input.graph.checks[3]
    .inputs.push('rust/scripts/source_contract.py'));
  assert.notEqual(script.mode, 'full');
});

function nativeProfile(input) {
  const cli = input.metadata.packages.find(item => item.name === 'cli');
  cli.features = {default: [], native: [], combined: ['native']};
  cli.targets.push(target('native', 'test', 'rust/frontends/cli/app/tests/native.rs', ['native']));
  input.graph.components.push({id: 'native', package: 'cli', inputs: ['rust/frontends/cli/app/src/native.rs'],
    tests: [{package: 'cli', target: 'test:native'}]});
  input.graph.profiles.push({id: 'native-windows', packages: ['cli'], features: ['cli/combined'],
    platforms: ['windows'], checks: ['native-contract']});
  input.graph.checks.push({id: 'native-contract', platforms: ['windows']});
}

test('Required-feature target is explicitly skipped by default and covered by platform profile', () => {
  const result = select(['rust/frontends/cli/app/src/native.rs'], nativeProfile);
  assert.equal(result.tests[0].default_available, false);
  assert.deepEqual(result.tests[0].profiles, ['native-windows']);
  const catalog = result.target_catalog.find(item => item.target === 'test:native');
  assert.deepEqual(catalog.required_features, ['native']);
  assert.equal(catalog.default_available, false);
  assert.deepEqual(catalog.profiles, ['native-windows']);
  const linux = commands(result, 'linux'), windows = commands(result, 'windows');
  assert.equal(linux.commands.filter(item => item.kind === 'test').length, 0);
  assert.ok(linux.skipped.some(item => item.profile === 'native-windows'));
  assert.ok(linux.skipped.some(item => item.check === 'native-contract'));
  assert.equal(windows.commands.filter(item => item.kind === 'test').length, 1);
  assert.ok(windows.commands.find(item => item.kind === 'test').args.includes('cli/combined'));
  assert.deepEqual(windows.checks, ['native-contract']);
});

test('Full package tests report required-feature target gaps and include full profiles', () => {
  const result = select(['rust/Cargo.toml'], nativeProfile);
  const cli = result.tests.find(item => item.package === 'cli');
  assert.deepEqual(cli.default_skipped_targets, ['test:native']);
  assert.ok(result.profiles[0].tests.some(item => item.package === 'cli' && item.target === 'all'));
  assert.ok(commands(result, 'linux').skipped.some(item => item.target === 'test:native'));
});

test('Exact targets whose required features are enabled by defaults execute without feature arguments', () => {
  const result = select(['rust/frontends/cli/app/tests/native.rs'], input => {
    nativeProfile(input);
    input.metadata.packages.find(item => item.name === 'cli').features.default = ['combined'];
    input.graph.profiles = [];
  });
  const selected = result.tests[0];
  assert.equal(selected.target, 'test:native');
  assert.deepEqual(selected.required_features, ['native']);
  assert.equal(selected.default_available, true);
  assert.deepEqual(selected.profiles, []);
  const tests = commands(result, 'linux').commands.filter(item => item.kind === 'test');
  assert.equal(tests.length, 1);
  assert.ok(tests[0].args.includes('--test'));
  assert.ok(!tests[0].args.includes('--features'));
});

test('Full all-target plans retain default execution and catalog feature-gated non-test fixture binaries', () => {
  const result = select(['rust/Cargo.toml'], input => {
    nativeProfile(input);
    const fixture = target('fixture', 'bin', 'rust/frontends/cli/app/src/bin/fixture.rs', ['native']);
    fixture.test = false;
    input.metadata.packages.find(item => item.name === 'cli').targets.push(fixture);
  });
  const cli = result.tests.find(item => item.package === 'cli');
  assert.equal(cli.target, 'all');
  assert.equal(cli.default_available, true);
  const fixture = result.target_catalog.find(item => item.target === 'bin:fixture');
  assert.deepEqual(fixture.required_features, ['native']);
  assert.equal(fixture.default_available, false);
  assert.equal(fixture.test, false);
  assert.deepEqual(fixture.profiles, ['native-windows']);
  const windows = commands(result, 'windows');
  assert.ok(windows.commands.some(item => item.kind === 'test' && item.package === 'cli' && !item.profile));
  assert.ok(windows.commands.some(item => item.kind === 'check' &&
    item.profile === 'native-windows' && item.args.includes('--all-targets')));
});

test('Feature profiles are relevant by compile intersection, but tests stay in explicitly modeled packages', () => {
  const result = select(['rust/sdk/deployment/src/notebook.rs'], input => {
    input.metadata.packages.find(item => item.name === 'cli').features.native = [];
    input.graph.profiles = [{id: 'cli-native', packages: ['cli'], features: ['cli/native']}];
  });
  assert.equal(result.profiles.length, 1);
  assert.deepEqual(result.profiles[0].tests.map(item => item.package), ['cli']);
  assert.deepEqual(testKeys(result), ['adapter/test:publish', 'cli/test:mount', 'deploy/test:notebook']);
});

test('Feature defaults are scoped to test invocation, not every workspace consumer', () => {
  const input = fixture();
  const models = input.metadata.packages.find(item => item.name === 'models');
  models.features.special = [];
  models.targets.push(target('special', 'test', 'rust/sdk/models/tests/special.rs', ['special']));
  const deploy = input.metadata.packages.find(item => item.name === 'deploy');
  deploy.features.default = ['models/special'];
  input.files = ['rust/sdk/models/tests/special.rs'];
  assert.throws(() => plan(input), /no applicable profile/);
});

test('Missing required-feature coverage fails planning, never declares clean', () => {
  assert.throws(() => select(['rust/frontends/cli/app/src/native.rs'], input => {
    nativeProfile(input);
    input.graph.profiles = [];
  }), /no applicable profile/);
});

test('Discovery binary has an explicit cross-platform profile and combined coverage', () => {
  const graph = JSON.parse(fs.readFileSync(
    path.join(__dirname, '..', 'validation', 'impact-graph.json'), 'utf8'));
  const discovery = graph.profiles.find(item => item.id === 'v2-discovery');
  assert.ok(discovery);
  assert.deepEqual(discovery.platforms, ['windows', 'linux', 'macos']);
  assert.ok(graph.profiles.find(item => item.id === 'combined').features
    .includes('fabric-cli/v2-discovery'));
  const input = {
    root: ROOT,
    metadata: metadata([pkg('fabric-cli', [], [
      target('fab-cli', 'bin', 'rust/crates/fabric-cli/src/bin/fab_cli.rs', ['v2-discovery']),
    ], {default: [], 'v2-discovery': []})]),
    graph: {schema: 1, profiles: [discovery]},
    files: ['rust/Cargo.toml'],
  };
  const result = plan(input);
  const binary = result.target_catalog.find(item => item.target === 'bin:fab-cli');
  assert.equal(binary.default_available, false);
  assert.deepEqual(binary.profiles, ['v2-discovery']);
  for (const platform of discovery.platforms) {
    assert.ok(commands(result, platform).commands.some(item =>
      item.profile === 'v2-discovery' && item.args.includes('--all-targets')));
  }
});

test('Weak dependency features do not activate an otherwise disabled optional dependency', () => {
  const input = fixture();
  const models = pkg('models', [], [target('special', 'test',
    'rust/sdk/models/tests/special.rs', ['special'])], {default: [], special: []});
  const consumer = pkg('consumer', [{name: 'models', rename: 'renamed',
    path: absolute('rust/sdk/models'), optional: true}],
  [], {default: ['renamed?/special'], enable: ['dep:renamed']});
  input.metadata = metadata([models, consumer]);
  input.graph = {schema: 1, profiles: [{id: 'weak', packages: ['models', 'consumer'],
    features: ['consumer/default']}]};
  input.files = ['rust/sdk/models/tests/special.rs'];
  assert.throws(() => plan(input), /no applicable profile/);
  input.graph.profiles[0].features.push('consumer/enable');
  const result = plan(input);
  assert.equal(result.tests.find(item => item.package === 'models').default_available, false);
  assert.deepEqual(result.tests.find(item => item.package === 'models').profiles, ['weak']);
});

test('Invalid graph schemas/fields, duplicates, dangling references and cycles are errors even with no files', () => {
  const cases = [
    [input => input.graph.schema = 2, /schema/],
    [input => input.graph.copyright = 1, /copyright/],
    [input => input.graph.components = null, /must be an array/],
    [input => input.graph.checks[0].packages = null, /must be an array/],
    [input => input.graph.command = 'arbitrary', /unknown fields/],
    [input => input.graph.components[0].commands = [], /unknown fields/],
    [input => input.graph.components[0].coverage = 'all', /Unknown component coverage/],
    [input => input.graph.components.push(clone(input.graph.components[0])), /Duplicate component/],
    [input => input.graph.checks.push(clone(input.graph.checks[0])), /Duplicate check/],
    [input => input.graph.components[0].depends_on = ['missing'], /Dangling component/],
    [input => input.graph.components[0].depends_on = ['notebook-mount'], /component cycle/],
    [input => input.graph.checks[0].depends_on = ['missing'], /Dangling check/],
    [input => input.graph.checks[0].depends_on = ['notebook-oracle'], /check cycle/],
    [input => input.graph.components[0].checks = ['missing'], /Unknown check/],
    [input => input.graph.checks[0].components = ['missing'], /Unknown check component/],
    [input => input.graph.components[0].package = 'missing', /Unknown component package/],
    [input => input.graph.components[0].tests[0].target = 'test:missing', /Unknown test target/],
    [input => input.graph.components[0].tests[0].filter = '', /Invalid test filter/],
    [input => input.graph.components[0].inputs = ['../escape/**'], /Noncanonical/],
    [input => input.graph.components[0].inputs = ['rust/[ab].rs'], /Unsupported glob/],
    [input => input.graph.checks[0].platforms = ['android'], /platforms/],
    [input => input.graph.checks[0].platforms = [], /platforms/],
  ];
  for (const [change, expected] of cases) assert.throws(() => select([], change), expected);
});

test('Graph JSON accepts an optional Microsoft copyright string without JSON comments', () => {
  const result = select(['rust/sdk/deployment/src/notebook.rs'], input => {
    input.graph.copyright = 'Copyright (c) Microsoft Corporation. Licensed under the MIT License.';
  });
  assert.equal(result.mode, 'affected');
  assert.deepEqual(testKeys(result), ['adapter/test:publish', 'cli/test:mount', 'deploy/test:notebook']);
});

test('Invalid feature declarations/profile strings and profile duplicates are rejected', () => {
  const cases = [
    [input => input.metadata.packages[0].features.bad = ['unknown'], /Unknown local feature/],
    [input => input.metadata.packages[0].features.bad = ['dep:unknown'], /Unknown feature dependency/],
    [input => input.metadata.packages[0].features.bad = ['unknown/feature'], /Unknown dependency feature/],
    [input => input.metadata.packages[1].features.bad = ['models/missing'], /Unknown internal dependency feature/],
    [input => input.graph.profiles = [{id: 'bad', packages: ['cli'], features: ['cli/unknown']}], /Unknown profile feature/],
    [input => input.graph.profiles = [{id: 'bad', packages: ['cli'], features: ['models/default']}], /missing from profile/],
    [input => input.graph.profiles = [{id: 'same', packages: ['cli']}, {id: 'same', packages: ['cli']}], /Duplicate profile/],
    [input => input.metadata.packages[0].targets[0]['required-features'] = ['unknown'], /Unknown required feature/],
  ];
  for (const [change, expected] of cases) assert.throws(() => select([], change), expected);
});

test('External/dangling/mismatched path dependencies, invalid ownership and Cargo cycles fail closed', () => {
  const cases = [
    [input => input.metadata.packages[1].dependencies[0].path = path.resolve(ROOT, '..', 'escape'), /Noncanonical/],
    [input => input.metadata.packages[1].dependencies[0].path = absolute('rust/missing'), /Dangling workspace/],
    [input => input.metadata.packages[1].dependencies[0].name = 'alias-not-actual', /name mismatch/],
    [input => input.metadata.packages[0].dependencies.push({name: 'cli', path: absolute('rust/frontends/cli/app')}), /Cargo dependency cycle/],
    [input => input.metadata.packages.push(clone(input.metadata.packages[0])), /Duplicate metadata/],
    [input => input.metadata.workspace_members.push('missing'), /Missing workspace member/],
    [input => input.metadata.packages[0].manifest_path = path.resolve(ROOT, '..', 'Cargo.toml'), /Noncanonical/],
    [input => input.metadata.version = 2, /format version/],
  ];
  for (const [change, expected] of cases) assert.throws(() => select([], change), expected);
});

test('Changed paths reject traversal, absolute paths and noncanonical spellings', () => {
  for (const file of ['../escape.rs', '/absolute.rs', 'C:/absolute.rs', 'rust\\file.rs',
    './rust/file.rs', 'rust//file.rs', 'rust/../file.rs', 'rust/file.rs\n']) {
    assert.throws(() => select([file]), /Noncanonical/, file);
  }
});

test('Literal Git filename metacharacters are data, not graph globs or shell expressions', () => {
  const file = 'tests/recordings/failure[.capacities-test_data-/.capacities/{}.Capacity].yaml';
  const result = select([file]);
  assert.equal(result.mode, 'full');
  assert.deepEqual(result.files, [file]);
  const wildcard = select(['rust/sdk/deployment/tests/literal[*?].rs']);
  assert.deepEqual(testKeys(wildcard), ['adapter/all', 'cli/all', 'deploy/all']);
});

test('Fingerprints are stable under input/package ordering and change with graph/data', () => {
  const input = fixture();
  input.files = ['rust/sdk/deployment/src/notebook.rs', 'README.md'];
  const first = plan(input);
  input.files.reverse();
  input.metadata.packages.reverse();
  const reordered = plan(input);
  assert.equal(first.fingerprint, reordered.fingerprint);
  input.graph.checks[0].platforms = ['windows'];
  assert.notEqual(first.fingerprint, plan(input).fingerprint);
  assert.equal(first.fingerprint.length, 64);
});

test('Canonical selections do not depend on host locale collation', () => {
  const original = String.prototype.localeCompare;
  try {
    String.prototype.localeCompare = () => { throw new Error('Locale collation must not select validation'); };
    const result = select(['rust/sdk/deployment/src/notebook.rs']);
    assert.deepEqual(testKeys(result), ['adapter/test:publish', 'cli/test:mount', 'deploy/test:notebook']);
  } finally { String.prototype.localeCompare = original; }
});

test('Fingerprints bind exact planner source and the complete metadata snapshot', () => {
  const input = fixture(), first = plan(input);
  const source = fs.readFileSync(path.join(__dirname, 'impact-plan.cjs'));
  assert.equal(first.script_digest, crypto.createHash('sha256').update(source).digest('hex'));
  input.metadata.packages[0].description = 'Changed data outside dependency graph';
  const next = plan(input);
  assert.equal(first.metadata_digest, next.metadata_digest);
  assert.notEqual(first.metadata_input_digest, next.metadata_input_digest);
  assert.notEqual(first.fingerprint, next.fingerprint);
});

test('File content snapshots prevent unchanged-HEAD fingerprints from hiding dirty content changes', () => {
  const input = fixture();
  input.root = path.resolve(__dirname, '..', '..');
  input.files = ['rust/scripts/impact-plan.test.cjs'];
  const first = plan(input), readSync = fs.readSync;
  let modified;
  try {
    fs.readSync = (...args) => {
      const count = readSync(...args);
      if (count) args[1][args[2]] ^= 1;
      return count;
    };
    modified = plan(input);
  } finally { fs.readSync = readSync; }
  assert.equal(first.file_snapshot[0].state, 'file');
  assert.notEqual(first.file_snapshot[0].digest, modified.file_snapshot[0].digest);
  assert.notEqual(first.fingerprint, modified.fingerprint);
  assert.equal(first.metadata_input_digest, modified.metadata_input_digest);
});

function cliFixture({files = ['rust/sdk/deployment/src/notebook.rs'], dirty = [], untracked = [],
  tracked = ['rust/sdk/deployment/tests/common/mod.rs'],
  unavailable = false, env = {}} = {}) {
  const input = fixture(), calls = [], writes = new Map();
  const base = '1'.repeat(40), head = '2'.repeat(40);
  const paths = {metadata: absolute('metadata.json'), graph: absolute('graph.json'),
    files: absolute('files.json'), output: absolute('plan.json'), github: absolute('github.txt'),
    summary: absolute('summary.md')};
  const read = file => {
    if (file === paths.metadata) return JSON.stringify(input.metadata);
    if (file === paths.graph || file === absolute('rust/validation/impact-graph.json')) return JSON.stringify(input.graph);
    if (file === paths.files) return JSON.stringify(files);
    throw new Error(`Unexpected read: ${file}`);
  };
  const exec = (program, args, cwd) => {
    calls.push({program, args, cwd});
    assert.equal(program, 'git', 'Unit suite must never run Cargo');
    assert.equal(cwd, ROOT);
    if (args[0] === 'rev-parse') return `${head}\n`;
    if (args[0] === 'cat-file') {
      if (unavailable && args[2].startsWith(base)) throw new Error('Unavailable commit');
      return '';
    }
    if (args[0] === 'ls-files') {
      const values = args.includes('--cached') ? tracked : untracked;
      return values.join('\0') + (values.length ? '\0' : '');
    }
    if (args[0] === 'diff') {
      assert.ok(args.includes('--no-renames'));
      assert.ok(args.includes('-z'));
      assert.equal(args.at(-1), '--');
      const values = args.includes(base) ? files : dirty;
      return values.join('\0') + (values.length ? '\0' : '');
    }
    throw new Error(`Unexpected command: ${args.join(' ')}`);
  };
  return {base, head, paths, calls, writes, hooks: {exec, read, env,
    write: (file, content, append = false) => writes.set(file, (append ? writes.get(file) || '' : '') + content)},
    args: ['--root', ROOT, '--metadata', paths.metadata, '--graph', paths.graph, '--output', paths.output]};
}

test('CLI PR and local dirty discovery use one shared planner and agree with explicit file JSON', () => {
  const ci = cliFixture({env: {CI: 'true'}});
  const pr = main([...ci.args, '--base', ci.base, '--head', ci.head,
    '--github-output', ci.paths.github, '--summary', ci.paths.summary], ci.hooks);
  const local = cliFixture({dirty: pr.files});
  const localPlan = main(local.args, local.hooks);
  assert.equal(pr.mode, localPlan.mode);
  assert.deepEqual(pr.packages, localPlan.packages);
  assert.deepEqual(pr.tests, localPlan.tests);
  const explicit = cliFixture({dirty: pr.files});
  const explicitPlan = main([...explicit.args, '--files-json', explicit.paths.files], explicit.hooks);
  assert.notEqual(explicitPlan.fingerprint, localPlan.fingerprint);
  assert.deepEqual(explicitPlan.tests, localPlan.tests);
  assert.equal(explicitPlan.synthetic_scope, true);
  assert.equal(explicitPlan.head_complete, false);
  assert.equal(localPlan.head_complete, true);
  assert.match(ci.writes.get(ci.paths.github), /mode=affected\n/);
  assert.match(ci.writes.get(ci.paths.summary), /Rust dependency impact/);
  assert.equal(JSON.parse(ci.writes.get(ci.paths.output)).fingerprint, pr.fingerprint);
  assert.equal(pr.base, ci.base);
  assert.equal(pr.head, ci.head);
  assert.equal(localPlan.dirty, true);
});

test('CLI local discovery includes staged/unstaged tracked and untracked names with NUL separators', () => {
  const fixture = cliFixture({dirty: ['rust/sdk/deployment/src/notebook.rs'],
    untracked: ['rust/sdk/deployment/src/spark.rs']});
  const result = main(fixture.args, fixture.hooks);
  assert.deepEqual(result.files, ['rust/sdk/deployment/src/notebook.rs', 'rust/sdk/deployment/src/spark.rs']);
  assert.deepEqual(result.dirty_files, result.files);
  assert.deepEqual(result.dirty_snapshot.map(item => item.path), result.files);
  assert.equal(result.dirty, true);
});

test('CLI accepts PowerShell UTF-8 BOMs in metadata, graph and changed-files JSON', () => {
  const fixture = cliFixture();
  const read = fixture.hooks.read;
  fixture.hooks.read = file => `\uFEFF${read(file)}`;
  const result = main([...fixture.args, '--files-json', fixture.paths.files], fixture.hooks);
  assert.equal(result.mode, 'affected');
  assert.deepEqual(result.changed_packages, ['deploy']);
});

test('CLI missing CI, zero and unavailable bases select full rather than clean', () => {
  const missing = cliFixture({env: {GITHUB_ACTIONS: 'true'}, files: []});
  assert.equal(main(missing.args, missing.hooks).mode, 'full');
  const zero = cliFixture({files: []});
  assert.equal(main([...zero.args, '--base', '0'.repeat(40)], zero.hooks).mode, 'full');
  const unavailable = cliFixture({files: [], unavailable: true});
  const result = main([...unavailable.args, '--base', unavailable.base], unavailable.hooks);
  assert.equal(result.mode, 'full');
  assert.match(result.reason, /unavailable/);
  assert.ok(!unavailable.calls.some(call => call.args[0] === 'diff' && call.args.includes(unavailable.base)));
});

test('CLI explicit full preserves all checks and local clean checkout remains none', () => {
  const fixture = cliFixture({files: []});
  assert.equal(main(fixture.args, fixture.hooks).mode, 'none');
  const full = main([...fixture.args, '--full'], fixture.hooks);
  assert.equal(full.mode, 'full');
  assert.equal(full.checks.length, 5);
});

test('CLI inventory freezes tracked helpers and all untracked sources, not only changed-file paths', () => {
  const fixture = cliFixture({untracked: ['rust/sdk/deployment/src/new.rs']});
  const result = main(fixture.args, fixture.hooks);
  assert.deepEqual(result.inputs.inventory.files.map(item => item.path),
    ['rust/sdk/deployment/src/new.rs', 'rust/sdk/deployment/tests/common/mod.rs']);
  assert.equal(result.inputs.inventory.scope, 'git-tracked-and-untracked');
  assert.equal(result.inputs.schema, 1);
  assert.equal(result.inputs.root, ROOT);
  assert.equal(result.inputs.head, fixture.head);
  assert.equal(result.inputs.script_digest, result.script_digest);
});

test('verifyPlan validates CLI receipt integrity, HEAD, graph, metadata and source inventory without Cargo', () => {
  const fixture = cliFixture();
  const result = main(fixture.args, fixture.hooks);
  const verified = verifyPlan(result, {root: ROOT, exec: fixture.hooks.exec, read: fixture.hooks.read});
  assert.equal(verified.valid, true);
  assert.equal(verified.head_complete, true);
  const modified = clone(result);
  modified.mode = 'full';
  assert.throws(() => verifyPlan(modified, {root: ROOT}), /fingerprint mismatch/);
  assert.throws(() => verifyPlan(result, {root: path.dirname(ROOT)}), /Plan root differs/);
  assert.throws(() => verifyPlan(result, {root: ROOT, exec: fixture.hooks.exec,
    read: file => {
      const value = JSON.parse(fixture.hooks.read(file));
      if (file === fixture.paths.graph) value.schema = 2;
      return JSON.stringify(value);
    }}), /Impact graph changed/);
  assert.throws(() => verifyPlan(result, {root: ROOT, exec: fixture.hooks.exec,
    read: file => {
      const value = JSON.parse(fixture.hooks.read(file));
      if (file === fixture.paths.metadata) value.changed = true;
      return JSON.stringify(value);
    }}), /Cargo metadata input changed/);
  assert.throws(() => verifyPlan(result, {root: ROOT, read: fixture.hooks.read,
    exec: (program, args, cwd) => args[0] === 'rev-parse' ? '3'.repeat(40) :
      fixture.hooks.exec(program, args, cwd)}), /Checkout HEAD changed/);
});

test('verifyPlan rejects newly added untracked candidate inputs', () => {
  const untracked = [], fixture = cliFixture({untracked});
  const result = main(fixture.args, fixture.hooks);
  untracked.push('rust/sdk/deployment/tests/new_helper.rs');
  assert.throws(() => verifyPlan(result, {root: ROOT, exec: fixture.hooks.exec,
    read: fixture.hooks.read}), /source inventory changed/);
});

test('verifyPlan rejects changed dirty file bytes without a HEAD change', () => {
  const input = fixture();
  input.root = path.resolve(__dirname, '..', '..');
  input.files = ['rust/scripts/impact-plan.test.cjs'];
  const result = plan(input), target = path.join(__dirname, 'impact-plan.test.cjs');
  const openSync = fs.openSync, readSync = fs.readSync, closeSync = fs.closeSync, descriptors = new Set();
  assert.equal(verifyPlan(result).valid, true);
  try {
    fs.openSync = (file, ...args) => {
      const descriptor = openSync(file, ...args);
      if (String(file) === target) descriptors.add(descriptor);
      return descriptor;
    };
    fs.readSync = (...args) => {
      const count = readSync(...args);
      if (count && descriptors.has(args[0])) args[1][args[2]] ^= 1;
      return count;
    };
    fs.closeSync = descriptor => {
      descriptors.delete(descriptor);
      return closeSync(descriptor);
    };
    assert.throws(() => verifyPlan(result), /source inventory changed/);
  } finally { fs.openSync = openSync; fs.readSync = readSync; fs.closeSync = closeSync; }
});

test('Synthetic API plans and their verifier never invoke Git or claim HEAD-complete evidence', () => {
  const result = select(['rust/sdk/deployment/src/notebook.rs']);
  const verified = verifyPlan(result, {root: ROOT, exec: () => { throw new Error('Must not execute'); }});
  assert.equal(verified.valid, true);
  assert.equal(verified.synthetic_scope, true);
  assert.equal(verified.head_complete, false);
});

test('Synthetic CLI receipts still verify HEAD and all source names without claiming complete selection', () => {
  const fixture = cliFixture();
  const cliPlan = main([...fixture.args, '--files-json', fixture.paths.files], fixture.hooks);
  const cliVerified = verifyPlan(cliPlan, {root: ROOT, read: fixture.hooks.read,
    exec: fixture.hooks.exec});
  assert.equal(cliVerified.head_complete, false);
  assert.throws(() => verifyPlan(cliPlan, {root: ROOT, read: fixture.hooks.read,
    exec: (program, args, cwd) => args[0] === 'rev-parse' ? '3'.repeat(40) :
      fixture.hooks.exec(program, args, cwd)}), /Checkout HEAD changed/);
});

test('CLI rejects a requested head different from the actual checkout', () => {
  const fixture = cliFixture();
  assert.throws(() => main([...fixture.args, '--head', '3'.repeat(40)], fixture.hooks),
    /differs from the checked-out HEAD/);
});

test('Explicit file inputs cannot hide dirty changes or committed PR inputs', () => {
  const fixture = cliFixture({files: ['rust/sdk/deployment/src/notebook.rs'],
    dirty: ['rust/sdk/deployment/src/spark.rs']});
  const result = main([...fixture.args, '--base', fixture.base, '--files-json', fixture.paths.files], fixture.hooks);
  assert.deepEqual(result.files, ['rust/sdk/deployment/src/notebook.rs', 'rust/sdk/deployment/src/spark.rs']);
});

test('Check dependencies execute before dependents and reject impossible platform setups', () => {
  const result = select(['rust/fixtures/schema/changed.json']);
  assert.deepEqual(result.check_order, ['fixture-setup', 'fixture-contract']);
  assert.deepEqual(commands(result, 'linux').checks, ['fixture-setup', 'fixture-contract']);
  assert.throws(() => select([], input => input.graph.checks.find(item =>
    item.id === 'fixture-setup').platforms = ['windows']), /dependency unavailable/);
});

test('CLI rejects unknown/duplicate options, abbreviated/injected SHAs and relative artifact paths before exec', () => {
  const fixture = cliFixture();
  for (const extra of [['--base', 'HEAD'], ['--head', '2'.repeat(40) + ';bad'],
    ['--unknown'], ['--full', '--full'], ['--files-json', '../outside'],
    ['--base'], ['--output', fixture.paths.output]]) {
    assert.throws(() => main([...fixture.args, ...extra], fixture.hooks));
  }
  assert.equal(fixture.calls.length, 0);
});

test('Graph cannot execute commands and test filters remain argv values rather than shell code', () => {
  const result = select(['rust/sdk/deployment/src/notebook.rs'], input => {
    input.graph.components[0].tests[0].filter = 'name; echo nope';
  });
  const command = commands(result, 'linux').commands.find(item =>
    item.kind === 'test' && item.package === 'deploy');
  assert.equal(command.program, 'cargo');
  assert.deepEqual(command.args.slice(-2), ['--', 'name; echo nope']);
  assert.ok(!Object.hasOwn(command, 'shell'));
});

test('native runtime feature tests have explicit CI profiles and cannot pass as empty default targets', () => {
  const root = path.resolve(__dirname, '..', '..');
  const graph = JSON.parse(fs.readFileSync(path.join(root, 'rust/validation/impact-graph.json'), 'utf8'));
  const cases = [
    ['fabric-sdk', 'livy-arrow', 'livy_arrow', 'rust/sdk/client/Cargo.toml'],
    ['fabric-sdk', 'livy-arrow-ipc', 'livy_arrow_ipc', 'rust/sdk/client/Cargo.toml'],
    ['fabric-cli', 'runtime-v2', 'runtime_v2', 'rust/frontends/cli/app/Cargo.toml'],
    ['fabric-sdk-transport', 'notebook-websocket', 'notebook_websocket',
      'rust/sdk/http/transport/Cargo.toml'],
  ];
  for (const [pkg, feature, target, manifestPath] of cases) {
    const profile = graph.profiles.find(item =>
      item.packages.includes(pkg) && item.features.includes(`${pkg}/${feature}`));
    assert.ok(profile, `Missing explicit profile for ${pkg}/${feature}`);
    assert.deepEqual(profile.platforms, ['windows', 'linux', 'macos']);
    assert.ok(graph.components.some(item => item.package === pkg &&
      item.tests.some(test => test.target === `test:${target}`)));
    const manifest = fs.readFileSync(path.join(root, manifestPath), 'utf8');
    const section = manifest.split('[[test]]').find(item => item.includes(`name = "${target}"`));
    assert.ok(section, `Missing Cargo test declaration for ${target}`);
    assert.ok(section.includes(`required-features = ["${feature}"]`));
  }
  for (const [pkg, target] of [
    ['fabric-sdk', 'notebook_runtime'], ['fabric-sdk', 'notebook_observability'],
    ['fabric-sdk', 'livy_native'], ['fabric-sdk-http', 'native_runtime'],
  ]) {
    assert.ok(graph.components.some(item => item.package === pkg &&
      item.tests.some(test => test.target === `test:${target}`)));
  }
});

function relocatedCatalogFixture(files) {
  const locations = [
    ['sdk-capacities', 'rust/sdk/capacities'],
    ['sdk-jobs', 'rust/sdk/jobs'],
    ['deploy-notebook', 'rust/sdk/deployment'],
    ['jobs', 'rust/frontends/cli/commands/jobs'],
    ['core', 'rust/sdk/request-context'],
  ];
  const catalog = JSON.parse(fs.readFileSync(path.join(__dirname, '../validation/impact-graph.json')));
  const components = locations.map(([id]) => {
    const component = catalog.components.find(item => item.id === id);
    assert.ok(component, id);
    return {id, package: component.package, inputs: component.inputs,
      tests: [{package: component.package, target: 'lib'}]};
  });
  const packages = components.map((component, index) => {
    const directory = locations[index][1];
    return {...pkg(component.package), manifest_path: absolute(`${directory}/Cargo.toml`),
      targets: [target(component.package.replaceAll('-', '_'), 'lib', `${directory}/src/lib.rs`)]};
  });
  return {root: ROOT, metadata: metadata(packages),
    graph: {schema: 1, components, profiles: [], checks: []}, files};
}

test('Relocated catalog inputs select capacity, job, deployment, CLI and request-context components', () => {
  for (const [file, component, name] of [
    ['rust/sdk/capacities/src/lib.rs', 'sdk-capacities', 'fabric-sdk-capacities'],
    ['rust/sdk/jobs/src/lib.rs', 'sdk-jobs', 'fabric-sdk-jobs'],
    ['rust/sdk/deployment/src/notebook.rs', 'deploy-notebook', 'fabric-deploy'],
    ['rust/frontends/cli/commands/jobs/src/lib.rs', 'jobs', 'fabric-cli-jobs'],
    ['rust/sdk/request-context/src/lib.rs', 'core', 'fabric-core'],
  ]) {
    const result = plan(relocatedCatalogFixture([file]));
    assert.equal(result.mode, 'affected', file);
    assert.deepEqual(result.components, [component], file);
    assert.deepEqual(result.changed_packages, [name], file);
    assert.deepEqual(testKeys(result), [`${name}/lib`], file);
  }
});

test('Old-path deletion and relocated addition retain both inputs and widen conservatively', () => {
  const files = ['rust/capabilities/jobs/fabric-sdk-jobs/src/lib.rs', 'rust/sdk/jobs/src/lib.rs'];
  const result = plan(relocatedCatalogFixture(files));
  assert.equal(result.mode, 'full');
  assert.deepEqual(result.files, files);
  assert.ok(result.packages.includes('fabric-sdk-jobs'));
  const unknown = plan(relocatedCatalogFixture(['rust/sdk/unclassified/input.json']));
  assert.equal(unknown.mode, 'full');
  assert.match(unknown.reason, /Unknown shared input/);
});

test('Advanced identity and CLI catalog inputs use the current package roots', () => {
  const catalog = JSON.parse(fs.readFileSync(path.join(__dirname, '../validation/impact-graph.json')));
  for (const component of catalog.components) {
    for (const input of component.inputs) {
      assert.doesNotMatch(input, /^rust\/(?:capabilities|hosts|infrastructure)\//, component.id);
    }
  }
  for (const [id, input] of [
    ['identity-interactive', 'rust/sdk/authentication/credentials/src/interactive*.rs'],
    ['identity-macos-helper', 'rust/sdk/authentication/credentials/src/macos_helper.rs'],
    ['identity-native-certificates', 'rust/sdk/authentication/credentials/src/certificate_macos*.rs'],
  ]) {
    const component = catalog.components.find(item => item.id === id);
    assert.ok(component, id);
    assert.ok(component.inputs.includes(input), id);
    assert.ok(component.checks.includes('combined'), id);
  }
});

test('Shared unconfigured-profile helper retains every actual subprocess consumer', () => {
  const catalog = JSON.parse(fs.readFileSync(path.join(__dirname, '../validation/impact-graph.json')));
  const component = catalog.components.find(item => item.id === 'cli-shared-test-helpers');
  assert.ok(component.inputs.includes('rust/frontends/cli/app/tests/support/**'));
  for (const target of ['headless_configuration', 'job_cli_contract',
    'local_mounted', 'native_endpoint_configuration']) {
    assert.ok(component.tests.some(item =>
      item.package === 'fabric-cli' && item.target === `test:${target}`), target);
  }
});

test('real HTTP-to-Arrow recovery boundary is explicitly tested with its forwarded feature', () => {
  const root = path.resolve(__dirname, '..', '..');
  const graph = JSON.parse(fs.readFileSync(path.join(root, 'rust/validation/impact-graph.json'), 'utf8'));
  const profile = graph.profiles.find(item => item.id === 'native-http-arrow');
  assert.ok(profile);
  assert.deepEqual(profile.packages, ['fabric-sdk-http']);
  assert.deepEqual(profile.features, ['fabric-sdk-http/livy-arrow']);
  assert.deepEqual(profile.platforms, ['windows', 'linux', 'macos']);
  assert.ok(graph.components.some(component => component.package === 'fabric-sdk-http' &&
    component.tests.some(test => test.target === 'test:native_stream')));
  const manifest = fs.readFileSync(path.join(root,
    'rust/sdk/http/executor/Cargo.toml'), 'utf8');
  assert.ok(manifest.includes('livy-arrow = ["fabric-sdk/livy-arrow"]'));
});
