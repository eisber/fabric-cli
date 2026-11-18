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
const target = require('./integration-target-witness.cjs');
const smoke = require('./integration-smoke.cjs');

const base = '1'.repeat(40);
const source = '2'.repeat(40);
const checkout = '3'.repeat(40);
const live = '4'.repeat(40);
const now = Date.parse('2026-11-18T03:00:00.000Z');
const secret = 'synthetic-token-never-retain';
const event = {number: 109, repository: {full_name: target.REPOSITORY},
  pull_request: {number: 109, base: {ref: target.BRANCH, sha: base,
    repo: {full_name: target.REPOSITORY}}, head: {sha: source}}};
const env = {GITHUB_EVENT_NAME: 'pull_request', GITHUB_REPOSITORY: target.REPOSITORY,
  GITHUB_REF: 'refs/pull/109/merge', GITHUB_SHA: checkout, GITHUB_RUN_ID: '44227969639',
  GITHUB_RUN_ATTEMPT: '1', GITHUB_JOB: 'smoke', GITHUB_SERVER_URL: 'https://github.com',
  GITHUB_API_URL: 'https://api.github.com', GH_TOKEN: secret};
const providerRef = (sha = live) => ({ref: target.TARGET_REF,
  url: `https://api.github.com/repos/${target.REPOSITORY}/git/refs/heads/${target.BRANCH}`,
  object: {type: 'commit', sha,
    url: `https://api.github.com/repos/${target.REPOSITORY}/git/commits/${sha}`}});
const response = (value, headers = {}) => new Response(JSON.stringify(value),
  {headers: {date: new Date(now).toUTCString(), ...headers}});
const acquire = (fetcher = async () => response(providerRef()), input = event, context = env,
  actual = checkout, parents = [live, source]) =>
  target.acquireWitness(input, context, actual, parents, fetcher, () => now);
const pin = (witness, input = event, context = env, parents = [live, source]) =>
  smoke.sourcePin('pull_request', input, context.GITHUB_REF, context.GITHUB_SHA,
    context.GITHUB_SHA, parents, false, witness, context);

test('stale embedded event base needs an independent exact target witness, never just first parent', async () => {
  const witness = await acquire();
  const receipt = pin(witness);
  assert.equal(receipt.event_base, base);
  assert.equal(receipt.checkout_base, live);
  assert.equal(receipt.base, live);
  assert.deepEqual(receipt.parents, [live, source]);
  assert.equal(receipt.target_witness, witness);
  assert.throws(() => pin(null), {code: 'TARGET_WITNESS_SCHEMA'});
  const matchingEvent = structuredClone(event);
  matchingEvent.pull_request.base.sha = live;
  assert.throws(() => pin(null, matchingEvent), {code: 'TARGET_WITNESS_SCHEMA'});
  assert.throws(() => pin(undefined), {code: 'TARGET_WITNESS_MISSING'});
});

test('reported PR109 event61c/checkoutaf846 mismatch is admitted only by a matching live witness', async () => {
  const input = structuredClone(event);
  input.pull_request.base.sha = '61c8221914152e231e5e64f7bd38418f66264999';
  input.pull_request.head.sha = '43f0218799d2db9da6ee12b8cc7d0063b3925d58';
  const context = {...env, GITHUB_SHA: '1e177d6afe64fe3e3ef584d1fb1cdb495ff3b2a2'};
  const parents = ['af846fe7e3ea444051aae3881491078323527d5f', input.pull_request.head.sha];
  const witness = await acquire(async () => response(providerRef(parents[0])),
    input, context, context.GITHUB_SHA, parents);
  assert.equal(pin(witness, input, context, parents).checkout_base, parents[0]);
  await assert.rejects(acquire(async () => response(providerRef(input.pull_request.base.sha)),
    input, context, context.GITHUB_SHA, parents), {code: 'TARGET_DRIFT'});
  assert.throws(() => pin(undefined, input, context, parents), {code: 'TARGET_WITNESS_MISSING'});
});

test('one bounded exact-endpoint read disables caches and redirects, with step-only authentication', async () => {
  let calls = 0;
  const witness = await acquire(async (url, options) => {
    calls++;
    assert.equal(url, target.ENDPOINT);
    assert.equal(options.method, 'GET');
    assert.equal(options.redirect, 'error');
    assert.equal(options.cache, 'no-store');
    assert.equal(options.headers['Cache-Control'], 'no-cache, no-store');
    assert.equal(options.headers.Authorization, `Bearer ${secret}`);
    assert.equal(options.headers['X-GitHub-Api-Version'], '2022-11-28');
    assert.ok(options.signal instanceof AbortSignal);
    return response({...providerRef(), unrelated: secret});
  });
  assert.equal(calls, 1);
  assert.equal(witness.observed_at, new Date(now).toISOString());
  assert.ok(!JSON.stringify(witness).includes(secret), 'Provider extras must not enter artifacts');
});

test('wrong repository/ref/PR/run/source context fails before any provider request', async () => {
  for (const change of [
    {GITHUB_EVENT_NAME: 'push'}, {GITHUB_REPOSITORY: 'other/fabric-cli'},
    {GITHUB_REF: 'refs/heads/main'}, {GITHUB_REF: 'refs/pull/109/head'},
    {GITHUB_REF: 'refs/pull/110/merge'}, {GITHUB_SHA: '0'.repeat(40)},
    {GITHUB_RUN_ID: ''}, {GITHUB_RUN_ATTEMPT: '0'}, {GITHUB_JOB: 'other'},
    {GITHUB_SERVER_URL: 'https://other.invalid'}, {GITHUB_API_URL: 'https://other.invalid'},
  ]) {
    await assert.rejects(acquire(() => { assert.fail('Must not contact provider'); }, event,
      {...env, ...change}), {code: 'TARGET_WITNESS_CONTEXT'});
  }
  for (const change of [
    value => { value.repository.full_name = 'other/fabric-cli'; },
    value => { value.pull_request.base.repo.full_name = 'other/fabric-cli'; },
    value => { value.pull_request.base.ref += '-lookalike'; },
    value => { value.pull_request.base.ref = 'main'; },
    value => { value.pull_request.head.sha = ''; },
    value => { value.pull_request.base.sha = '0'.repeat(40); },
    value => { value.pull_request.number++; },
    value => { value.number = '109'; },
  ]) {
    const input = structuredClone(event);
    change(input);
    await assert.rejects(acquire(() => { assert.fail('Must not contact provider'); }, input),
      {code: 'TARGET_WITNESS_CONTEXT'});
  }
  await assert.rejects(acquire(() => { assert.fail('No anonymous/auth fallback'); }, event,
    {...env, GH_TOKEN: ''}), {code: 'TARGET_WITNESS_AUTH'});
});

test('exact checkout and two ordered parents are mandatory before acquisition and at consumption', async () => {
  const witness = await acquire();
  for (const [actual, parents] of [
    [source, [live, source]], [checkout, [source, live]], [checkout, [base, live]],
    [checkout, [live]], [checkout, [live, source, base]], [checkout, []],
    [checkout, ['0'.repeat(40), source]],
  ]) {
    await assert.rejects(acquire(() => { assert.fail('Must not contact provider'); },
      event, env, actual, parents), {code: 'TARGET_WITNESS_CHECKOUT'});
    assert.throws(() => target.validateWitness(witness, target.witnessContext(event, env),
      actual, parents, now), {code: 'TARGET_WITNESS_CHECKOUT'});
  }
});

test('provider ref schema binds exact repository, full branch, commit type and nonzero SHA', async () => {
  for (const change of [
    value => { value.ref += '-lookalike'; },
    value => { value.ref = 'refs/heads/main'; },
    value => { value.url = value.url.replace('eisber', 'other'); },
    value => { value.object.type = 'tag'; },
    value => { value.object.sha = '0'.repeat(40); },
    value => { value.object.sha = 'bad'; },
    value => { value.object.url = value.object.url.replace('eisber', 'other'); },
    value => { delete value.object; },
  ]) {
    const value = providerRef();
    change(value);
    await assert.rejects(acquire(async () => response(value)), {code: 'TARGET_WITNESS_PROVIDER_SCHEMA'});
  }
});

test('target drift and an old live head are typed failures, never ancestor or retry acceptance', async () => {
  let calls = 0;
  await assert.rejects(acquire(async () => {
    calls++;
    return response(providerRef(base));
  }), error => {
    assert.equal(error.code, 'TARGET_DRIFT');
    assert.equal(error.expected_checkout_base, live);
    assert.equal(error.observed_target_base, base);
    return true;
  });
  assert.equal(calls, 1);
});

test('cached or stale provider observations cannot be re-labelled as fresh target evidence', async () => {
  for (const headers of [{age: '1'}, {age: 'invalid'},
    {date: new Date(now - 60000).toUTCString()}, {date: ''},
    {date: new Date(now + 60000).toUTCString()}]) {
    await assert.rejects(acquire(async () => response(providerRef(), headers)),
      {code: 'TARGET_WITNESS_FRESHNESS'});
  }
  assert.equal((await acquire(async () => response(providerRef(), {age: '0'}))).checkout_base, live);
});

test('failed, malformed, oversized, interrupted or redirected provider reads never fall back or leak errors', async () => {
  for (const fetcher of [
    async () => new Response(secret, {status: 403}),
    async () => new Response(secret, {status: 429}),
    async () => new Response(secret, {status: 302}),
    async () => new Response(secret),
    async () => new Response(' '.repeat(16385)),
    async () => new Response(new ReadableStream({start(controller) { controller.error(new Error(secret)); }})),
    async () => { throw new Error(secret); },
  ]) {
    let calls = 0;
    await assert.rejects(acquire((...args) => { calls++; return fetcher(...args); }), error => {
      assert.equal(error.code, 'TARGET_WITNESS_PROVIDER');
      assert.ok(!error.message.includes(secret));
      return true;
    });
    assert.equal(calls, 1);
  }
});

test('witness binding rejects stale run/attempt, wrong source/head/ref/repository, schema and future time', async () => {
  const witness = await acquire();
  for (const change of [
    {repository: 'other/fabric-cli'}, {target_ref: `${target.TARGET_REF}-lookalike`},
    {event_ref: 'refs/pull/109/head'}, {source: base}, {event_base: live}, {checkout: source},
    {run_id: '44227969638'}, {run_attempt: '2'}, {job: 'other'}, {schema: 2},
    {endpoint: target.ENDPOINT.replace('eisber', 'other')}, {kind: 'first-parent'},
    {checkout_base: base}, {checkout_base: '0'.repeat(40)}, {unexpected: secret},
    {observed_at: 'not a time'}, {observed_at: new Date(now + 1000).toISOString()},
    {requested_at: new Date(now - 60000).toISOString()},
    {provider_date: new Date(now - 60000).toUTCString()},
  ]) {
    assert.throws(() => target.validateWitness({...witness, ...change},
      target.witnessContext(event, env), checkout, [live, source], now));
  }
  const missing = {...witness};
  delete missing.observed_at;
  assert.throws(() => pin(missing), {code: 'TARGET_WITNESS_SCHEMA'});
  assert.equal(target.validateWitness(witness, target.witnessContext(event, env),
    checkout, [live, source], now + 45 * 60000), witness,
  'Immutable same-attempt evidence remains bound during later candidate steps');
});

test('exclusive witness file, workflow digest and safe failure receipts retain no tokens or raw provider errors', async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'target-witness-'));
  try {
    const file = path.join(directory, target.FILE);
    const environmentFile = path.join(directory, 'workflow-env');
    const witness = await acquire();
    await target.recordWitness(file, async () => witness, environmentFile);
    const bytes = fs.readFileSync(file);
    const exported = fs.readFileSync(environmentFile, 'utf8');
    assert.equal(exported, `INTEGRATION_TARGET_WITNESS_SHA256=${target.digest(bytes)}\n`);
    assert.ok(!bytes.toString().includes(secret));
    assert.ok(!exported.includes(secret));
    const consumerEnv = {...env, RUNNER_TEMP: directory,
      INTEGRATION_TARGET_WITNESS_SHA256: target.digest(bytes)};
    assert.deepEqual(pin(target.loadWitness(consumerEnv)), pin(witness));
    assert.deepEqual(pin(undefined, event, consumerEnv), pin(witness), 'Real callers load the same witness');
    await assert.rejects(target.recordWitness(file, () => {
      assert.fail('Existing witness must not be refreshed or overwritten');
    }, environmentFile), {code: 'EEXIST'});
    fs.appendFileSync(file, ' ');
    assert.throws(() => target.loadWitness(consumerEnv), {code: 'TARGET_WITNESS_DIGEST'});
    fs.writeFileSync(file, '{');
    assert.throws(() => target.loadWitness({...consumerEnv,
      INTEGRATION_TARGET_WITNESS_SHA256: target.digest('{')}), {code: 'TARGET_WITNESS_SCHEMA'});
    assert.throws(() => target.loadWitness({...consumerEnv, INTEGRATION_TARGET_WITNESS_SHA256: ''}),
      {code: 'TARGET_WITNESS_MISSING'});
    fs.unlinkSync(file);
    assert.throws(() => target.loadWitness(consumerEnv), {code: 'TARGET_WITNESS_MISSING'});
    await assert.rejects(target.recordWitness(file, async () => {
      throw new Error(secret);
    }, environmentFile), {code: 'TARGET_WITNESS_FAILED'});
    assert.deepEqual(JSON.parse(fs.readFileSync(file, 'utf8')),
      {schema: 1, passed: false, error_code: 'TARGET_WITNESS_FAILED'});
    assert.equal(fs.readFileSync(environmentFile, 'utf8'), exported, 'Failure must not export fresh proof');
    assert.ok(!fs.readFileSync(file, 'utf8').includes(secret));
    fs.unlinkSync(file);
    await assert.rejects(target.recordWitness(file, () => acquire(async () => response(providerRef(base))),
      environmentFile), {code: 'TARGET_DRIFT'});
    assert.deepEqual(JSON.parse(fs.readFileSync(file, 'utf8')), {schema: 1, passed: false,
      error_code: 'TARGET_DRIFT', expected_checkout_base: live, observed_target_base: base});
    fs.unlinkSync(file);
    await assert.rejects(target.recordWitness(file, () => acquire(async () => new Response(secret,
      {status: 429, headers: {'retry-after': '60', 'x-ratelimit-reset': '1794970920'}})),
    environmentFile), error => {
      assert.equal(error.code, 'TARGET_WITNESS_PROVIDER');
      assert.ok(error.message.includes('"provider_status":429'));
      assert.ok(!error.message.includes(secret));
      return true;
    });
    assert.deepEqual(JSON.parse(fs.readFileSync(file, 'utf8')), {schema: 1, passed: false,
      error_code: 'TARGET_WITNESS_PROVIDER', provider_status: 429, retry_after_seconds: 60,
      rate_limit_reset_epoch_seconds: 1794970920});
  } finally {
    fs.rmSync(directory, {recursive: true});
  }
});

test('real acquisition entry accepts the workflow mixed-separator path and exports only a digest', async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'target-witness-acquisition-'));
  try {
    const eventFile = path.join(directory, 'event.json');
    const environmentFile = path.join(directory, 'workflow-env');
    fs.writeFileSync(eventFile, JSON.stringify(event));
    const context = {...env, RUNNER_TEMP: directory, GITHUB_EVENT_PATH: eventFile,
      GITHUB_ENV: environmentFile, RUNNER_ENVIRONMENT: 'github-hosted'};
    let providerReads = 0;
    const sandbox = {module: {exports: {}}, require(name) {
      if (name === 'node:child_process') return {spawnSync(file, args) {
        assert.equal(file, 'git');
        assert.ok(args[0] === 'rev-parse' || args[0] === 'show');
        return {status: 0, stdout: args[0] === 'rev-parse' ? `${checkout}\n` : `${live} ${source}\n`};
      }};
      return require(name);
    }, __dirname, process: {env: context}, Buffer, AbortSignal,
    fetch: async (url, options) => {
      providerReads++;
      assert.equal(url, target.ENDPOINT);
      assert.equal(options.headers.Authorization, `Bearer ${secret}`);
      return new Response(JSON.stringify(providerRef()), {headers: {date: new Date().toUTCString()}});
    }};
    vm.runInNewContext(fs.readFileSync(path.join(__dirname, 'integration-target-witness.cjs'), 'utf8') +
      '\nmodule.exports.main = main;\n', sandbox, {timeout: 1000});
    await sandbox.module.exports.main(`${directory}/${target.FILE}`);
    assert.equal(providerReads, 1);
    const exported = fs.readFileSync(environmentFile, 'utf8');
    assert.match(exported, /^INTEGRATION_TARGET_WITNESS_SHA256=[a-f0-9]{64}\n$/);
    context.INTEGRATION_TARGET_WITNESS_SHA256 = exported.trim().split('=')[1];
    assert.equal(pin(undefined, event, context).checkout_base, live);
    assert.ok(!fs.readFileSync(path.join(directory, target.FILE), 'utf8').includes(secret));
  } finally {
    fs.rmSync(directory, {recursive: true});
  }
});

test('workflow witness is PR-only, after exact checkout and cheap contracts, before any native work', () => {
  const workflow = fs.readFileSync(path.resolve(__dirname, '../../.github/workflows/integration-smoke.yml'),
    'utf8').replace(/\r\n/g, '\n');
  const witnessStep = workflow.split('      - name: Independently witness the exact integration target')[1]
    .split('\n      - name:')[0];
  assert.match(witnessStep, /if: github.event_name == 'pull_request'/);
  assert.match(witnessStep, /GH_TOKEN: \$\{\{ github.token \}\}/);
  assert.match(witnessStep, /node rust\/scripts\/integration-target-witness.cjs/);
  assert.ok(!witnessStep.includes('continue-on-error'));
  assert.equal((workflow.match(/GH_TOKEN:|github.token/g) || []).length, 2);
  assert.match(workflow, /permissions:\n  contents: read/);
  assert.match(workflow, /persist-credentials: false/);
  assert.ok(!workflow.includes('contents: write') && !workflow.includes('pull_request_target'));
  const step = workflow.indexOf('      - name: Independently witness');
  assert.ok(step > workflow.indexOf('ref: ${{ github.sha }}'));
  assert.ok(step > workflow.indexOf('node --test'));
  assert.ok(step < workflow.indexOf('      - name: Build two profiles'));
  assert.ok(step < workflow.indexOf('cargo '));
  assert.ok(workflow.includes('${{ runner.temp }}/integration-target-witness.json'));
});

test('real smoke entry rejects missing or invalid witness before inventories or any native command', async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'target-witness-entry-'));
  try {
    const eventFile = path.join(directory, 'event.json');
    fs.writeFileSync(eventFile, JSON.stringify(event));
    const witness = await acquire();
    const witnessFile = path.join(directory, target.FILE);
    const bytes = JSON.stringify(witness);
    fs.writeFileSync(witnessFile, bytes);
    for (const [label, change, errorCode] of [
      ['missing-digest', {}, 'TARGET_WITNESS_MISSING'],
      ['wrong-digest', {INTEGRATION_TARGET_WITNESS_SHA256: 'a'.repeat(64)}, 'TARGET_WITNESS_DIGEST'],
      ['wrong-run', {INTEGRATION_TARGET_WITNESS_SHA256: target.digest(bytes), GITHUB_RUN_ATTEMPT: '2'},
        'TARGET_WITNESS_BINDING'],
    ]) {
      const artifacts = path.join(directory, label);
      const calls = [];
      const childProcess = {spawnSync(file, args) {
        calls.push([file, args]);
        assert.equal(file, 'git', 'No native process is permitted by this negative fixture');
        const stdout = args[0] === 'rev-parse' ? `${checkout}\n` : `${live} ${source}\n`;
        return {stdout, stderr: '', status: 0, signal: null};
      }};
      const sandbox = {module: {exports: {}}, require(name) {
        if (name === 'node:child_process') return childProcess;
        if (name === './validate-impact.cjs') return {sourceInventory() {
          assert.fail('No source inventory is permitted before the source pin passes');
        }};
        if (name === './integration-target-witness.cjs') return target;
        return require(name);
      }, __dirname, process: {platform: 'win32',
        env: {...env, RUNNER_TEMP: directory, GITHUB_EVENT_PATH: eventFile,
          RUNNER_ENVIRONMENT: 'github-hosted', RUNNER_OS: 'Windows', ...change}}};
      vm.runInNewContext(fs.readFileSync(path.join(__dirname, 'integration-smoke.cjs'), 'utf8') +
        '\nmodule.exports.main = main;\n', sandbox, {timeout: 1000});
      assert.throws(() => sandbox.module.exports.main(artifacts), {code: errorCode});
      assert.equal(calls.length, 2, 'Only HEAD and its ordered parents were inspected');
      const receipt = JSON.parse(fs.readFileSync(path.join(artifacts, 'receipt.json'), 'utf8'));
      assert.equal(receipt.passed, false);
      assert.equal(receipt.source_context.event_base, base);
      assert.deepEqual(receipt.source_context.parents, [live, source]);
      assert.equal(fs.existsSync(path.join(artifacts, 'source-before.json')), false);
      assert.ok(!JSON.stringify(receipt).includes(secret));
    }
  } finally {
    fs.rmSync(directory, {recursive: true});
  }
});

test('all candidate sourcePin callers preserve recorded and independently witnessed hard base pins', () => {
  for (const name of ['dataagent', 'notebook', 'ocv', 'schema', 'update']) {
    const text = fs.readFileSync(path.join(__dirname, `integration-${name}-smoke.cjs`), 'utf8');
    const hardBase = name === 'dataagent' ? 'entry.base' : 'BASE';
    assert.ok(text.includes(`assert.equal(event.pull_request.base.sha, ${hardBase}, 'Integration base moved')`));
    assert.ok(text.includes(`assert.equal(report.source.checkout_base, ${hardBase}, 'Witnessed integration base moved')`));
    assert.ok(text.indexOf('report.source = sourcePin(') < text.indexOf('before = sourceInventory('));
  }
  assert.equal(require('./integration-dataagent-link-smoke.cjs').ENTRY.base,
    '313271f4104a562d853318503f8e14f552ac6dc2');
  assert.equal(require('./integration-kql-link-smoke.cjs').ENTRY.base,
    '61c8221914152e231e5e64f7bd38418f66264999');
});

test('real synthetic Git merge and target refs bind witness; drift and push ancestry stay fail-closed', async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'target-witness-git-'));
  const gitEnv = {...process.env, GIT_AUTHOR_NAME: 'Fixture', GIT_AUTHOR_EMAIL: 'fixture@example.invalid',
    GIT_COMMITTER_NAME: 'Fixture', GIT_COMMITTER_EMAIL: 'fixture@example.invalid',
    GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: path.join(directory, 'no-global-config')};
  const git = (...args) => {
    const result = spawnSync('git', ['-c', `core.hooksPath=${directory}/no-hooks`,
      '-c', 'commit.gpgSign=false', ...args], {cwd: directory, env: gitEnv,
      encoding: 'utf8', shell: false, windowsHide: true, timeout: 10000});
    assert.equal(result.error, undefined);
    assert.equal(result.status, 0, `git ${args.join(' ')}: ${result.stderr}`);
    return result.stdout.trim();
  };
  const commit = name => {
    fs.writeFileSync(path.join(directory, name), `${name}\n`);
    git('add', name);
    git('commit', '-m', name);
    return git('rev-parse', 'HEAD');
  };
  try {
    git('-c', 'init.templateDir=', 'init', '-b', 'target');
    const recorded = commit('initial');
    git('switch', '-c', 'source');
    const head = commit('source-file');
    git('switch', 'target');
    const targetHead = commit('target-file');
    git('switch', '-c', 'tested-merge');
    git('merge', '--no-ff', 'source', '-m', 'fixture-merge');
    const actual = git('rev-parse', 'HEAD');
    const parents = git('show', '-s', '--format=%P', 'HEAD').split(' ');
    assert.deepEqual(parents, [targetHead, head]);
    const input = structuredClone(event);
    input.pull_request.base.sha = recorded;
    input.pull_request.head.sha = head;
    const context = {...env, GITHUB_SHA: actual, RUNNER_TEMP: directory};
    const fetcher = async () => response(providerRef(git('rev-parse', 'refs/heads/target')));
    const witness = await acquire(fetcher, input, context, actual, parents);
    const environmentFile = path.join(directory, 'workflow-env');
    await target.recordWitness(path.join(directory, target.FILE), async () => witness, environmentFile);
    context.INTEGRATION_TARGET_WITNESS_SHA256 = fs.readFileSync(environmentFile, 'utf8').trim().split('=')[1];
    const receipt = pin(undefined, input, context, parents);
    assert.equal(receipt.event_base, recorded);
    assert.equal(receipt.checkout_base, targetHead);
    assert.throws(() => pin(witness, input, context, [...parents].reverse()));
    git('switch', 'target');
    commit('target-moved-once');
    const after = commit('target-moved-twice');
    await assert.rejects(acquire(fetcher, input, context, actual, parents), {code: 'TARGET_DRIFT'});
    assert.equal(pin(witness, input, context, parents).checkout_base, targetHead,
      'Previously acquired immutable same-attempt witness is not re-labelled as a new live read');
    const pushEvent = {repository: event.repository, before: targetHead, after, deleted: false};
    git('merge-base', '--is-ancestor', targetHead, after);
    assert.equal(smoke.sourcePin('push', pushEvent, target.TARGET_REF, after, after,
      git('show', '-s', '--format=%P', after).split(' '), true).base, targetHead);
    assert.throws(() => smoke.sourcePin('push', pushEvent, target.TARGET_REF,
      after, after, parents, false));
    assert.throws(() => smoke.sourcePin('push', {...pushEvent, before: '0'.repeat(40)},
      target.TARGET_REF, after, after, parents, true));
    assert.throws(() => smoke.sourcePin('push', {...pushEvent, deleted: true},
      target.TARGET_REF, after, after, parents, true));
  } finally {
    fs.rmSync(directory, {recursive: true});
  }
});
