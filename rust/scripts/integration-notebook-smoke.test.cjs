// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {spawnSync} = require('node:child_process');
const {test} = require('node:test');
const notebook = require('./integration-notebook-smoke.cjs');

test('Notebook proof retains bounded complete targets and correct native feature profiles', () => {
  assert.equal(notebook.COMMANDS.length, 8);
  assert.deepEqual(notebook.COMMANDS[0], ['fetch', '--locked', '--target', 'x86_64-pc-windows-msvc']);
  const selected = notebook.COMMANDS.flat();
  for (const name of ['standalone', 'endpoint_context', 'create_policy', 'notebook_create',
    'report_create', 'create_host', 'create_v2', 'create_v2_process', 'v2_create_report',
    'livy_list_entry', 'eventhouse_create_binary', 'kql_database_create_binary',
    'eventhouse_create', 'kql_database_create', 'create_binding_composition']) {
    assert.ok(selected.includes(name), name);
  }
  for (const args of notebook.COMMANDS.slice(1)) {
    assert.ok(args.includes('--offline') && args.includes('--locked'));
    assert.ok(!args.includes('--workspace') && !args.includes('--all-features'));
    assert.ok(!args.includes('--'), 'No filter may silently suppress tests');
    if (args.includes('fabric-cli')) {
      assert.ok(args.includes('--no-default-features'));
      const features = args[args.indexOf('--features') + 1].split(',');
      assert.ok(features.includes('headless'));
      assert.ok(features.includes('v2-create') || features.includes('v2-create-eventhouse') ||
        features.includes('v2-kql-database-create'));
    }
  }
  assert.deepEqual(notebook.MINIMUM_TESTS, {create_host: 9, create_v2: 65, create_v2_process: 7});
});

test('evidence requires every target and rejects zero, ignored, filtered, failed or missing tests', () => {
  const args = ['test', '--test', 'create_v2'];
  const stderr = 'Running tests\\create_v2.rs (target\\debug\\deps\\create_v2.exe)\n';
  const stdout = 'test result: ok. 65 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n';
  assert.deepEqual(notebook.testResults(stdout, stderr, args),
    [{target: 'create_v2', passed: 65, failed: 0, ignored: 0, measured: 0, filtered: 0}]);
  for (const wrong of ['', stdout.replace('65 passed', '0 passed'), stdout.replace('65 passed', '64 passed'),
    stdout.replace('0 ignored', '1 ignored'), stdout.replace('0 filtered', '1 filtered'),
    stdout.replace('0 failed', '1 failed'), stdout.replace('ok.', 'FAILED.')]) {
    assert.throws(() => notebook.testResults(wrong, stderr, args));
  }
  assert.throws(() => notebook.testResults(stdout, '', args));
  assert.throws(() => notebook.testResults(stdout, stderr.replace('create_v2.rs', 'other.rs'), args));
  assert.throws(() => notebook.testResults(stdout + stdout, stderr, args));
  assert.throws(() => notebook.testResults('', '', ['test']));
  assert.throws(() => notebook.testResults(stdout.replace('65', '1'),
    stderr.replace('create_v2.rs', 'standalone.rs'), ['test', '--test', 'standalone']));
});

test('real optional runner refuses local native execution and retains the actual failure', () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'notebook-smoke-contract-'));
  try {
    const artifacts = path.join(directory, 'receipts');
    const result = spawnSync(process.execPath, [path.join(__dirname, 'integration-notebook-smoke.cjs'), artifacts], {
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
