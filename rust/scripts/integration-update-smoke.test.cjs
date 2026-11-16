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
const update = require('./integration-update-smoke.cjs');
const notebook = require('./integration-notebook-smoke.cjs');
const ocv = require('./integration-ocv-smoke.cjs');

test('UPDATE leaf is bounded to this candidate PR and excluded from postpush', () => {
  const text = fs.readFileSync(path.resolve(__dirname, '../../.github/workflows/integration-smoke.yml'), 'utf8')
    .replace(/\r\n/g, '\n');
  const block = text.split("      - name: Check the PR56 candidate's ")[1].split('\n      - name:')[0];
  const expression = block.match(/\n        if: >-\n((?:          .+\n)+)/)[1]
    .trim().replace(/\s*\n\s*/g, ' ');
  for (const [event, head, expected] of [
    ['pull_request', update.CANDIDATE, true], ['push', update.CANDIDATE, false],
    ['pull_request', `${update.CANDIDATE}-other`, false],
    ['pull_request', 'unrelated', false], ['schedule', update.CANDIDATE, false],
  ]) {
    assert.equal(vm.runInNewContext(expression, {
      github: {event_name: event, event: {pull_request: {head: {ref: head}}}},
    }, {timeout: 1000}), expected);
  }
  assert.match(block, /timeout-minutes: 10/);
  assert.match(block, /CARGO_TARGET_DIR: \$\{\{ runner.temp \}\}\/integration-smoke-target/);
  assert.ok(block.includes('node rust/scripts/integration-update-smoke.cjs'));
  assert.ok(!block.includes('continue-on-error'));
  assert.ok(text.includes('rust/scripts/integration-update-smoke.test.cjs'));
});

test('UPDATE commands select real registered SDK/parser/host/process tests and shared consumers', () => {
  assert.equal(update.BASE, '92ebf4a34ef319c291c8344eafdb4c0ab2242ea6');
  assert.equal(update.SOURCE, '3f0f3a83d4b926c2b20dd0e168d3fab576d6112c');
  assert.deepEqual(update.COMMANDS[0].args, notebook.COMMANDS[0]);
  assert.deepEqual(update.COMMANDS.slice(6, 13).map(command => command.args), notebook.COMMANDS.slice(1));
  assert.deepEqual(update.COMMANDS.slice(13), ocv.COMMANDS.slice(3, 8));
  assert.deepEqual(update.UPDATE_TARGETS.map(item => item.target), ['update', 'v2_update', 'update_v2']);
  assert.ok(update.COMMANDS[1].required.includes(
    'approval_and_dispatch_share_exact_raw_patch_and_selected_endpoint_once'));
  assert.ok(update.HOST_TESTS.includes(
    'v2_update_notebook_host::tests::cancellation_during_patch_retains_unknown_receipt_across_publication_without_repoll_panic'));
  for (const {args} of update.COMMANDS.slice(1)) {
    assert.ok(args.includes('--offline') && args.includes('--locked'));
    assert.ok(!args.includes('--workspace') && !args.includes('--all-features'));
    assert.ok(args.includes('--test') || args.includes('--bin') || args.includes('--lib'));
  }
});

test('UPDATE complete-target proof rejects missing registered cases, empty, ignored or filtered suites', () => {
  const command = update.COMMANDS[1];
  const stdout = command.required.map(name => `test ${name} ... ok\n`).join('') +
    `test result: ok. ${command.required.length} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n`;
  const stderr = 'Running tests/update.rs (target/debug/deps/update.exe)\n';
  assert.equal(update.testResults(stdout, stderr, command)[0].passed, command.required.length);
  for (const wrong of ['', stdout.replace('0 ignored', '1 ignored'), stdout.replace('0 filtered', '1 filtered'),
    stdout.replace('0 failed', '1 failed'), stdout.replace('ok.', 'FAILED.'),
    stdout.replace(command.required[0], 'unrelated'), stdout.replace(`${command.required.length} passed`, '0 passed')]) {
    assert.throws(() => update.testResults(wrong, stderr, command));
  }
  assert.throws(() => update.testResults(stdout, '', command));
  assert.throws(() => update.testResults(stdout + stdout, stderr, command));
});

test('actual binary host proof requires every registered case with explicit namespace filtering', () => {
  const command = update.COMMANDS[4];
  const stdout = command.required.map(name => `test ${name} ... ok\n`).join('') +
    `test result: ok. ${command.required.length} passed; 0 failed; 0 ignored; 0 measured; 24 filtered out\n`;
  const stderr = 'Running unittests src\\v2_main.rs (target\\debug\\deps\\fab_cli_v2.exe)\n';
  assert.equal(update.testResults(stdout, stderr, command)[0].passed, command.required.length);
  assert.throws(() => update.testResults(stdout.replace(command.required[0], 'other'), stderr, command));
  assert.throws(() => update.testResults(stdout.replace('0 ignored', '1 ignored'), stderr, command));
  assert.throws(() => update.testResults(stdout, stderr.replace('v2_main.rs', 'main.rs'), command));
  assert.throws(() => update.testResults(stdout + stdout, stderr, command));
});

test('UPDATE runner refuses unallocated local native execution and retains failure evidence', () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'update-smoke-contract-'));
  try {
    const artifacts = path.join(directory, 'receipts');
    const result = spawnSync(process.execPath, [path.join(__dirname, 'integration-update-smoke.cjs'), artifacts], {
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
