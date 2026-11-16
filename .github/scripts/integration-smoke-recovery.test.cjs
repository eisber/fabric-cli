// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const {test} = require('node:test');
const recovery = require('./integration-smoke-recovery.cjs');

const source = recovery.ANCHOR;
const base = 'ab95cd28ce1b1a111eda4e146ba9cbd275367021';
const second = '7238b6a0ed24b970656895cb5121cf960b1070e9';
const driver = '4'.repeat(40);
const environment = () => ({
  GITHUB_EVENT_NAME: 'workflow_dispatch', GITHUB_REPOSITORY: recovery.REPOSITORY,
  GITHUB_REF: 'refs/heads/main', GITHUB_SHA: driver, GITHUB_WORKFLOW_SHA: driver,
  GITHUB_WORKFLOW_REF: `${recovery.REPOSITORY}/${recovery.WORKFLOW}@refs/heads/main`,
  GITHUB_SERVER_URL: 'https://github.com', GITHUB_API_URL: 'https://api.github.com',
  GITHUB_RUN_ID: '123', GITHUB_RUN_ATTEMPT: '1',
  REQUESTED_SOURCE_SHA: source, REQUESTED_BASE_SHA: base,
});
const event = () => ({repository: {full_name: recovery.REPOSITORY, default_branch: 'main'},
  inputs: {source_sha: source, base_sha: base}});
const context = () => recovery.manualContext(environment(), event(), driver);

test('genuine manual proof binds the requested source, first parent, driver and event kind', () => {
  const receipt = recovery.manualPin(context(), source, source, [base, second], true);
  assert.equal(receipt.event, 'workflow_dispatch');
  assert.equal(receipt.source, source);
  assert.equal(receipt.checkout, source);
  assert.equal(receipt.resolved_head, source);
  assert.equal(receipt.base, base);
  assert.equal(receipt.driver, driver);
  assert.equal(receipt.anchor, recovery.ANCHOR);
  assert.deepEqual(receipt.parents, [base, second]);
  assert.equal(receipt.integration_ref, `refs/heads/${recovery.BRANCH}`);
  assert.equal(receipt.run_id, '123');
  assert.equal(receipt.run_attempt, '1');
});

test('short, zero, uppercase, whitespace, option and shell-shaped inputs fail closed', () => {
  for (const value of [undefined, null, 123, '', source.slice(0, 7), '0'.repeat(40),
    source.toUpperCase(), ` ${source}`, `${source}\n`, `${source};echo injected`,
    `$(echo ${source})`, '--help', `${source}/../main`, 'refs/heads/main']) {
    assert.throws(() => recovery.requireSha(value), String(value));
    for (const field of ['REQUESTED_SOURCE_SHA', 'REQUESTED_BASE_SHA']) {
      assert.throws(() => recovery.manualContext({...environment(), [field]: value}, event(), driver));
    }
  }
  assert.throws(() => recovery.manualContext({...environment(), REQUESTED_BASE_SHA: source},
    {...event(), inputs: {source_sha: source, base_sha: source}}, driver));
});

test('manual guard cannot accept push, PR, another repository, branch or driver revision', () => {
  for (const [field, value] of [
    ['GITHUB_EVENT_NAME', 'push'], ['GITHUB_EVENT_NAME', 'pull_request'],
    ['GITHUB_EVENT_NAME', 'repository_dispatch'], ['GITHUB_REPOSITORY', 'other/fork'],
    ['GITHUB_REF', `refs/heads/${recovery.BRANCH}`], ['GITHUB_REF', 'refs/heads/main-lookalike'],
    ['GITHUB_WORKFLOW_REF', `${recovery.REPOSITORY}/${recovery.WORKFLOW}@refs/heads/feature`],
    ['GITHUB_WORKFLOW_SHA', source], ['GITHUB_SHA', source],
    ['GITHUB_SERVER_URL', 'https://example.invalid'], ['GITHUB_API_URL', 'https://example.invalid'],
  ]) {
    assert.throws(() => recovery.manualContext({...environment(), [field]: value}, event(), driver));
  }
  assert.throws(() => recovery.manualContext(environment(), event(), source));
  for (const repository of [
    {full_name: 'other/fork', default_branch: 'main'},
    {full_name: recovery.REPOSITORY, default_branch: 'integration'},
  ]) assert.throws(() => recovery.manualContext(environment(), {...event(), repository}, driver));
  for (const inputs of [{}, {source_sha: second, base_sha: base},
    {source_sha: source, base_sha: second}]) {
    assert.throws(() => recovery.manualContext(environment(), {...event(), inputs}, driver));
  }
});

test('stale head, wrong checkout, wrong base, non-merge and rewritten history are rejected', () => {
  for (const [resolved, actual, parents, ancestry] of [
    [second, source, [base, second], true],
    [source, second, [base, second], true],
    [source, source, [second, base], true],
    [source, source, [base], true],
    [source, source, [base, second, driver], true],
    [source, source, [base, 'not-a-commit'], true],
    [source, source, [base, second], false],
  ]) assert.throws(() => recovery.manualPin(context(), resolved, actual, parents, ancestry));
  assert.throws(() => recovery.manualPin({...context(), event: 'push'},
    source, source, [base, second], true));
});

test('fixed provider lookup authenticates read-only and refuses bad or unavailable provider facts', async () => {
  let called = false;
  const head = await recovery.resolveHead('synthetic-token', async (url, options) => {
    called = true;
    assert.equal(url, `https://api.github.com/repos/${recovery.REPOSITORY}/branches/${encodeURIComponent(recovery.BRANCH)}`);
    assert.equal(options.headers.Authorization, 'Bearer synthetic-token');
    assert.equal(options.method, undefined);
    assert.ok(options.signal);
    return {status: 200, json: async () => ({name: recovery.BRANCH, commit: {sha: source}})};
  });
  assert.ok(called);
  assert.equal(head, source);
  await assert.rejects(recovery.resolveHead('', async () => assert.fail('No unauthenticated fallback')));
  for (const response of [
    {status: 403}, {status: 404}, {status: 500},
    {status: 200, json: async () => ({name: 'main', commit: {sha: source}})},
    {status: 200, json: async () => ({name: recovery.BRANCH, commit: {sha: 'short'}})},
  ]) await assert.rejects(recovery.resolveHead('synthetic-token', async () => response));
  await assert.rejects(recovery.resolveHead('synthetic-token', async () => { throw new Error('network failed'); }),
    /network failed/);
});

test('artifacts and cold build outputs cannot be placed inside either checkout', () => {
  const roots = [path.resolve('driver'), path.resolve('source')];
  for (const root of roots) {
    assert.throws(() => recovery.externalDirectory(root, roots));
    assert.throws(() => recovery.externalDirectory(path.join(root, 'target'), roots));
  }
  assert.throws(() => recovery.externalDirectory('relative', roots));
  assert.equal(recovery.externalDirectory(path.resolve('artifacts'), roots), path.resolve('artifacts'));
});

function hostedHarness(fault = '') {
  const driverRoot = path.resolve('driver');
  const sourceRoot = path.resolve('source');
  const artifacts = path.resolve('hosted-receipts');
  const files = new Map();
  const commands = [];
  const outputChecks = [];
  let helperLoads = 0;
  let inventories = 0;
  const env = {...environment(), GH_TOKEN: 'synthetic-token', GITHUB_TOKEN: 'synthetic-token',
    GITHUB_EVENT_PATH: path.resolve('event.json'), GITHUB_OUTPUT: path.resolve('github-output'),
    GITHUB_STEP_SUMMARY: path.resolve('github-summary'), RUNNER_ENVIRONMENT: 'github-hosted',
    RUNNER_OS: 'Windows', CARGO_TARGET_DIR: path.resolve('cold-target'), RECOVERY_ARTIFACTS: artifacts};
  files.set(env.GITHUB_EVENT_PATH, JSON.stringify(event()));
  files.set(path.join(sourceRoot, 'rust/Cargo.toml'), 'version = "0.1.0"\n');
  files.set(path.join(sourceRoot, 'rust/rust-toolchain.toml'), 'channel = "1.91.0"\n');
  files.set(path.join(sourceRoot, 'rust/Cargo.lock'), 'synthetic lockfile');
  files.set(path.join(driverRoot, recovery.WORKFLOW), 'synthetic workflow');
  files.set(path.join(driverRoot, '.github/scripts/integration-smoke-recovery.cjs'), 'synthetic driver');
  const fakeFs = {
    realpathSync: file => file,
    mkdirSync: () => {},
    existsSync: () => fault === 'warm-target',
    readFileSync: file => {
      assert.ok(files.has(file), `Unexpected read: ${file}`);
      return files.get(file);
    },
    writeFileSync: (file, value) => files.set(file, value),
    appendFileSync: (file, value) => files.set(file, (files.get(file) || '') + value),
  };
  const fakeGit = (file, args, options) => {
    assert.equal(file, 'git', 'No real native processes are admitted in this harness');
    let stdout = '';
    let status = 0;
    if (args[0] === 'rev-parse') {
      stdout = options.cwd === driverRoot ? driver : fault === 'wrong-checkout' ? second : source;
    } else if (args[0] === 'show') {
      stdout = `${base} ${second}`;
    } else if (args[0] === 'merge-base') {
      assert.deepEqual(Array.from(args), ['merge-base', '--is-ancestor', recovery.ANCHOR, source]);
      status = fault === 'rewritten' ? 1 : 0;
    } else {
      assert.equal(args[0], 'status');
      if (fault === 'dirty-source' && options.cwd === sourceRoot) stdout = ' M rust/Cargo.lock';
    }
    return {stdout, stderr: '', status, signal: null};
  };
  const fakeSmoke = {
    BRANCH: recovery.BRANCH,
    capture: (file, args, options, report, persist) => {
      commands.push({file, args: Array.from(args), options});
      assert.equal(options.env.GH_TOKEN, undefined, 'Repository token must not reach builds or binaries');
      assert.equal(options.env.GITHUB_TOKEN, undefined);
      let stdout = '';
      let status = 0;
      if (file === 'rustc') stdout = 'rustc 1.91.0 synthetic\nhost: x86_64-pc-windows-msvc\n';
      if (file === 'cargo' && args[0] === '--version') stdout = 'cargo 1.91.0 synthetic\n';
      if (file === process.execPath && fault === 'contracts-failed') status = 1;
      if (file === 'cargo' && args[0] === 'build') {
        const bin = args[args.indexOf('--bin') + 1];
        files.set(path.join(env.CARGO_TARGET_DIR, 'debug', `${bin}.exe`), `${bin} executable`);
        if (fault === 'build-failed') status = 1;
      }
      const result = {stdout, stderr: '', status, signal: null};
      report.commands.push({file, args, exit: status});
      persist(report.commands.at(-1), result);
      return result;
    },
    buildArgs: (profile, offline) => ['build', '--locked', ...(offline ? ['--offline'] : []),
      '-p', 'fabric-cli', '--bin', profile.bin, '--no-default-features',
      ...(profile.features.length ? ['--features', profile.features.join(',')] : [])],
    isolatedEnvironment: home => ({HOME: home, PATH: ''}),
    smokeCases: () => ['help', 'version', 'error'].map(kind => ({kind, args: [`--${kind}`]})),
    verifyOutput: (bin, version, item) => {
      outputChecks.push([bin, version, item.kind]);
      if (fault === 'bad-output') throw new Error('Exact output guard failed');
    },
    requireUnchanged: (before, after) => assert.equal(after.digest, before.digest, 'Source bytes changed'),
  };
  const inventory = {
    sourceInventory: root => {
      assert.equal(root, sourceRoot);
      inventories++;
      return {digest: fault === 'source-mutated' && inventories > 1 ? 'changed' : 'stable', files: []};
    },
    sourceDifferences: () => [],
  };
  const module = {exports: {}};
  const sandbox = {module, process: {platform: 'win32', execPath: process.execPath},
    AbortSignal, console, fetch: async () => ({status: 200, json: async () => ({
      name: recovery.BRANCH, commit: {sha: fault === 'moved-head' ? second : source},
    })}),
    require: name => {
      if (name === 'node:fs') return fakeFs;
      if (name === 'node:child_process') return {spawnSync: fakeGit};
      if (name === path.join(sourceRoot, 'rust/scripts/integration-smoke.cjs')) {
        helperLoads++;
        return fakeSmoke;
      }
      if (name === path.join(sourceRoot, 'rust/scripts/validate-impact.cjs')) return inventory;
      return require(name);
    }};
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, 'integration-smoke-recovery.cjs'), 'utf8'),
    sandbox, {timeout: 1000});
  fakeSmoke.PROFILES = module.exports.PROFILES;
  return {api: module.exports, env, driverRoot, commands, outputChecks,
    helperLoads: () => helperLoads,
    receipt: () => JSON.parse(files.get(path.join(artifacts, 'execution/receipt.json'))),
    output: () => files.get(env.GITHUB_OUTPUT),
    summary: () => files.get(env.GITHUB_STEP_SUMMARY)};
}

test('hosted orchestration preserves helper guards, source inventories and both build profiles', async () => {
  const harness = hostedHarness();
  await harness.api.preflight(harness.driverRoot, harness.env);
  assert.equal(harness.output(), `source_sha=${source}\n`);
  await harness.api.runSmoke(harness.driverRoot, harness.env);
  const receipt = harness.receipt();
  assert.equal(receipt.passed, true);
  assert.equal(receipt.source.checkout, source);
  assert.equal(receipt.source.event, 'workflow_dispatch');
  assert.equal(receipt.source.driver, driver);
  assert.equal(receipt.binaries.length, 2);
  assert.ok(receipt.binaries.every(bin => /^[a-f0-9]{64}$/.test(bin.sha256)));
  assert.equal(harness.outputChecks.length, 6);
  assert.equal(harness.commands[0].file, process.execPath);
  assert.deepEqual(harness.commands[0].args, ['--test', 'rust/scripts/integration-smoke.test.cjs',
    'rust/scripts/integration-notebook-smoke.test.cjs']);
  const builds = harness.commands.filter(command => command.args[0] === 'build');
  assert.deepEqual(builds.map(command => command.args), [
    ['build', '--locked', '-p', 'fabric-cli', '--bin', 'fab-cli-v2',
      '--no-default-features', '--features', 'headless'],
    ['build', '--locked', '--offline', '-p', 'fabric-cli', '--bin', 'fab-rs', '--no-default-features'],
  ]);
  assert.ok(builds.every(command => command.options.env.CARGO_TARGET_DIR === harness.env.CARGO_TARGET_DIR));
  assert.match(harness.summary(), /workflow_dispatch/);
});

test('failed provenance never loads source helpers; later failures leave unsuccessful durable receipts', async () => {
  for (const fault of ['moved-head', 'wrong-checkout', 'rewritten', 'dirty-source',
    'contracts-failed', 'warm-target', 'build-failed', 'bad-output', 'source-mutated']) {
    const harness = hostedHarness(fault);
    if (fault === 'moved-head') {
      await assert.rejects(harness.api.preflight(harness.driverRoot, harness.env));
      assert.equal(harness.output(), undefined);
    } else {
      await harness.api.preflight(harness.driverRoot, harness.env);
      await assert.rejects(harness.api.runSmoke(harness.driverRoot, harness.env), undefined, fault);
      assert.equal(harness.receipt().passed, false, fault);
      assert.ok(harness.receipt().error || harness.receipt().integrity_error, fault);
    }
    if (['moved-head', 'wrong-checkout', 'rewritten', 'dirty-source'].includes(fault)) {
      assert.equal(harness.helperLoads(), 0, fault);
      assert.equal(harness.commands.length, 0, fault);
    }
  }
});

test('workflow keeps original integration events separate and runs no native PR jobs', () => {
  const workflow = fs.readFileSync(path.resolve(__dirname, '..', 'workflows',
    'integration-smoke-recovery.yml'), 'utf8').replace(/\r\n/g, '\n');
  assert.match(workflow, /workflow_dispatch:\n    inputs:\n      source_sha:/);
  assert.match(workflow, /base_sha:\n        description: .+\n        required: true\n        type: string/);
  assert.match(workflow, /pull_request:\n    branches: \[main\]/);
  assert.ok(!/^\s+(push|pull_request_target|repository_dispatch|schedule):/m.test(workflow));
  assert.match(workflow, /permissions:\n  contents: read/);
  assert.match(workflow, /REQUESTED_SOURCE_SHA: \$\{\{ inputs.source_sha \}\}/);
  assert.match(workflow, /REQUESTED_BASE_SHA: \$\{\{ inputs.base_sha \}\}/);
  assert.ok(!/run:.*\$\{\{/.test(workflow), 'No expression interpolation in shell commands');
  assert.match(workflow, /ref: \$\{\{ steps.pin.outputs.source_sha \}\}/);
  assert.match(workflow, /repository: eisber\/fabric-cli/);
  assert.match(workflow, /fetch-depth: 0/);
  assert.equal((workflow.match(/persist-credentials: false/g) || []).length, 3);
  assert.match(workflow, /if: always\(\)/);
  assert.match(workflow, /if-no-files-found: error/);
  const native = workflow.split('\n  smoke:')[1];
  const condition = native.match(/\n    if: (.+)/)[1];
  for (const [name, ref, expected] of [
    ['workflow_dispatch', 'refs/heads/main', true],
    ['workflow_dispatch', `refs/heads/${recovery.BRANCH}`, false],
    ['push', `refs/heads/${recovery.BRANCH}`, false],
    ['pull_request', 'refs/pull/1/merge', false], ['schedule', 'refs/heads/main', false],
  ]) assert.equal(vm.runInNewContext(condition, {github: {event_name: name, ref}}), expected);
  assert.match(native, /needs: contracts/);
  assert.match(native, /runs-on: windows-2025/);
  assert.match(native, /timeout-minutes: 45/);
  assert.ok(!native.includes('matrix:') && !native.includes('continue-on-error'));
  for (const line of workflow.split('\n').filter(line => line.includes('uses:'))) {
    assert.match(line, /@[a-f0-9]{40}\b/);
  }
  assert.deepEqual(recovery.PROFILES, [
    {bin: 'fab-cli-v2', features: ['headless']}, {bin: 'fab-rs', features: []},
  ]);
});
