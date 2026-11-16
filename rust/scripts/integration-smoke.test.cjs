// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const vm = require('node:vm');
const {spawnSync} = require('node:child_process');
const {test} = require('node:test');
const smoke = require('./integration-smoke.cjs');

const base = '1'.repeat(40);
const source = '2'.repeat(40);
const combined = '3'.repeat(40);
const ref = `refs/heads/${smoke.BRANCH}`;
const event = {
  repository: {full_name: 'eisber/fabric-cli'}, before: base, after: combined, deleted: false,
  pull_request: {base: {ref: smoke.BRANCH, sha: base}, head: {sha: source}},
};
const workflow = file => fs.readFileSync(path.resolve(__dirname, '../../.github/workflows', file), 'utf8')
  .replace(/\r\n/g, '\n');
const result = (stdout = '', stderr = '', status = 0) => ({stdout, stderr, status, signal: null});
const baselineError = {error: {code: 'invalid_arguments',
  message: 'Invalid arguments. Run fab-rs --help for available commands and usage.'}};
const v2Error = {error: {code: 'InvalidTarget',
  message: 'get requires an absolute typed resource or returned-handle path'}, evidence: null};

function jobCondition(text, job) {
  const block = text.split(`\n  ${job}:`)[1].split(/\n  [a-z][\w-]*:/)[0];
  const condition = block.match(/\n    if: >-\n((?:      .+\n)+)/);
  if (condition) return condition[1].trim().replace(/\s*\n\s*/g, ' ');
  return block.match(/\n    if: \$\{\{ (.+) \}\}/)?.[1];
}

function evaluates(expression, name, target, branchRef, succeeded = true, head = '') {
  return vm.runInNewContext(expression, {
    github: {event_name: name, event: {pull_request: {base: {ref: target}, head: {ref: head}}}, ref: branchRef},
    always: () => true, success: () => succeeded,
  }, {timeout: 1000});
}

test('real workflow conditions route only trusted integration PR targets and push refs', () => {
  const full = workflow('rust-ci.yml');
  const native = workflow('rust-native-shard.yml');
  const minimal = workflow('integration-smoke.yml');
  const optional = workflow('semantic-model-bpa.yml');
  for (const [name, target, branchRef, selected] of [
    ['pull_request', smoke.BRANCH, 'refs/pull/90/merge', true],
    ['push', '', ref, true],
    ['pull_request', 'main', ref, false],
    ['pull_request', 'feature/rust', ref, false],
    ['pull_request', `${smoke.BRANCH}-lookalike`, ref, false],
    ['push', '', 'refs/heads/main', false],
    ['push', '', 'refs/heads/feature/rust', false],
    ['push', '', `${ref}-lookalike`, false],
    ['schedule', '', ref, false],
    ['workflow_dispatch', '', ref, false],
    ['merge_group', smoke.BRANCH, ref, false],
  ]) {
    assert.equal(smoke.integrationEvent(name, {pull_request: {base: {ref: target}}}, branchRef), selected);
    assert.equal(evaluates(jobCondition(minimal, 'smoke'), name, target, branchRef), selected);
    assert.equal(evaluates(jobCondition(full, 'changes'), name, target, branchRef), !selected);
    assert.equal(evaluates(jobCondition(full, 'validation'), name, target, branchRef, false), !selected);
    const expectedOptional = !(name === 'pull_request' && target === smoke.BRANCH);
    assert.equal(evaluates(jobCondition(optional, 'offline-tom'), name, target, branchRef), expectedOptional);
  }
  assert.match(minimal, /pull_request:\n    branches: \[dev\/marcozo\/fab-cli-v2-integration-20261104\]/);
  assert.match(minimal, /push:\n    branches: \[dev\/marcozo\/fab-cli-v2-integration-20261104\]/);
  assert.ok(!minimal.includes('workflow_dispatch:') && !minimal.includes('pull_request_target:'));
  assert.equal((minimal.match(/runs-on:/g) || []).length, 1);
  assert.match(minimal, /runs-on: windows-2025/);
  assert.match(minimal, /name: Integration smoke \(Windows, provisional\)/);
  assert.match(minimal, /ref: \$\{\{ github.sha \}\}/);
  assert.match(minimal, /fetch-depth: 0/);
  assert.match(minimal, /persist-credentials: false/);
  assert.match(minimal, /permissions:\n  contents: read/);
  assert.match(minimal, /if: always\(\)/);
  assert.match(minimal, /if-no-files-found: error/);
  for (const line of minimal.split('\n').filter(line => line.includes('uses:'))) {
    assert.match(line, /@[a-f0-9]{40}\b/);
  }
  assert.match(full, /needs: \[changes, native\]/);
  assert.match(full, /if: needs.changes.outputs.run_native == 'true'/);
  for (const retained of ['merge_group:', 'schedule:', 'workflow_dispatch:',
    'uses: ./.github/workflows/rust-native-shard.yml']) {
    assert.ok(full.includes(retained), `Full diagnostic path lost: ${retained}`);
  }
  assert.ok(native.includes('matrix: ${{ fromJSON(inputs.matrix) }}'));
  assert.ok(native.includes('node rust/scripts/validate-impact.cjs'));
  const {matrixForPlan, shardsForPlan} = require('./impact-shards.cjs');
  const diagnostic = {mode: 'full', files: [], packages: [], tests: [], profiles: [], checks: []};
  assert.deepEqual([...new Set(matrixForPlan(diagnostic).include.map(entry => entry.os))].sort(),
    ['macos-15', 'ubuntu-24.04', 'windows-2025']);
  assert.ok(shardsForPlan(diagnostic, 'windows').flatMap(shard => shard.commands).some(command =>
    command.args.join(' ').includes('--workspace --all-targets --all-features')));
});

test('bounded Report, KQL and Eventhouse conflict checks remain independent candidate-only PR steps', () => {
  const text = workflow('integration-smoke.yml');
  for (const [label, branch, prefix] of [
    ['Report66', 'dev/marcozo/reconcile-and-integrate-native-report-cr-toewpt', 'report'],
    ['KQL73', 'dev/marcozo/reconcile-and-integrate-kql-database-cre-toey4y', 'kql'],
    ['Eventhouse76', 'dev/marcozo/evaluate-and-integrate-released-eventhou-tof03g', 'eventhouse'],
  ]) {
    const block = text.split(`      - name: Check the ${label} conflict candidate's `)[1]
      .split('\n      - name:')[0];
    const expression = block.match(/\n        if: >-\n((?:          .+\n)+)/)[1]
      .trim().replace(/\s*\n\s*/g, ' ');
    for (const [event, head, expected] of [
      ['pull_request', branch, true], ['push', branch, false],
      ['pull_request', `${branch}-lookalike`, false],
      ['pull_request', 'unrelated', false], ['schedule', branch, false],
    ]) {
      assert.equal(evaluates(expression, event, smoke.BRANCH, ref, true, head), expected);
    }
    assert.match(block, /timeout-minutes: 10/);
    assert.match(block, /CARGO_TARGET_DIR: \$\{\{ runner.temp \}\}\/integration-smoke-target/);
    assert.match(block, /cargo fetch --locked --target x86_64-pc-windows-msvc/);
    assert.match(block, /cargo test --offline --locked -p fabric-sdk-operations --test standalone --test endpoint_context/);
    assert.match(block, /cargo test --offline --locked -p fabric-sdk-items --test create_policy/);
    assert.match(block, /cargo test --offline --locked -p fabric-sdk --test report_create/);
    assert.ok(block.includes(`source-inventory.cjs "$receipts/${prefix}-create-before.json"`));
    assert.ok(!block.includes('continue-on-error') && !block.includes('--all-features'));
    if (label === 'KQL73') {
      assert.match(block, /--no-default-features --features headless,v2-kql-database-create,runtime-v2 --test create_host --test kql_database_create --test kql_database_create_binary --test v2_create_report --test livy_list_entry/);
    }
    if (label === 'Eventhouse76') {
      assert.match(block, /--features headless,v2-create-eventhouse,runtime-v2 --test eventhouse_create_binary/);
      assert.match(block, /--features headless,v2-kql-database-create,runtime-v2 --test kql_database_create_binary/);
      assert.match(block, /--features headless,v2-create-eventhouse,v2-kql-database-create,runtime-v2 --test create_host --test eventhouse_create --test kql_database_create --test create_binding_composition --test v2_create_report --test livy_list_entry/);
      const after = text.split('      - name: Verify the optional Eventhouse76 check left source unchanged')[1]
        .split('\n      - name:')[0];
      assert.ok(after.includes('always()'));
      assert.ok(after.includes(`github.event.pull_request.head.ref == '${branch}'`));
      assert.ok(after.includes('eventhouse-create-after.json'));
      assert.ok(after.includes('eventhouse-create-before.json'));
    }
  }
});

test('PR54 schema proof is candidate-only and never broadens the generic or postpush profile', () => {
  const schema = require('./integration-schema-smoke.cjs');
  const block = workflow('integration-smoke.yml').split("      - name: Check the PR54 candidate's ")[1]
    .split('\n      - name:')[0];
  const expression = block.match(/\n        if: >-\n((?:          .+\n)+)/)[1]
    .trim().replace(/\s*\n\s*/g, ' ');
  for (const [event, head, expected] of [
    ['pull_request', schema.CANDIDATE, true], ['push', schema.CANDIDATE, false],
    ['pull_request', `${schema.CANDIDATE}-lookalike`, false],
    ['pull_request', 'unrelated', false], ['schedule', schema.CANDIDATE, false],
  ]) {
    assert.equal(evaluates(expression, event, smoke.BRANCH, ref, true, head), expected);
  }
  assert.match(block, /timeout-minutes: 10/);
  assert.match(block, /CARGO_TARGET_DIR: \$\{\{ runner.temp \}\}\/integration-smoke-target/);
  assert.ok(block.includes('node rust/scripts/integration-schema-smoke.cjs'));
  assert.ok(!block.includes('continue-on-error'));
});

test('PR source pin requires exact checkout and both exact parents, not source-only smoke', () => {
  const pin = () => smoke.sourcePin('pull_request', event, 'refs/pull/90/merge',
    combined, combined, [base, source], false);
  assert.deepEqual(pin(), {event: 'pull_request', ref: 'refs/pull/90/merge',
    source, base, checkout: combined, parents: [base, source]});
  for (const [expected, actual, parents] of [
    [combined, source, [base, source]], [combined, combined, [source, base]],
    [combined, combined, [base, '4'.repeat(40)]], [combined, combined, [base]],
  ]) {
    assert.throws(() => smoke.sourcePin('pull_request', event, ref, expected, actual, parents, true));
  }
  assert.throws(() => smoke.sourcePin('pull_request',
    {...event, repository: {full_name: 'other/fork'}}, ref, combined, combined, [base, source], false));
});

test('push receipt preserves exact before/after and rejects missing or rewritten ancestry', () => {
  assert.equal(smoke.sourcePin('push', event, ref, combined, combined,
    ['4'.repeat(40)], true).base, base, 'Multi-commit fast forwards are allowed');
  for (const change of [{before: '0'.repeat(40)}, {after: source}, {deleted: true}]) {
    assert.throws(() => smoke.sourcePin('push', {...event, ...change}, ref,
      combined, combined, [base, source], true));
  }
  assert.throws(() => smoke.sourcePin('push', event, ref, combined, combined, [base], false));
  assert.throws(() => smoke.sourcePin('workflow_dispatch', event, ref, combined, combined, [base], true));
});

test('two actual bin profiles build once: headless superset followed by offline default baseline', () => {
  assert.deepEqual(smoke.PROFILES, [
    {bin: 'fab-cli-v2', features: ['headless']}, {bin: 'fab-rs', features: []},
  ]);
  assert.deepEqual(smoke.buildArgs(smoke.PROFILES[0], false),
    ['build', '--locked', '-p', 'fabric-cli', '--bin', 'fab-cli-v2', '--no-default-features', '--features', 'headless']);
  assert.deepEqual(smoke.buildArgs(smoke.PROFILES[1], true),
    ['build', '--locked', '--offline', '-p', 'fabric-cli', '--bin', 'fab-rs', '--no-default-features']);
});

test('exact versions, help shape and sanitized offline error envelopes are mandatory', () => {
  for (const bin of ['fab-rs', 'fab-cli-v2']) {
    const cases = smoke.smokeCases(bin);
    assert.equal(cases.length, 3);
    const version = result(`${bin} 0.1.0\n`);
    smoke.verifyOutput(bin, '0.1.0', cases[1], version);
    for (const wrong of [result(`${bin} 0.1.0`), result(`${bin} 0.1.0\n`, 'noise'),
      result(`${bin} 9.0.0\n`), result(`${bin} 0.1.0\n`, '', 1)]) {
      assert.throws(() => smoke.verifyOutput(bin, '0.1.0', cases[1], wrong));
    }
    const help = bin === 'fab-rs'
      ? 'Usage: fab-rs\nCommands: ls get\n--help --version\n'
      : 'experimental explicit resource head\nUsage: fab-cli-v2 get <path>\nsearch <scope>\nexists <path>\nno identity fallback or interactive sign-in\n';
    smoke.verifyOutput(bin, '0.1.0', cases[0], result(help));
    assert.throws(() => smoke.verifyOutput(bin, '0.1.0', cases[0], result('help')));
    const error = bin === 'fab-rs' ? baselineError : v2Error;
    const stderr = JSON.stringify(error) + (bin === 'fab-rs' ? '\n' : '');
    smoke.verifyOutput(bin, '0.1.0', cases[2], result('', stderr, 2));
    for (const wrong of [result('', stderr), result('noise', stderr, 2),
      result('', '{}', 2), result('', stderr.replace(error.error.code, 'NotConfigured'), 2),
      result('', stderr.replace(error.error.message, 'must-not-be-echoed'), 2),
      result('', JSON.stringify({...error, extra: true}), 2)]) {
      assert.throws(() => smoke.verifyOutput(bin, '0.1.0', cases[2], wrong));
    }
  }
});

test('executables receive no ambient credentials, tools or profile state', () => {
  const env = smoke.isolatedEnvironment('isolated-home', 'sandbox', {
    SystemRoot: 'windows', WINDIR: 'windows', PATH: 'tools', GH_TOKEN: 'synthetic',
    AZURE_CLIENT_SECRET: 'synthetic', FAB_RUST_AUTH_MODE: 'headless', USERPROFILE: 'ambient',
  });
  assert.deepEqual(env, {HOME: 'isolated-home', USERPROFILE: 'isolated-home',
    XDG_CONFIG_HOME: 'isolated-home', XDG_CACHE_HOME: 'isolated-home',
    APPDATA: 'isolated-home', LOCALAPPDATA: 'isolated-home', TEMP: 'sandbox', TMP: 'sandbox',
    TMPDIR: 'sandbox', NO_COLOR: '1', PATH: '', SystemRoot: 'windows', WINDIR: 'windows'});
});

test('negative exits, launch errors and timeouts are persisted, never recast as success', () => {
  for (const response of [result('', 'build failed', 1),
    {...result(), status: null, error: new Error('spawn ENOENT')},
    {...result(), status: null, signal: 'SIGTERM', error: new Error('ETIMEDOUT')}]) {
    const report = {commands: []};
    let persisted = false;
    const invoke = () => smoke.capture('synthetic', ['build'], {timeout: 100}, report,
      (entry, output) => { persisted = true; assert.equal(output, response); },
      (file, args, options) => { assert.equal(options.shell, false); return response; });
    if (response.error) assert.throws(invoke);
    else assert.equal(invoke().status, 1);
    assert.equal(persisted, true);
    assert.equal(report.commands.length, 1);
    assert.equal(report.commands[0].exit, response.status);
    assert.equal(report.commands[0].error, response.error?.message || null);
    assert.ok(report.commands[0].elapsed_seconds >= 0);
  }
});

test('source integrity rejects changed bytes, removed/new files, modes and tracking state', () => {
  const before = {digest: base, files: [{path: 'Cargo.lock', sha256: source, mode: 420, tracked: true}]};
  smoke.requireUnchanged(before, structuredClone(before));
  for (const after of [{...before, digest: source},
    {...before, files: []}, {...before, files: [{...before.files[0], mode: 493}]},
    {...before, files: [{...before.files[0], tracked: false}]},
    {...before, files: [...before.files, {path: 'generated'}]}]) {
    assert.throws(() => smoke.requireUnchanged(before, after));
  }
});

test('real script refuses unadmitted local native execution with a failed durable receipt', () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'integration-smoke-contract-'));
  try {
    const artifacts = path.join(directory, 'receipts');
    const execution = spawnSync(process.execPath, [path.join(__dirname, 'integration-smoke.cjs'), artifacts], {
      env: {...process.env, RUNNER_ENVIRONMENT: 'local'}, encoding: 'utf8', timeout: 10000,
    });
    assert.equal(execution.status, 1);
    const receipt = JSON.parse(fs.readFileSync(path.join(artifacts, 'receipt.json'), 'utf8'));
    assert.equal(receipt.passed, false);
    assert.match(receipt.error, /hosted Windows only|No shared local native builds/);
    assert.deepEqual(receipt.commands, []);
  } finally {
    fs.rmSync(directory, {recursive: true});
  }
});
