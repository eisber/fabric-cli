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
const ocv = require('./integration-ocv-smoke.cjs');
const notebook = require('./integration-notebook-smoke.cjs');
const schema = require('./integration-schema-smoke.cjs');

test('OCV workflow leaf is bounded and candidate-only, never an implicit postpush qualification', () => {
  const text = fs.readFileSync(path.resolve(__dirname, '../../.github/workflows/integration-smoke.yml'), 'utf8')
    .replace(/\r\n/g, '\n');
  const block = text.split("      - name: Check the PR97 candidate's ")[1].split('\n      - name:')[0];
  const expression = block.match(/\n        if: >-\n((?:          .+\n)+)/)[1]
    .trim().replace(/\s*\n\s*/g, ' ');
  for (const [event, head, expected] of [
    ['pull_request', ocv.CANDIDATE, true], ['push', ocv.CANDIDATE, false],
    ['pull_request', `${ocv.CANDIDATE}-lookalike`, false],
    ['pull_request', 'unrelated', false], ['schedule', ocv.CANDIDATE, false],
  ]) {
    assert.equal(vm.runInNewContext(expression, {
      github: {event_name: event, event: {pull_request: {head: {ref: head}}}},
    }, {timeout: 1000}), expected);
  }
  assert.match(block, /timeout-minutes: 10/);
  assert.match(block, /CARGO_TARGET_DIR: \$\{\{ runner.temp \}\}\/integration-smoke-target/);
  assert.ok(block.includes('node rust/scripts/integration-ocv-smoke.cjs'));
  assert.ok(!block.includes('continue-on-error'));
  assert.ok(text.includes('rust/scripts/integration-ocv-smoke.test.cjs'));
});

test('OCV proof preserves explicit features, offline suites and existing shared-consumer selection', () => {
  assert.equal(ocv.COMMANDS.length, 16);
  assert.deepEqual(ocv.COMMANDS[0].args, notebook.COMMANDS[0]);
  assert.deepEqual(ocv.COMMANDS.slice(8, 15).map(command => command.args), notebook.COMMANDS.slice(1));
  assert.deepEqual(ocv.COMMANDS.at(-1).args, schema.COMMANDS.at(-1));
  assert.deepEqual(ocv.COMMANDS.slice(1, 8).map(command => command.minimum), [48, 48, 50, 12, 2, 16, 5]);
  for (const {args} of ocv.COMMANDS.slice(1)) {
    assert.ok(args.includes('--offline') && args.includes('--locked'));
    assert.ok(!args.includes('--workspace') && !args.includes('--all-features'));
    assert.ok(args.includes('--lib') || args.includes('--bin') || args.includes('--test'));
    if (args.includes('--bin')) {
      assert.equal(args.at(-1), 'feedback_host::tests::');
    }
  }
});

test('unit evidence rejects empty, ignored, filtered, missing and failed suites', () => {
  const command = ocv.COMMANDS[2];
  const stderr = 'Running unittests src\\lib.rs (target\\debug\\deps\\identity.exe)\n';
  const stdout = 'test entra_contract::tests::ocv_audiences_use_distinct_exact_delegated_scopes ... ok\n' +
    'test result: ok. 48 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n';
  assert.equal(ocv.testResults(stdout, stderr, command)[0].passed, 48);
  for (const wrong of ['', stdout.replace('48 passed', '0 passed'), stdout.replace('48 passed', '47 passed'),
    stdout.replace('0 ignored', '1 ignored'), stdout.replace('0 filtered', '1 filtered'),
    stdout.replace('0 failed', '1 failed'), stdout.replace('ok.', 'FAILED.'),
    stdout.replace('ocv_audiences', 'other_audiences')]) {
    assert.throws(() => ocv.testResults(wrong, stderr, command));
  }
  assert.throws(() => ocv.testResults(stdout, '', command));
  assert.throws(() => ocv.testResults(stdout + stdout, stderr, command));
});

test('host evidence requires every named fake lifecycle test despite the explicit namespace filter', () => {
  const command = ocv.COMMANDS[7];
  const stderr = 'Running unittests src/main.rs (target/debug/deps/fab_rs.exe)\n';
  const stdout = command.required.map(name => `test ${name} ... ok\n`).join('') +
    'test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 30 filtered out\n';
  assert.equal(ocv.testResults(stdout, stderr, command)[0].passed, 5);
  assert.throws(() => ocv.testResults(stdout.replace('5 passed', '0 passed'), stderr, command));
  assert.throws(() => ocv.testResults(stdout.replace(command.required[4], 'other'), stderr, command));
  assert.throws(() => ocv.testResults(stdout, stderr.replace('main.rs', 'lib.rs'), command));
});

test('feedback integration evidence retains complete native and default test counts', () => {
  const stderr = 'Running tests/feedback.rs (target/debug/deps/feedback.exe)\n';
  const stdout = 'test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n';
  assert.equal(ocv.testResults(stdout, stderr, ocv.COMMANDS[6])[0].passed, 16);
  assert.throws(() => ocv.testResults(stdout.replace('16 passed', '12 passed'), stderr, ocv.COMMANDS[6]));
  assert.throws(() => ocv.testResults(stdout, stderr.replace('feedback.rs', 'other.rs'), ocv.COMMANDS[6]));
});

test('OCV runner refuses local native execution and retains the actual failure', () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'ocv-smoke-contract-'));
  try {
    const artifacts = path.join(directory, 'receipts');
    const result = spawnSync(process.execPath, [path.join(__dirname, 'integration-ocv-smoke.cjs'), artifacts], {
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
