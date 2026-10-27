// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {sha, lane, ciDelta, select, plan, execute, options, requireFastPass} = require('./rust-fast-check.cjs');

test('only full source IDs and finite lanes are accepted', () => {
  assert.equal(sha('a'.repeat(40)), true);
  for (const value of ['abc123', '0'.repeat(40), '--upload-pack=x', 'a'.repeat(40) + ';echo']) {
    assert.equal(sha(value), false);
  }
  for (const value of ['baseline', 'cli-jobs', 'endpoint', 'native-values', 'sdk-values', 'cli-auth']) assert.equal(lane(value), value);
  for (const value of ['__proto__', 'unknown', 'cli-jobs;echo']) assert.throws(() => lane(value));
  assert.throws(() => options(['--source', 'a', '--source', 'b'], ['--source']));
  assert.throws(() => options(['--command', 'echo'], ['--source']));
});

test('auth lane executes finite synthetic contracts, never a real login or native identity run', () => {
  const manifest = {lane: 'cli-auth', packages: ['fabric-cli', 'fabric-identity']};
  const checks = plan('check', manifest);
  assert.ok(checks[1].args.includes('windows-broker'));
  assert.ok(plan('clippy', manifest)[1].args.includes('windows-broker'));
  const tests = plan('test', manifest);
  assert.equal(tests.length, 3);
  for (const selector of ['auth_commands', 'auth_arguments', 'cli_contract', 'workspace_contract']) {
    assert.ok(tests[0].args.includes(selector));
  }
  assert.ok(tests[1].args.includes('--bin') && tests[1].args.includes('fab-rs'));
  assert.ok(tests[2].args.includes('--no-default-features'));
  const family = plan('family', manifest, '/target', '/receipts', () => true);
  assert.equal(family.length, 1);
  assert.ok(family[0].args.includes('--doc'));
  for (const item of [...checks, ...plan('clippy', manifest), ...tests, ...family]) {
    assert.equal(item.program, 'cargo');
    assert.ok(item.args.includes('--offline') && item.args.includes('--locked'));
    for (const prohibited of ['run', 'login', 'logout', 'status', 'native-read', 'fabric-identity-windows']) {
      assert.ok(!item.args.includes(prohibited));
    }
  }
  assert.throws(() => plan('family', manifest, '/target', '/receipts', () => false),
    /offline auth command consumer is missing/);
});

test('SDK/value lane retains in-boundary consumers without certifying central CLI', () => {
  const pkg = (name, layer, dependencies = []) => ({
    id: name, name, metadata: {fabric: {layer}}, dependencies: dependencies.map(name => ({name})),
  });
  const packages = [
    pkg('fabric-core', 'core'), pkg('fabric-core-json', 'core'),
    pkg('fabric-sdk-http', 'sdk', ['fabric-core']),
    pkg('fabric-sdk', 'sdk', ['fabric-sdk-http']),
    pkg('fabric-sdk-items', 'sdk', ['fabric-core']),
    pkg('fabric-cli-output', 'cli', ['fabric-core-json']),
    pkg('fabric-cli', 'cli', ['fabric-sdk-http']),
    pkg('fabric-identity', 'identity', ['fabric-core']),
    pkg('fabric-deploy', 'deploy', ['fabric-sdk']),
  ];
  assert.deepEqual(select({packages, workspace_default_members: packages.map(pkg => pkg.id)}, 'sdk-values'),
    ['fabric-cli-output', 'fabric-core', 'fabric-core-json', 'fabric-sdk', 'fabric-sdk-http', 'fabric-sdk-items']);
});

test('CI composition cannot modify product, old workflow, lock or oracle', () => {
  ciDelta(['.github/workflows/rust-fast-check.yml', 'rust/scripts/rust-fast-check.cjs',
    'rust/tests/performance/startup.py', 'rust/tests/performance/test_startup.py',
    'rust/tests/performance/measurement-request.json', 'rust/docs/performance-baseline.md']);
  for (const file of ['rust/Cargo.lock', '.github/workflows/rust-ci.yml',
    'rust/crates/fabric-cli/src/main.rs', 'rust/tests/compatibility/jobs/reference.py']) {
    assert.throws(() => ciDelta([file]));
  }
});

test('native CLI compilation precedes lint, unit tests and family consumers', () => {
  const manifest = {lane: 'cli-jobs', packages: ['fabric-cli', 'fabric-cli-jobs']};
  const commands = plan('check', manifest);
  assert.equal(commands.length, 2);
  assert.ok(!commands[0].args.includes('--features'));
  assert.deepEqual(commands[1], {program: 'cargo', args: [
    'check', '--offline', '--locked', '-p', 'fabric-cli',
    '--features', 'native-read', '--all-targets',
  ]});
  assert.equal(plan('check', {packages: ['fabric-sdk-http']}).length, 1);
});

test('SDK/value checks cover endpoint consumers and explicit native transport features', () => {
  const manifest = {lane: 'sdk-values', packages: [
    'fabric-core', 'fabric-sdk-http', 'fabric-sdk-operations', 'fabric-sdk-transport',
  ]};
  const checks = plan('check', manifest);
  assert.deepEqual(checks[1].args, [
    'check', '--offline', '--locked', '-p', 'fabric-sdk-transport', '--no-default-features', '--lib',
  ]);
  const lints = plan('clippy', manifest);
  assert.ok(lints[1].args.includes('hyper-rustls'));
  assert.ok(lints[1].args.includes('--all-targets'));
  const tests = plan('test', manifest);
  assert.deepEqual(tests[0].args, [
    'test', '--offline', '--locked', '-p', 'fabric-sdk-http', '-p', 'fabric-sdk-operations',
    '--test', 'endpoint_context', '--', '--test-threads=1',
  ]);
  assert.ok(tests.some(item => item.args.includes('--doc')));
  assert.ok(tests.some(item => item.args.includes('hyper-rustls')));
  for (const item of [...checks, ...lints, ...tests]) {
    assert.ok(item.args.includes('--offline'));
    assert.ok(item.args.includes('--locked'));
  }
  assert.throws(() => plan('test', {...manifest, packages: ['fabric-sdk-http']}),
    /requires HTTP and operation endpoint consumers/);
});

test('selection retains transitive consumers and safe default scope', () => {
  const metadata = {
    workspace_default_members: ['core', 'http', 'items', 'cli'],
    packages: [
      {id: 'core', name: 'fabric-core', dependencies: []},
      {id: 'http', name: 'fabric-sdk-http', dependencies: [{name: 'fabric-core'}]},
      {id: 'items', name: 'fabric-sdk-items', dependencies: [{name: 'fabric-core'}]},
      {id: 'cli', name: 'fabric-cli', dependencies: [{name: 'fabric-sdk-http'}]},
      {id: 'windows', name: 'fabric-identity-windows', dependencies: []},
    ],
  };
  const expected = ['fabric-cli', 'fabric-core', 'fabric-sdk-http', 'fabric-sdk-items'];
  assert.deepEqual(select(metadata, 'baseline'), expected);
  assert.deepEqual(select(metadata, 'endpoint'), expected);
  assert.throws(() => select(metadata, 'cli-jobs'), /Missing lane package/);
});

test('mandatory stages use locked offline all-targets and actual single-thread tests', () => {
  const manifest = {lane: 'endpoint', packages: ['fabric-core'], target: 'x86_64-unknown-linux-gnu'};
  assert.deepEqual(plan('fmt', manifest)[0].args, ['fmt', '--all', '--', '--check']);
  for (const stage of ['check', 'clippy']) {
    const args = plan(stage, manifest)[0].args;
    for (const expected of ['--offline', '--locked', '--all-targets', 'fabric-core']) assert.ok(args.includes(expected));
  }
  for (const item of plan('test', manifest)) assert.ok(item.args.includes('--test-threads=1'));
  assert.throws(() => plan('arbitrary shell', manifest));
});

test('job lane executes actual consumer and requires released comparison hook', () => {
  const manifest = {lane: 'cli-jobs', packages: ['fabric-cli', 'fabric-cli-jobs']};
  const commands = plan('family', manifest, '/private/target', '/private/receipts', () => true);
  assert.ok(commands.some(item => item.args.includes('offline_job')));
  assert.ok(commands.some(item => item.args.includes('tests/compatibility/job_reads/compare.py')));
  assert.ok(commands.some(item => item.args.includes('native-read')));
  assert.throws(() => plan('family', manifest, '/target', '/receipts', () => false), /comparator is missing/);
});

test('native-values requires native and text observations without weakening oracle', () => {
  const manifest = {lane: 'native-values', packages: ['fabric-core-json', 'fabric-cli-output']};
  const commands = plan('family', manifest, '/target', '/receipts', () => true);
  assert.ok(commands.some(item => item.args.includes('native_json') && item.stdout));
  assert.ok(commands.some(item => item.args.includes('--encoding-only')));
  assert.ok(commands.some(item => item.args.includes('tests/compatibility/output/text_reference.py')));
  assert.throws(() => plan('family', manifest, '/target', '/receipts', () => false), /comparator is missing/);
  assert.throws(() => plan('family', {...manifest, lane: 'sdk-values'},
    '/target', '/receipts', () => false), /comparator is missing/);
});

test('SDK/value family cannot pull deferred CLI consumers back into compilation', () => {
  const manifest = {lane: 'sdk-values', packages: [
    'fabric-core', 'fabric-core-json', 'fabric-sdk', 'fabric-sdk-http',
    'fabric-sdk-jobs', 'fabric-sdk-operations', 'fabric-sdk-onelake', 'fabric-cli-output',
    'fabric-sdk-transport',
  ]};
  const commands = plan('family', manifest, '/target', '/receipts', () => true);
  for (const item of commands) {
    for (const pkg of ['fabric-cli', 'fabric-cli-jobs', 'fabric-cli-mutations', 'fabric-deploy']) {
      assert.ok(!item.args.includes(pkg));
    }
  }
  assert.ok(commands.some(item => item.args.includes('tests/compatibility/jobs/reference.py')));
  assert.ok(commands.some(item => item.args.includes('native_json')));
  assert.ok(commands.some(item => item.args.includes('injected_credentials')));
  assert.ok(commands.some(item => item.args.includes('sdk_consumer') && item.args.includes('hyper-rustls')));
  assert.ok(commands.some(item => item.args.includes('tests/compatibility/items/reference.py') &&
    item.args.includes('--include-python-only')));
  assert.ok(commands.some(item => item.args.includes('tests/compatibility/http/reference.py')));
  assert.ok(commands.some(item => item.args.includes('test_text_reference.py')));
});

test('HTTP source oracle uses its documented pinned dependencies without altering shared requirements', () => {
  const requirements = fs.readFileSync(path.join(__dirname, 'rust-fast-requirements.txt'), 'utf8');
  assert.ok(requirements.includes('-r ../tests/compatibility/requirements.txt'));
  assert.ok(requirements.includes('requests==2.34.2'));
  assert.ok(requirements.includes('urllib3==2.8.0'));
  ciDelta(['rust/scripts/rust-fast-requirements.txt']);
});

test('command spy proves nonzero exits, spawn errors and signals stop subsequent work', () => {
  const commands = [{program: 'cargo', args: ['test']}, {program: 'cargo', args: ['run']}];
  for (const failure of [{status: 101}, {status: null, error: new Error('not installed')},
    {status: null, signal: 'SIGTERM'}]) {
    const calls = [];
    const results = execute(commands, '.', (program, args, opts) => {
      calls.push({program, args, opts});
      return failure;
    });
    assert.equal(calls.length, 1);
    assert.equal(calls[0].opts.shell, false);
    assert.equal(calls[0].opts.timeout, 300000);
    assert.equal(results.length, 1);
    assert.notEqual(results[0].exit, 0);
  }
  assert.equal(execute(commands, '.', () => ({status: 0})).length, 2);
});

test('full dispatch requires real fast success at the combined immutable head', () => {
  const receipt = {head: 'a'.repeat(40), source: 'b'.repeat(40)};
  const run = {head_sha: receipt.head, path: '.github/workflows/rust-fast-check.yml',
    status: 'completed', conclusion: 'success'};
  requireFastPass(receipt, run);
  for (const patch of [{head_sha: receipt.source}, {status: 'in_progress'},
    {conclusion: 'failure'}, {conclusion: 'skipped'}, {path: '.github/workflows/rust-ci.yml'}]) {
    assert.throws(() => requireFastPass(receipt, {...run, ...patch}));
  }
});

test('workflow limits runner context to steps and retains pinned bounded push gate', () => {
  const workflow = fs.readFileSync(path.resolve(__dirname, '../../.github/workflows/rust-fast-check.yml'), 'utf8');
  const beforeSteps = workflow.split('    steps:')[0];
  assert.ok(!beforeSteps.includes('runner.temp'));
  assert.ok(workflow.includes('branches: ["validation/rust-fast-*"]'));
  assert.ok(workflow.includes('timeout-minutes: 15'));
  assert.ok(workflow.includes('ref: ${{ github.sha }}'));
  assert.ok(workflow.includes('persist-credentials: false'));
  assert.ok(workflow.includes('"$GITHUB_ENV"'));
  assert.ok(workflow.indexOf('run check') < workflow.indexOf('run clippy'));
  assert.ok(workflow.indexOf('run check') < workflow.indexOf('run test'));
  assert.ok(workflow.indexOf('tests/performance') < workflow.indexOf('run check'));
  for (const action of workflow.matchAll(/uses: ([^\s]+)/g)) {
    assert.match(action[1], /@[a-f0-9]{40}$/);
  }
});
