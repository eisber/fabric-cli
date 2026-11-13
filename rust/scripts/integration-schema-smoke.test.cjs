// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {spawnSync} = require('node:child_process');
const {test} = require('node:test');
const schema = require('./integration-schema-smoke.cjs');

test('schema proof retains narrow offline targets and the registered process-test module', () => {
  assert.equal(schema.COMMANDS.length, 4);
  assert.deepEqual(schema.COMMANDS[0], ['fetch', '--locked', '--target', 'x86_64-pc-windows-msvc']);
  for (const args of schema.COMMANDS.slice(1)) {
    assert.ok(args.includes('--offline') && args.includes('--locked'));
    assert.ok(!args.includes('--workspace') && !args.includes('--all-features'));
  }
  assert.ok(schema.COMMANDS[1].includes('schema_document'));
  for (const name of ['v2_schema_native', 'v2_help', 'v2_discovery', 'v2_discovery_host', 'table_mounted']) {
    assert.ok(schema.COMMANDS[2].includes(name));
  }
  assert.ok(schema.COMMANDS[3].includes('schema::tests::'));
  assert.ok(schema.COMMANDS[3].includes('fab-cli-discovery'));
});

test('test evidence rejects empty, ignored, failing and missing targets without hiding counts', () => {
  assert.deepEqual(schema.testResults(
    'test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 19 filtered out\n'),
  [{passed: 10, failed: 0, ignored: 0, measured: 0, filtered: 19}]);
  for (const output of ['', 'test result: FAILED. 10 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out',
    'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out',
    'test result: ok. 10 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out']) {
    assert.throws(() => schema.testResults(output));
  }
});

test('real optional runner refuses local native execution and retains its failure', () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'schema-smoke-contract-'));
  try {
    const artifacts = path.join(directory, 'receipts');
    const result = spawnSync(process.execPath, [path.join(__dirname, 'integration-schema-smoke.cjs'), artifacts], {
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
