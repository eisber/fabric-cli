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

test('real shared-input mappings preserve named leaves through Cargo reverse-consumer planning', () => {
  const root = path.resolve(__dirname, '../..');
  const catalog = JSON.parse(fs.readFileSync(path.join(root, 'rust/validation/impact-graph.json')));
  const locations = [
    ['fabric-sdk-items', 'rust/engine/control-plane/resources/items'],
    ['fabric-cli-mutations', 'rust/clients/cli/commands/mutations'],
    ['fabric-cli', 'rust/clients/cli/app'],
  ];
  const ids = ['sdk-items', 'sdk-items-link', 'mutations', 'mutations-link', 'cli-dataagent-link'];
  const components = catalog.components.filter(item => ids.includes(item.id)).map(item => ({
    ...item, depends_on: (item.depends_on || []).filter(id => ids.includes(id)),
    checks: [], profiles: item.package === 'fabric-cli' ? ['runtime-v2-native'] : [],
  }));
  assert.equal(components.length, ids.length);
  const packages = locations.map(([name, directory]) => {
    const manifest = fs.readFileSync(path.join(root, directory, 'Cargo.toml'), 'utf8');
    const features = Object.fromEntries([...((manifest.split('[features]')[1] || '').split(/^\[/m)[0])
      .matchAll(/^([\w-]+)\s*=/gm)].map(match => [match[1], []]));
    const targets = [...new Set(components.flatMap(item => item.tests)
      .filter(item => item.package === name).map(item => item.target))].map(target => {
      const [kind, targetName] = target.split(':');
      const section = manifest.split(`[[${kind}]]`).find(part =>
        part.match(new RegExp(`^name = "${targetName}"$`, 'm')));
      const source = kind === 'lib' ? 'src/lib.rs' :
        section?.match(/^path = "([^"]+)"$/m)?.[1] || `tests/${targetName}.rs`;
      assert.ok(fs.existsSync(path.join(root, directory, source)));
      return {name: targetName || name, kind: [kind], test: true,
        src_path: path.join(root, directory, source),
        'required-features': JSON.parse(section?.match(/^required-features = (\[[^\n]+\])$/m)?.[1] || '[]')};
    });
    const dependencies = locations.filter(([dependency]) => dependency !== name &&
      new RegExp(`^${dependency}(?:\\.workspace)?\\s*=`, 'm').test(manifest)).map(([dependency, location]) => ({
      name: dependency, path: path.join(root, location), kind: null, optional: false, target: null,
    }));
    return {id: name, name, manifest_path: path.join(root, directory, 'Cargo.toml'),
      dependencies, features, targets};
  });
  assert.ok(packages.find(item => item.name === 'fabric-cli-mutations').dependencies
    .some(item => item.name === 'fabric-sdk-items'));
  assert.ok(packages.find(item => item.name === 'fabric-cli').dependencies
    .some(item => item.name === 'fabric-cli-mutations'));
  const graph = {schema: 1, components,
    profiles: [{...catalog.profiles.find(item => item.id === 'runtime-v2-native'), checks: []}], checks: []};
  const metadata = {version: 1, packages, workspace_members: packages.map(item => item.id)};
  const files = ENTRY.inputs.filter(file => /(?:v2_link(?:_wire)?|items\/tests\/links)\.rs$/.test(file));
  assert.equal(files.length, 3);
  const selection = plan({root, graph, metadata, files});
  const leaves = ['v2_link_dataagent_host', 'v2_link_dataagent_process'];
  const selected = dataagent.selectedCommands(selection, undefined, graph, leaves);
  for (const name of leaves) assert.ok(selected.commands.some(item => item.args.includes(name)));
  for (const packageName of ['fabric-cli', 'fabric-cli-mutations', 'fabric-sdk-items']) {
    assert.ok(!selection.tests.some(item => item.package === packageName && item.target === 'all'));
  }
  const broken = structuredClone(graph);
  for (const component of broken.components) component.inputs = component.inputs?.filter(file => !files.includes(file));
  const fallback = plan({root, graph: broken, metadata, files});
  assert.ok(fallback.tests.some(item => item.package === 'fabric-cli' && item.target === 'all'));
  assert.throws(() => dataagent.selectedCommands(fallback, undefined, broken, leaves), /leaf selection/);
});
