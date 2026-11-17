// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const {test} = require('node:test');
const dataagent = require('./integration-dataagent-smoke.cjs');
const {ENTRY} = require('./integration-dataagent-link-smoke.cjs');

test('LINK proof is restricted to this exact candidate and immutable integration pins', () => {
  const workflow = fs.readFileSync(path.resolve(__dirname, '../../.github/workflows/integration-smoke.yml'), 'utf8')
    .replace(/\r\n/g, '\n');
  const block = workflow.split("      - name: Check the PR52 candidate's ")[1].split('\n      - name:')[0];
  const expression = block.match(/\n        if: >-\n((?:          .+\n)+)/)[1]
    .trim().replace(/\s*\n\s*/g, ' ');
  for (const [event, ref, expected] of [
    ['pull_request', ENTRY.candidate, true], ['push', ENTRY.candidate, false],
    ['pull_request', `${ENTRY.candidate}-other`, false], ['schedule', ENTRY.candidate, false],
  ]) {
    assert.equal(vm.runInNewContext(expression, {
      github: {event_name: event, event: {pull_request: {head: {ref}}}},
    }, {timeout: 1000}), expected);
  }
  assert.match(block, /timeout-minutes: 10/);
  assert.match(block, /integration-dataagent-link-smoke\.cjs/);
  assert.ok(!block.includes('continue-on-error'));
  assert.equal(ENTRY.base, '313271f4104a562d853318503f8e14f552ac6dc2');
  assert.equal(ENTRY.source, '1785a96cb74c325c6ee94bcffad72f98a9882ac9');
});

test('original LINK and LIST inventories must actually execute, with no ignored or filtered proxy', () => {
  for (const name of ENTRY.leaves) {
    const command = dataagent.leafCommand(name);
    const count = command.required.length;
    const stdout = command.required.map(item => `test ${item} ... ok\n`).join('') +
      `test result: ok. ${count} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n`;
    const stderr = `Running tests/${name}.rs (target/debug/deps/test.exe)\n`;
    assert.equal(dataagent.testResults(stdout, stderr, command)[0].passed, count);
    for (const wrong of ['', stdout.replace('0 ignored', '1 ignored'), stdout.replace('0 filtered', '1 filtered'),
      stdout.replace(command.required[0], 'other'), stdout.replace(`${count} passed`, '0 passed')]) {
      assert.throws(() => dataagent.testResults(wrong, stderr, command));
    }
  }
  const command = ENTRY.commands[0];
  assert.ok(command.args.includes('v2_link_dataagent_main::tests::'));
  assert.ok(command.required.every(name => name.startsWith('v2_link_dataagent_main::tests::')));
  const stdout = command.required.map(name => `test ${name} ... ok\n`).join('') +
    `test result: ok. ${command.required.length} passed; 0 failed; 0 ignored; 0 measured; 17 filtered out\n`;
  const stderr = 'Running unittests src/v2_main.rs (target/debug/deps/test.exe)\n';
  assert.equal(dataagent.testResults(stdout, stderr, command)[0].passed, command.required.length);
  assert.throws(() => dataagent.testResults(stdout.replace(command.required[0], 'other'), stderr, command));
});

test('LINK selection retains shared wire, SDK, output and current combined consumers', () => {
  const graph = JSON.parse(fs.readFileSync(path.join(__dirname, '../validation/impact-graph.json')));
  const component = graph.components.find(item => item.id === 'cli-dataagent-link');
  for (const target of ['v2_link_dataagent_host', 'v2_link_dataagent_process']) {
    assert.ok(component.tests.some(item => item.package === 'fabric-cli' && item.target === `test:${target}`));
  }
  for (const [packageName, target] of [
    ['fabric-cli-mutations', 'v2_link'], ['fabric-cli-mutations', 'v2_link_wire'], ['fabric-sdk-items', 'links'],
  ]) {
    assert.ok(component.tests.some(item => item.package === packageName && item.target === `test:${target}`));
  }
  for (const dependency of ['mutations', 'sdk-items', 'operations', 'cli-headless', 'output']) {
    assert.ok(component.depends_on.includes(dependency));
  }
  const names = ENTRY.leaves;
  const selection = {
    tests: names.map(name => ({package: 'fabric-cli', target: `test:${name}`})),
    target_catalog: names.map(name => ({package: 'fabric-cli', target: `test:${name}`,
      source: `${name}.rs`, required_features: ['headless']})),
  };
  const selected = dataagent.selectedCommands(selection, () => '', graph, names);
  for (const name of names) assert.ok(selected.commands.some(item => item.args.includes(name)));
  assert.throws(() => dataagent.selectedCommands({...selection,
    tests: selection.tests.slice(0, -1)}, () => '', graph, names), /leaf selection/);
});
