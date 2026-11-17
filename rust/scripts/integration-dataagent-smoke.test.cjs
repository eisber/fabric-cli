// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const vm = require('node:vm');
const {spawnSync} = require('node:child_process');
const {test} = require('node:test');
const dataagent = require('./integration-dataagent-smoke.cjs');
const update = require('./integration-update-smoke.cjs');

test('DataAgent proof is bounded to the owned candidate, never postpush or a different branch', () => {
  const workflow = fs.readFileSync(path.resolve(__dirname, '../../.github/workflows/integration-smoke.yml'), 'utf8')
    .replace(/\r\n/g, '\n');
  const block = workflow.split("      - name: Check the PR51 candidate's ")[1].split('\n      - name:')[0];
  const expression = block.match(/\n        if: >-\n((?:          .+\n)+)/)[1]
    .trim().replace(/\s*\n\s*/g, ' ');
  for (const [event, head, expected] of [
    ['pull_request', dataagent.CANDIDATE, true], ['push', dataagent.CANDIDATE, false],
    ['pull_request', `${dataagent.CANDIDATE}-other`, false], ['schedule', dataagent.CANDIDATE, false],
  ]) {
    assert.equal(vm.runInNewContext(expression, {
      github: {event_name: event, event: {pull_request: {head: {ref: head}}}},
    }, {timeout: 1000}), expected);
  }
  assert.match(block, /timeout-minutes: 10/);
  assert.ok(!block.includes('continue-on-error'));
  assert.match(block, /CARGO_TARGET_DIR: \$\{\{ runner.temp \}\}\/integration-smoke-target/);
  assert.ok(workflow.includes('rust/scripts/integration-dataagent-smoke.test.cjs'));
  assert.equal(dataagent.BASE, 'ce50f369f2713ed1a2599099458f19bb0750d3e3');
  assert.equal(dataagent.SOURCE, '6af13a0d3f87077151ea674f32e2da659996b5ab');
});

test('the original headless DataAgent suites retain all eleven and three registered cases', () => {
  for (const [index, count] of [11, 3].entries()) {
    const command = dataagent.leafCommand(dataagent.LEAVES[index]);
    assert.equal(command.required.length, count);
    assert.ok(command.args.includes('headless') && command.exhaustive);
    const stdout = command.required.map(name => `test ${name} ... ok\n`).join('') +
      `test result: ok. ${count} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n`;
    const stderr = `Running tests/${dataagent.LEAVES[index]}.rs (target/debug/deps/test.exe)\n`;
    assert.equal(update.testResults(stdout, stderr, command)[0].passed, count);
    for (const wrong of ['', stdout.replace('0 ignored', '1 ignored'), stdout.replace('0 filtered', '1 filtered'),
      stdout.replace(command.required[0], 'other'), stdout.replace(`${count} passed`, '0 passed')]) {
      assert.throws(() => update.testResults(wrong, stderr, command));
    }
  }
});

test('metadata-selected targets retain manifest and source feature guards without broad Cargo execution', () => {
  const names = [...dataagent.LEAVES, 'optional_consumer'];
  const selection = {tests: [...names.map(name => ({package: 'fabric-cli', target: `test:${name}`})),
    {package: 'other', target: 'all'}],
  target_catalog: names.map(name => ({package: 'fabric-cli', target: `test:${name}`,
    source: `${name}.rs`, required_features: name === 'optional_consumer' ? ['v2-discovery'] : []}))};
  const selected = dataagent.selectedCommands(selection, () => '#![cfg(feature = "native-host")]\n');
  assert.deepEqual(selected.cli_features, ['headless', 'native-host', 'runtime-v2', 'v2-discovery']);
  for (const name of names) assert.ok(selected.commands.some(command => command.args.includes(name)));
  for (const {args} of selected.commands) {
    assert.ok(!args.includes('--workspace') && !args.includes('--all-features') && !args.includes('--all-targets'));
  }
  assert.equal(selected.deferred.length, 1);
  assert.match(selected.deferred[0].disposition, /not claimed/);
  assert.throws(() => dataagent.selectedCommands({tests: [], target_catalog: []}), /leaf selection/);
  assert.throws(() => dataagent.selectedCommands({...selection, target_catalog: []}), /metadata target/);
});

test('isolated Eventhouse and KQL process profiles are not broadened into a different owner contract', () => {
  const targets = [
    ...dataagent.LEAVES.map(name => [name, []]),
    ['eventhouse_create_binary', ['headless', 'v2-create-eventhouse']],
    ['kql_database_create_binary', ['headless', 'v2-kql-database-create']],
    ['create_binding_composition', ['headless', 'v2-create-eventhouse', 'v2-kql-database-create']],
  ];
  const selection = {
    tests: targets.map(([name]) => ({package: 'fabric-cli', target: `test:${name}`})),
    target_catalog: targets.map(([name, features]) => ({package: 'fabric-cli',
      target: `test:${name}`, source: `${name}.rs`, required_features: features})),
  };
  const selected = dataagent.selectedCommands(selection, () => '');
  const featuresFor = name => {
    const args = selected.commands.find(command => command.args.includes(name)).args;
    return args[args.indexOf('--features') + 1].split(',');
  };
  assert.ok(!featuresFor('eventhouse_create_binary').includes('v2-kql-database-create'));
  assert.ok(!featuresFor('kql_database_create_binary').includes('v2-create-eventhouse'));
  for (const feature of ['headless', 'runtime-v2', 'v2-create-eventhouse', 'v2-kql-database-create']) {
    assert.ok(featuresFor('create_binding_composition').includes(feature));
    assert.ok(selected.cli_features.includes(feature));
  }
});

test('failed commands retain partial suite observations without turning them into successful proof', () => {
  assert.deepEqual(dataagent.testObservations(
    'test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n' +
    'test result: FAILED. 5 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n'), [
    {status: 'ok', passed: 11, failed: 0, ignored: 0, measured: 0, filtered: 0},
    {status: 'FAILED', passed: 5, failed: 1, ignored: 0, measured: 0, filtered: 0},
  ]);
  assert.deepEqual(dataagent.testObservations(''), []);
});

test('impact registration selects actual DataAgent/SDK targets and existing profile obligations', () => {
  const graph = JSON.parse(fs.readFileSync(path.join(__dirname, '../validation/impact-graph.json')));
  const component = graph.components.find(item => item.id === 'cli-dataagent-list');
  assert.ok(component.inputs.includes('rust/clients/cli/app/src/v2_main.rs'));
  for (const target of dataagent.LEAVES) {
    assert.ok(component.tests.some(item => item.package === 'fabric-cli' && item.target === `test:${target}`));
  }
  for (const target of ['dataagents', 'representations']) {
    assert.ok(component.tests.some(item => item.package === 'fabric-sdk' && item.target === `test:${target}`));
  }
  for (const dependency of ['cli-get', 'cli-headless', 'query', 'output', 'storage-files']) {
    assert.ok(component.depends_on.includes(dependency));
  }
  assert.ok(component.checks.includes('headless') && component.checks.includes('combined'));
});

test('DataAgent runner refuses local native work and persists its actual failure', () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'dataagent-smoke-contract-'));
  try {
    const artifacts = path.join(directory, 'receipts');
    const result = spawnSync(process.execPath, [path.join(__dirname, 'integration-dataagent-smoke.cjs'), artifacts], {
      env: {}, encoding: 'utf8', timeout: 30000,
    });
    assert.equal(result.status, 1);
    const receipt = JSON.parse(fs.readFileSync(path.join(artifacts, 'receipt.json'), 'utf8'));
    assert.equal(receipt.passed, false);
    assert.equal(receipt.commands.length, 0);
    assert.match(receipt.error, /Hosted Windows only|No shared local native execution/);
  } finally {
    fs.rmSync(directory, {recursive: true});
  }
});
