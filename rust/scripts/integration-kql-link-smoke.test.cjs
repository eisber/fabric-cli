// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const {test} = require('node:test');
const {plan} = require('./impact-plan.cjs');
const dataagent = require('./integration-dataagent-smoke.cjs');
const update = require('./integration-update-smoke.cjs');
const kql = require('./integration-kql-link-smoke.cjs');

const ROOT = path.resolve(__dirname, '../..');
const read = file => fs.readFileSync(path.join(ROOT, file), 'utf8');
const graph = () => JSON.parse(read('rust/validation/impact-graph.json'));
const features = ({args}) => args.includes('--features') ? args[args.indexOf('--features') + 1].split(',') : [];
const output = (names, filtered = 0) => names.map(name => `test ${name} ... ok\n`).join('') +
  `test result: ok. ${names.length} passed; 0 failed; 0 ignored; 0 measured; ${filtered} filtered out\n`;

test('PR55 proof is candidate-only, immutable-pinned, and retains the existing finite hosted runner', () => {
  const workflow = read('.github/workflows/integration-smoke.yml').replace(/\r\n/g, '\n');
  const block = workflow.split("      - name: Check the PR55 candidate's ")[1].split('\n      - name:')[0];
  const expression = block.match(/\n        if: >-\n((?:          .+\n)+)/)[1]
    .trim().replace(/\s*\n\s*/g, ' ');
  for (const [event, ref, expected] of [
    ['pull_request', kql.ENTRY.candidate, true], ['push', kql.ENTRY.candidate, false],
    ['pull_request', `${kql.ENTRY.candidate}-other`, false], ['schedule', kql.ENTRY.candidate, false],
  ]) {
    assert.equal(vm.runInNewContext(expression, {
      github: {event_name: event, event: {pull_request: {head: {ref}}}},
    }, {timeout: 1000}), expected);
  }
  assert.equal(kql.ENTRY.base, '61c8221914152e231e5e64f7bd38418f66264999');
  assert.equal(kql.ENTRY.source, 'e1baaee16dc6eb4917c5310224c27fcd258564ef');
  assert.match(block, /timeout-minutes: 10/);
  assert.match(block, /integration-kql-link-smoke\.cjs/);
  assert.ok(!block.includes('continue-on-error'));
  for (const binding of ['FAB_CLI_V2_TEST_ROOT', 'FAB_IDENTITY_TEST_ROOT', 'FAB_UPDATE_TEST_ROOT']) {
    assert.ok(block.includes(`${binding}:`));
  }
  assert.ok(workflow.includes('rust/scripts/integration-kql-link-smoke.test.cjs'));
  const runner = read('rust/scripts/integration-dataagent-smoke.cjs');
  assert.match(runner, /entry\.leaves\.map\(entry\.leafCommand \|\| leafCommand\)/);
  for (const guard of ['570000', 'github-hosted', 'metadata', '--offline', '--locked',
    'selection.json', 'selected.deferred', 'source-before.json', 'source-after.json', 'requireUnchanged']) {
    assert.ok(runner.includes(guard));
  }
  assert.match(read(kql.ENTRY.driver), /dataagent\.main\(process\.argv\[2\], ENTRY\)/);
});

test('complete KQL and preserved DataAgent leaves require their current registered cases, including headless process tests', () => {
  for (const name of kql.ENTRY.leaves) {
    const command = kql.ENTRY.leafCommand(name);
    const file = `rust/clients/cli/app/tests/${name}.rs`;
    assert.deepEqual(command.required, update.registeredTests(file));
    assert.ok(command.exhaustive && features(command).includes('headless'));
    assert.equal(features(command).includes('v2-link-kql'), name === kql.KQL_TARGET);
    const stdout = output(command.required);
    const stderr = `Running tests/${name}.rs (target/debug/deps/test.exe)\n`;
    assert.equal(dataagent.testResults(stdout, stderr, command)[0].passed, command.required.length);
    for (const wrong of ['', output([]), stdout.replace('0 ignored', '1 ignored'),
      stdout.replace('0 filtered', '1 filtered'), stdout.replace(command.required[0], 'other')]) {
      assert.throws(() => dataagent.testResults(wrong, stderr, command));
    }
    if (name === kql.KQL_TARGET) {
      const source = read(file);
      assert.match(source, /#!\[cfg\(feature = "v2-link-kql"\)\]/);
      const processNames = [...source.matchAll(
        /#\[cfg\(feature = "headless"\)\]\s*#\[test\]\s*fn\s+(\w+)/g,
      )].map(match => match[1]);
      assert.ok(processNames.length > 0, 'The registered headless process case is required');
      for (const processName of processNames) {
        assert.ok(command.required.includes(processName));
        assert.throws(() => dataagent.testResults(stdout.replace(processName, 'other'), stderr, command));
      }
    }
  }
  assert.deepEqual(features(dataagent.leafCommand(kql.KQL_TARGET)), ['headless'],
    'The existing runner default is unchanged; only the KQL entry overrides it');
});

test('KQL native-adapter and current DataAgent main namespaces use dynamic qualified inventories', () => {
  for (const module of ['v2_link_kql_main', 'v2_link_dataagent_main']) {
    const commands = kql.ENTRY.commands.filter(item => item.args.includes(`${module}::tests::`));
    assert.deepEqual(commands.map(features), [
      ['headless', 'v2-link-kql'], kql.REGISTRATION_PROFILES.at(-1).features,
    ]);
    for (const command of commands) {
      assert.ok(command.exhaustive);
      assert.deepEqual(command.required, update.registeredTests(`rust/clients/cli/app/src/${module}_tests.rs`)
        .map(name => `${module}::tests::${name}`));
      const stdout = output(command.required, 17);
      const stderr = 'Running unittests src/v2_main.rs (target/debug/deps/test.exe)\n';
      const result = dataagent.testResults(stdout, stderr, command)[0];
      assert.equal(result.passed, command.required.length);
      assert.equal(result.filtered, 17, 'Unrelated binary-unit cases remain explicitly filtered');
      for (const wrong of ['', output([]), stdout.replace('0 ignored', '1 ignored'),
        stdout.replace(command.required[0], command.required[0].split('::').at(-1))]) {
        assert.throws(() => dataagent.testResults(wrong, stderr, command));
      }
      assert.throws(() => dataagent.testResults(stdout, stderr.replace('v2_main', 'main'), command));
    }
  }
});

test('shared mutation, wire and SDK LINK suites require every currently registered source test', () => {
  for (const target of kql.CONTRACT_TARGETS) {
    const command = kql.ENTRY.commands.find(item => item.args.includes(target.package) &&
      item.args.includes(target.target));
    assert.ok(command?.exhaustive);
    assert.deepEqual(command.required, update.registeredTests(target.source));
    assert.ok(kql.ENTRY.inputs.includes(target.source));
    const stdout = output(command.required);
    const stderr = `Running tests/${target.target}.rs (target/debug/deps/test.exe)\n`;
    assert.equal(dataagent.testResults(stdout, stderr, command)[0].passed, command.required.length);
    for (const wrong of ['', output([]), stdout.replace(command.required[0], 'other'),
      stdout.replace('0 ignored', '1 ignored'), stdout.replace('0 filtered', '1 filtered')]) {
      assert.throws(() => dataagent.testResults(wrong, stderr, command));
    }
  }
});

test('default, headless, registration-only and combined refresh profiles get actual check and strict Clippy', () => {
  const commands = kql.registrationCommands();
  assert.equal(commands.length, 2 * kql.REGISTRATION_PROFILES.length);
  assert.deepEqual(kql.REGISTRATION_PROFILES.map(item => item.features), [
    [], ['headless'], ['v2-link-kql'], ['headless', 'runtime-v2', 'v2-link-kql', 'v2-refresh'],
  ]);
  for (const profile of kql.REGISTRATION_PROFILES) {
    const selected = commands.filter(command => JSON.stringify(features(command)) === JSON.stringify(profile.features));
    assert.deepEqual(selected.map(command => command.args[0]), ['check', 'clippy']);
    for (const {args} of selected) {
      for (const flag of ['--offline', '--locked', '--all-targets', '--no-default-features']) {
        assert.ok(args.includes(flag));
      }
      assert.ok(!args.includes('--all-features') && !args.includes('--workspace'));
      if (args[0] === 'clippy') assert.deepEqual(args.slice(-3), ['--', '-D', 'warnings']);
    }
  }
  const component = graph().components.find(item => item.id === 'cli-kql-link');
  for (const id of ['v2-link-kql', 'v2-link-kql-headless', 'v2-link-kql-refresh-headless']) {
    assert.ok(component.profiles.includes(id));
    const profile = graph().profiles.find(item => item.id === id);
    assert.ok(profile.features.includes('fabric-cli/v2-link-kql'));
  }
  const combined = graph().profiles.find(item => item.id === 'v2-link-kql-refresh-headless');
  assert.deepEqual(combined.features.map(item => item.slice('fabric-cli/'.length)).sort(),
    kql.REGISTRATION_PROFILES.at(-1).features);
});

// TEST ONLY: synthetic metadata for four real package/target interfaces. Hosted
// proof instead captures Cargo's locked metadata for the complete workspace.
function fixture() {
  const catalog = graph();
  const ids = ['sdk-items', 'sdk-items-link', 'mutations', 'mutations-link', 'sdk',
    'cli-dataagent-list', 'cli-dataagent-link', 'cli-kql-link'];
  const components = catalog.components.filter(item => ids.includes(item.id)).map(item => ({
    ...item, depends_on: (item.depends_on || []).filter(id => ids.includes(id)), checks: [],
  }));
  assert.equal(components.length, ids.length);
  const locations = [
    ['fabric-sdk-items', 'rust/engine/control-plane/resources/items'],
    ['fabric-cli-mutations', 'rust/clients/cli/commands/mutations'],
    ['fabric-sdk', 'rust/engine/sdk'],
    ['fabric-cli', 'rust/clients/cli/app'],
  ];
  const packages = locations.map(([name, directory]) => {
    const manifest = read(`${directory}/Cargo.toml`);
    const features = Object.fromEntries([...((manifest.split('[features]')[1] || '').split(/^\[/m)[0])
      .matchAll(/^([\w-]+)\s*=/gm)].map(match => [match[1], []]));
    const targets = [...new Set(components.flatMap(item => item.tests)
      .filter(item => item.package === name).map(item => item.target))].map(target => {
      const [kind, targetName] = target.split(':');
      const section = manifest.split(`[[${kind}]]`).find(part =>
        part.match(new RegExp(`^name = "${targetName}"$`, 'm')));
      const source = kind === 'lib' ? 'src/lib.rs' :
        section?.match(/^path = "([^"]+)"$/m)?.[1] || `tests/${targetName}.rs`;
      assert.ok(fs.existsSync(path.join(ROOT, directory, source)));
      return {name: targetName || name, kind: [kind], test: true,
        src_path: path.join(ROOT, directory, source),
        'required-features': JSON.parse(section?.match(/^required-features = (\[[^\n]+\])$/m)?.[1] || '[]')};
    });
    const dependencies = locations.filter(([dependency]) => dependency !== name &&
      new RegExp(`^${dependency}(?:\\.workspace)?\\s*=`, 'm').test(manifest)).map(([dependency, location]) => ({
      name: dependency, path: path.join(ROOT, location), kind: null, optional: false, target: null,
    }));
    return {id: name, name, manifest_path: path.join(ROOT, directory, 'Cargo.toml'),
      dependencies, features, targets};
  });
  const profiles = catalog.profiles.filter(item => item.packages.every(name =>
    locations.some(([packageName]) => packageName === name)) && item.features.every(feature =>
    locations.some(([packageName]) => feature.startsWith(`${packageName}/`))))
    .map(item => ({...item, checks: []}));
  return {root: ROOT, graph: {schema: 1, components, profiles, checks: []},
    metadata: {version: 1, packages, workspace_members: packages.map(item => item.id)}};
}

test('real KQL and shared-input registrations preserve named leaves through Cargo reverse-consumer planning', () => {
  const input = fixture();
  const shared = kql.ENTRY.inputs.filter(file => /(?:v2_link(?:_wire)?|items\/tests\/links)\.rs$/.test(file));
  assert.equal(shared.length, 3);
  const kqlInputs = kql.ENTRY.inputs.filter(file =>
    /v2_link_(?:kql_(?:host|main|main_tests)|dispatch)\.rs$/.test(file));
  assert.equal(kqlInputs.length, 5);
  assert.ok(kqlInputs.includes('rust/clients/cli/app/src/v2_link_dispatch.rs'));
  for (const files of [shared, ...kqlInputs.map(file => [file])]) {
    const selection = plan({...input, files});
    assert.equal(selection.mode, 'affected');
    const selected = dataagent.selectedCommands(selection, undefined, input.graph, kql.ENTRY.leaves);
    for (const name of kql.ENTRY.leaves) {
      assert.ok(selected.commands.some(item => item.args.includes(name)), `Missing actual leaf ${name}`);
    }
    const kqlCommand = selected.commands.find(item => item.args.includes(kql.KQL_TARGET));
    assert.ok(features(kqlCommand).includes('v2-link-kql') && features(kqlCommand).includes('headless'));
    for (const [packageName, target] of [
      ['fabric-cli-mutations', 'v2_link'], ['fabric-cli-mutations', 'v2_link_wire'],
      ['fabric-sdk-items', 'links'], ['fabric-sdk', 'kql_metadata'], ['fabric-sdk', 'dataagents'],
    ]) {
      assert.ok(selection.tests.some(item => item.package === packageName && item.target === `test:${target}`));
    }
    for (const {name} of input.metadata.packages) {
      assert.ok(!selection.tests.some(item => item.package === name && item.target === 'all'));
    }
    const broken = structuredClone(input.graph);
    for (const component of broken.components) component.inputs = component.inputs?.filter(file => !files.includes(file));
    const fallback = plan({...input, graph: broken, files});
    assert.throws(() => dataagent.selectedCommands(fallback, undefined, broken, kql.ENTRY.leaves), /leaf selection/);
  }
  for (const dependency of ['mutations-link', 'sdk-items-link', 'operations', 'cli-get', 'cli-headless', 'output']) {
    assert.ok(graph().components.find(item => item.id === 'cli-kql-link').depends_on.includes(dependency));
  }
  for (const file of ['rust/clients/cli/app/src/v2_main.rs',
    'rust/clients/cli/app/src/v2_link_dataagent_main.rs',
    'rust/clients/cli/app/src/v2_link_dataagent_main_tests.rs',
    'rust/clients/cli/app/src/v2_dataagent_list_main.rs']) {
    assert.ok(kql.ENTRY.inputs.includes(file), `Missing coupled main consumer input ${file}`);
  }
});

test('an unmapped reverse consumer remains deferred rather than silently claimed as qualified', () => {
  const input = fixture();
  const name = 'proof-test-only-unmapped-consumer';
  input.metadata.packages.push({id: name, name,
    manifest_path: path.join(ROOT, 'rust', name, 'Cargo.toml'),
    features: {}, targets: [{name, kind: ['lib'], test: true,
      src_path: path.join(ROOT, 'rust', name, 'src', 'lib.rs')}],
    dependencies: [{name: 'fabric-sdk-items', path: path.join(ROOT, 'rust/engine/control-plane/resources/items'),
      kind: null, optional: false, target: null}],
  });
  input.metadata.workspace_members.push(name);
  const selection = plan({...input, files: ['rust/engine/control-plane/resources/items/tests/links.rs']});
  assert.ok(selection.packages.includes(name));
  assert.ok(selection.tests.some(item => item.package === name && item.target === 'all'));
  const selected = dataagent.selectedCommands(selection, undefined, input.graph, kql.ENTRY.leaves);
  assert.ok(selected.deferred.some(item => item.package === name && /not claimed/.test(item.disposition)));
  assert.ok(!selected.commands.some(item => item.args.includes(name)));
});

test('KQL proof keeps isolated CREATE owners separate and preserves the complete qualified Report inventory', () => {
  const targets = [
    ...kql.ENTRY.leaves.map(name => [name, name === kql.KQL_TARGET ? ['v2-link-kql'] : []]),
    ['eventhouse_create_binary', ['headless', 'v2-create-eventhouse']],
    ['kql_database_create_binary', ['headless', 'v2-kql-database-create']],
    ['create_binding_composition', ['headless', 'v2-create-eventhouse', 'v2-kql-database-create']],
    ['v2_create_report', []],
  ];
  const selection = {
    tests: targets.map(([name]) => ({package: 'fabric-cli', target: `test:${name}`,
      reasons: name === 'v2_create_report' ? ['Component closure: cli-report-create'] : []})),
    target_catalog: targets.map(([name, required]) => ({package: 'fabric-cli',
      target: `test:${name}`, source: `rust/clients/cli/app/tests/${name}.rs`, required_features: required})),
    profiles: [graph().profiles.find(item => item.id === 'v2-create-headless')],
  };
  const selected = dataagent.selectedCommands(selection, undefined, graph(), kql.ENTRY.leaves);
  const command = name => selected.commands.find(item => item.args.includes(name));
  assert.ok(!features(command('eventhouse_create_binary')).includes('v2-kql-database-create'));
  assert.ok(!features(command('kql_database_create_binary')).includes('v2-create-eventhouse'));
  for (const feature of ['v2-create-eventhouse', 'v2-kql-database-create']) {
    assert.ok(features(command('create_binding_composition')).includes(feature));
    assert.ok(selected.cli_features.includes(feature));
  }
  const report = command('v2_create_report');
  assert.ok(features(report).includes('v2-create'));
  // Isolate Report's receipt from other named targets in its feature group.
  const reportCommand = {args: ['test', '--test', 'v2_create_report']};
  const stdout = output(dataagent.REPORT_TESTS);
  const stderr = 'Running tests/v2_create_report.rs (target/debug/deps/test.exe)\n';
  assert.equal(dataagent.testResults(stdout, stderr, reportCommand)[0].passed, dataagent.REPORT_TESTS.length);
  for (const prefix of ['host::', 'host::http::', 'host::status::', 'process::']) {
    const name = dataagent.REPORT_TESTS.find(item => item.startsWith(prefix));
    assert.ok(name);
    assert.throws(() => dataagent.testResults(stdout.replace(name, 'other'), stderr, reportCommand));
  }
  assert.throws(() => dataagent.testResults(
    'test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n',
    stderr, reportCommand), /Report host\/process/);
});
