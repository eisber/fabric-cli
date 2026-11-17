// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {spawnSync} = require('node:child_process');
const {test} = require('node:test');
const p = require('./rust-native-posthoc.cjs');

const driver = '4'.repeat(40), source = p.REFERENCE_SOURCE, base = 'ce50f369f2713ed1a2599099458f19bb0750d3e3';
const inputs = () => ({source_sha: source, base_sha: base, mode: 'full', route: 'manual', batch_id: ''});
const event = () => ({repository: {full_name: p.REPOSITORY, default_branch: 'main'}, inputs: inputs()});
const environment = () => ({GITHUB_EVENT_NAME: 'workflow_dispatch', GITHUB_REPOSITORY: p.REPOSITORY,
  GITHUB_REF: 'refs/heads/main', GITHUB_WORKFLOW_REF: `${p.REPOSITORY}/${p.WORKFLOW}@refs/heads/main`,
  GITHUB_SERVER_URL: 'https://github.com', GITHUB_API_URL: 'https://api.github.com',
  GITHUB_SHA: driver, GITHUB_WORKFLOW_SHA: driver, GITHUB_JOB: 'prepare',
  GITHUB_RUN_ID: '123', GITHUB_RUN_ATTEMPT: '2', REQUESTED_SOURCE_SHA: source,
  REQUESTED_BASE_SHA: base, REQUESTED_MODE: 'full', REQUESTED_ROUTE: 'manual', REQUESTED_BATCH_ID: ''});
const context = () => p.requestContext(environment(), event(), driver);

function fixture(t, leaf = '') {
  const container = fs.mkdtempSync(path.join(os.tmpdir(), 'posthoc-offline-'));
  t.after(() => fs.rmSync(container, {recursive: true, force: true}));
  const root = leaf ? path.join(container, leaf) : container;
  if (leaf) fs.mkdirSync(root);
  const run = args => {
    const result = spawnSync('git', args, {cwd: root, encoding: 'utf8', shell: false,
      env: p.gitEnvironment(), timeout: 10000});
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stderr);
    return result.stdout.trim();
  };
  run(['init', '--quiet']);
  run(['config', 'core.autocrlf', 'false']);
  run(['config', 'user.name', 'Offline fixture']);
  run(['config', 'user.email', 'fixture@example.invalid']);
  run(['remote', 'add', 'origin', `https://github.com/${p.REPOSITORY}.git`]);
  fs.writeFileSync(path.join(root, 'Cargo.lock'), 'offline fixture only\n');
  fs.writeFileSync(path.join(root, '.gitignore'), 'ignored-output\n');
  run(['add', '.']);
  run(['-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m', 'Offline fixture, not native evidence']);
  return {root, run};
}

function stageFixture(t) {
  const sourceFixture = fixture(t, 'source'), workspace = path.dirname(sourceFixture.root);
  const driverRoot = path.join(workspace, 'driver');
  fs.mkdirSync(driverRoot);
  const request = {...context(), source: sourceFixture.run(['rev-parse', 'HEAD'])};
  const env = {...environment(), GITHUB_WORKSPACE: workspace,
    POSTHOC_ARTIFACTS: path.join(workspace, 'receipts'), GITHUB_OUTPUT: path.join(workspace, 'output')};
  // Only unavailable provider/main-lineage/bridge boundaries are fixtures.
  // Actual source Git, filesystem guards, inventories and receipt writes run.
  const services = {readContext: () => request, resolveHead: async () => request.source,
    verifyBridge: inventory => inventory.files,
    clean: (root, commands) => root === driverRoot ? undefined : p.clean(root, commands),
    git: (root, args, commands) => {
      if (root === driverRoot) {
        assert.deepEqual(args, ['rev-parse', 'HEAD']);
        return driver;
      }
      if (args[0] === 'merge-base') return '';
      return p.git(root, args, commands);
    }};
  const read = stage => JSON.parse(fs.readFileSync(path.join(env.POSTHOC_ARTIFACTS, `${stage}.json`), 'utf8'));
  return {...sourceFixture, driverRoot, workspace, request, env, services, read};
}

test('default-main genuine manual context keeps driver, DUT, base, event and attempt separate', () => {
  assert.deepEqual(context(), {repository: p.REPOSITORY, branch: p.BRANCH, event: 'workflow_dispatch',
    ref: 'refs/heads/main', driver, driver_base: p.MAIN_BASE,
    workflow_ref: `${p.REPOSITORY}/${p.WORKFLOW}@refs/heads/main`, workflow_sha: driver,
    source, base, anchor: p.ANCHOR, mode: 'full', route: 'manual', batch_id: null,
    run_id: '123', job: 'prepare', run_attempt: '2'});
});

test('batched mode is a real dispatch with explicit batch provenance, not a synthetic push', () => {
  const env = {...environment(), REQUESTED_ROUTE: 'batch', REQUESTED_BATCH_ID: 'merged-17.1',
    REQUESTED_MODE: 'affected'};
  const ev = {...event(), inputs: {...inputs(), route: 'batch', batch_id: 'merged-17.1', mode: 'affected'}};
  const request = p.requestContext(env, ev, driver);
  assert.equal(request.route, 'batch');
  assert.equal(request.batch_id, 'merged-17.1');
  assert.equal(request.event, 'workflow_dispatch');
  assert.equal(request.mode, 'affected');
  assert.equal(p.debt(request.mode).admitted, false);
});

for (const value of [undefined, null, 12, '', '0'.repeat(40), source.slice(0, 7),
  source.toUpperCase(), ` ${source}`, `${source}\n`, `${source};echo unsafe`, '--help',
  `$(echo ${source})`, 'refs/heads/main', `${source}/../main`]) {
  test(`invalid pin rejected: ${JSON.stringify(value)}`, () => {
    assert.throws(() => p.sha(value));
    for (const [input, variable] of [['source_sha', 'REQUESTED_SOURCE_SHA'], ['base_sha', 'REQUESTED_BASE_SHA']]) {
      assert.throws(() => p.requestContext({...environment(), [variable]: value},
        {...event(), inputs: {...inputs(), [input]: value}}, driver));
    }
  });
}

for (const [key, value] of [
  ['GITHUB_EVENT_NAME', 'push'], ['GITHUB_EVENT_NAME', 'schedule'],
  ['GITHUB_EVENT_NAME', 'pull_request'], ['GITHUB_EVENT_NAME', 'repository_dispatch'],
  ['GITHUB_REPOSITORY', 'foreign/fork'], ['GITHUB_REF', `refs/heads/${p.BRANCH}`],
  ['GITHUB_REF', 'refs/heads/main-other'], ['GITHUB_SHA', source],
  ['GITHUB_WORKFLOW_SHA', source], ['GITHUB_WORKFLOW_REF', `${p.REPOSITORY}/${p.WORKFLOW}@main`],
  ['GITHUB_SERVER_URL', 'https://foreign.invalid'], ['GITHUB_API_URL', 'https://foreign.invalid'],
  ['GITHUB_JOB', 'native'], ['GITHUB_RUN_ID', '0'], ['GITHUB_RUN_ATTEMPT', '-1'],
]) {
  test(`foreign or unsupported context rejected: ${key}=${value}`, () => {
    assert.throws(() => p.requestContext({...environment(), [key]: value}, event(), driver));
  });
}

test('source, driver, base, dispatch and provider pin mismatches fail closed', () => {
  assert.throws(() => p.requestContext(environment(), event(), source));
  for (const repository of [{full_name: 'foreign/fork', default_branch: 'main'},
    {full_name: p.REPOSITORY, default_branch: p.BRANCH}]) {
    assert.throws(() => p.requestContext(environment(), {...event(), repository}, driver));
  }
  for (const changed of [{source_sha: driver}, {base_sha: source}, {mode: 'FULL'}, {route: 'scheduled'},
    {source_sha: base}, {command: 'cargo test'}, {budget: 'unlimited'}, {batch_id: 'unexpected'}]) {
    assert.throws(() => p.requestContext(environment(), {...event(), inputs: {...inputs(), ...changed}}, driver));
  }
  assert.throws(() => p.matchResolved(context(), base), /fixed branch head/);
  assert.equal(p.matchResolved(context(), source).resolved_source, source);
  for (const [actual, anchor, ancestor] of [[driver, true, true], [source, false, true], [source, true, false]]) {
    assert.throws(() => p.proveLineage(context(), actual, anchor, ancestor));
  }
  p.proveLineage(context(), source, true, true);
});

test('invalid batch text, injection and silently defaulted full mode are not accepted', () => {
  for (const batch_id of ['', 'a'.repeat(65), 'batch\nother', '--help', '$(pwd)', '../../main']) {
    assert.throws(() => p.requestContext({...environment(), REQUESTED_ROUTE: 'batch', REQUESTED_BATCH_ID: batch_id},
      {...event(), inputs: {...inputs(), route: 'batch', batch_id}}, driver));
  }
  for (const field of ['mode', 'route', 'source_sha', 'base_sha', 'batch_id']) {
    const incomplete = inputs();
    delete incomplete[field];
    assert.throws(() => p.requestContext(environment(), {...event(), inputs: incomplete}, driver));
  }
});

test('fixed provider lookup is read-only, bounded, authenticated and never redirects', async () => {
  const resolved = await p.resolveHead('fixture-token-not-a-credential', async (url, opts) => {
    assert.equal(url, `https://api.github.com/repos/${p.REPOSITORY}/branches/${encodeURIComponent(p.BRANCH)}`);
    assert.equal(opts.method, 'GET');
    assert.equal(opts.redirect, 'error');
    assert.equal(opts.headers.Authorization, 'Bearer fixture-token-not-a-credential');
    assert.ok(opts.signal instanceof AbortSignal);
    return {status: 200, json: async () => ({name: p.BRANCH, commit: {sha: source}})};
  });
  assert.equal(resolved, source);
  await assert.rejects(p.resolveHead('', () => assert.fail('No anonymous fallback')));
  for (const response of [{status: 403}, {status: 404}, {status: 500},
    {status: 200, json: async () => ({name: 'main', commit: {sha: source}})},
    {status: 200, json: async () => ({name: p.BRANCH, commit: {sha: '0'.repeat(40)}})}]) {
    await assert.rejects(p.resolveHead('fixture', async () => response));
  }
  for (const message of ['provider timeout', 'provider canceled', 'network unavailable']) {
    await assert.rejects(p.resolveHead('fixture', async () => { throw new Error(message); }), new RegExp(message));
  }
});

test('Git subprocess boundary strips all tokens, event overrides, hooks and command injection', () => {
  const env = p.gitEnvironment({PATH: '/fixture/bin', GH_TOKEN: 'never-forward',
    GITHUB_TOKEN: 'never-forward', FAB_TOKEN: 'never-forward', RUSTC_WRAPPER: 'unsafe',
    GIT_CONFIG_COUNT: '1', GITHUB_SHA: source, CARGO_HOME: '/unsafe', NODE_OPTIONS: '--require unsafe'});
  assert.equal(env.PATH, '/fixture/bin');
  for (const name of ['GH_TOKEN', 'GITHUB_TOKEN', 'FAB_TOKEN', 'RUSTC_WRAPPER', 'GIT_CONFIG_COUNT',
    'GITHUB_SHA', 'CARGO_HOME', 'NODE_OPTIONS']) assert.equal(Object.hasOwn(env, name), false);
  assert.equal(env.GIT_NO_REPLACE_OBJECTS, '1');
  assert.equal(env.GIT_CONFIG_GLOBAL, process.platform === 'win32' ? 'NUL' : os.devNull);
  assert.equal(env.GIT_TERMINAL_PROMPT, '0');
});

test('actual clean Git bytes and manifest-lock inventory are hashed, not a mocked native run', t => {
  const {root, run} = fixture(t), commands = [];
  assert.equal(p.repositoryRoot(root, commands), fs.realpathSync(root));
  const before = p.snapshot(root, commands);
  assert.equal(before.head, run(['rev-parse', 'HEAD']));
  assert.equal(before.tree, run(['rev-parse', 'HEAD^{tree}']));
  assert.equal(before.files.find(file => file.path === 'Cargo.lock').bytes, 21);
  assert.match(before.files.find(file => file.path === 'Cargo.lock').sha256, /^[a-f0-9]{64}$/);
  p.unchanged(before, p.snapshot(root));
  assert.ok(commands.length > 0 && commands.every(command => command.program === 'git' && command.exit === 0));
  assert.throws(() => p.verifyBridge(before), /Missing source input/);
});

test('dirty, staged, ignored, untracked and stat-cache-hidden source mutations fail', t => {
  const {root, run} = fixture(t);
  fs.writeFileSync(path.join(root, 'ignored-output'), 'not evidence');
  assert.throws(() => p.snapshot(root), /dirty|ignored/);
  fs.unlinkSync(path.join(root, 'ignored-output'));
  fs.writeFileSync(path.join(root, 'untracked'), 'not evidence');
  assert.throws(() => p.snapshot(root), /dirty/);
  fs.unlinkSync(path.join(root, 'untracked'));
  fs.writeFileSync(path.join(root, 'Cargo.lock'), 'changed bytes\n');
  assert.throws(() => p.snapshot(root), /dirty/);
  run(['add', 'Cargo.lock']);
  assert.throws(() => p.snapshot(root), /dirty/);
  run(['-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m', 'Changed fixture']);
  run(['update-index', '--assume-unchanged', 'Cargo.lock']);
  fs.writeFileSync(path.join(root, 'Cargo.lock'), 'hidden mutation\n');
  assert.throws(() => p.snapshot(root), /Actual source bytes differ/);
});

test('committed source changes invalidate inventory and never reuse a watermark', t => {
  const {root, run} = fixture(t), before = p.snapshot(root);
  fs.writeFileSync(path.join(root, 'Cargo.lock'), 'new fixture\n');
  run(['add', '.']);
  run(['-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m', 'New fixture source']);
  const after = p.snapshot(root);
  assert.notEqual(after.sha256, before.sha256);
  assert.throws(() => p.unchanged(before, after), /DUT revision changed/);
  assert.throws(() => p.unchanged(before, {...before, sha256: 'changed'}), /inventory changed/);
  assert.equal(p.debt('affected').watermark, null);
});

test('real declared text/CRLF checkout matches pinned blob while retaining raw byte hashes', t => {
  const {root, run} = fixture(t);
  fs.writeFileSync(path.join(root, '.gitattributes'), '*.bim text eol=crlf\n');
  fs.writeFileSync(path.join(root, 'model.bim'), '{\n  "fixture": true\n}\n');
  run(['add', '.gitattributes', 'model.bim']);
  run(['-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m', 'Declared CRLF fixture']);
  fs.unlinkSync(path.join(root, 'model.bim'));
  run(['checkout-index', '--force', '--index', '--', 'model.bim']);
  const actual = fs.readFileSync(path.join(root, 'model.bim'));
  assert.equal(actual.toString(), '{\r\n  "fixture": true\r\n}\r\n');
  const before = p.snapshot(root);
  const model = before.files.find(file => file.path === 'model.bim');
  assert.equal(model.bytes, 25);
  assert.equal(model.blob, run(['rev-parse', 'HEAD:model.bim']));
  assert.equal(model.checkout_representation, 'declared-text-crlf');
  p.unchanged(before, p.snapshot(root));
  run(['update-index', '--assume-unchanged', 'model.bim']);
  fs.writeFileSync(path.join(root, 'model.bim'), '{\n  "fixture": true\n}\n');
  assert.throws(() => p.snapshot(root), /declared CRLF checkout bytes/);
  fs.writeFileSync(path.join(root, 'model.bim'), '{\r\n  "fixture": false\r\n}\r\n');
  assert.throws(() => p.snapshot(root), /dirty|Actual source bytes differ/);
});

test('unsupported filters, ident and encodings fail without executing a source-defined transform', t => {
  const {root, run} = fixture(t);
  const attributes = {text: 'set', eol: 'crlf', filter: 'unspecified',
    ident: 'unspecified', 'working-tree-encoding': 'unspecified'};
  for (const [key, value] of [['filter', 'arbitrary'], ['ident', 'set'],
    ['working-tree-encoding', 'UTF-16']]) {
    assert.throws(() => p.verifyCheckoutBytes(Buffer.from('fixture\r\n'), source,
      {...attributes, [key]: value}, 'fixture'), /Unsupported checkout transformation/);
  }
  fs.writeFileSync(path.join(root, '.gitattributes'), 'Cargo.lock filter=never-execute\n');
  run(['add', '.gitattributes']);
  run(['-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m', 'Unsupported filter fixture']);
  assert.throws(() => p.snapshot(root), /Unsupported checkout transformation filter/);
});

test('foreign checkout origin and symlink/submodule tree entries cannot qualify', t => {
  const {root, run} = fixture(t);
  run(['remote', 'set-url', 'origin', 'https://github.com/foreign/fork.git']);
  assert.throws(() => p.repositoryRoot(root), /foreign repository/);
  const blob = run(['rev-parse', 'HEAD:Cargo.lock']);
  run(['update-index', '--add', '--cacheinfo', `120000,${blob},link`]);
  run(['-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m', 'Symlink metadata fixture']);
  assert.throws(() => p.snapshot(root));
});

test('helper API pin change fails rather than copying or silently trusting another executor', () => {
  const files = [...Object.entries(p.BRIDGE_BLOBS).map(([name, blob]) => ({path: name, blob})),
    ...['rust/Cargo.toml', 'rust/Cargo.lock', 'rust/rust-toolchain.toml',
      'rust/validation/impact-graph.json'].map(name => ({path: name, blob: '5'.repeat(40)}))];
  assert.equal(p.verifyBridge({files}).length, 12);
  const changed = files.map(file => ({...file}));
  changed[0].blob = '6'.repeat(40);
  assert.throws(() => p.verifyBridge({files: changed}), /API changed/);
});

test('affected without native baseline stays NOT_ADMITTED; even full gets no implicit budget', () => {
  for (const mode of ['full', 'affected']) {
    const receipt = p.debt(mode);
    assert.equal(receipt.status, 'NOT_ADMITTED');
    assert.equal(receipt.native_status, 'UNRUN');
    assert.equal(receipt.admitted, false);
    assert.equal(receipt.selection, null);
    assert.equal(receipt.selection_digest, null);
    assert.equal(receipt.native_baseline, null);
    assert.equal(receipt.complete_aggregate, false);
    assert.equal(receipt.watermark, null);
    assert.deepEqual(receipt.executed_commands, []);
    assert.equal(receipt.tests_passed, 0);
    assert.deepEqual(receipt.coverage.map(row => [row.platform, row.status, row.steps, row.acquisition, row.arch]),
      [['linux', 'UNRUN', 0, null, null], ['macos', 'UNRUN', 0, null, null]]);
    assert.deepEqual(receipt.resource_policy, {enabled: false, maxShards: 0, maxParallel: 0,
      maxTests: 0, nativeBaseline: null});
    assert.deepEqual(receipt.future_execution_ceiling, {job_seconds: 900, executor_seconds: 720, command_seconds: 600});
  }
  assert.ok(p.debt('affected').reasons.includes('full-native-baseline-required'));
  assert.throws(() => p.debt('unknown'));
});

test('artifact paths are external, bounded and exclusive; failures cannot overwrite earlier proof', t => {
  const {root} = fixture(t), output = path.join(os.tmpdir(), `posthoc-artifact-${path.basename(root)}`);
  for (const value of ['', '.', root, path.join(root, 'artifacts')]) {
    assert.throws(() => p.artifactDirectory(value, [root]));
  }
  assert.equal(p.artifactDirectory(output, [root]), output);
  const file = path.join(root, 'receipt.json');
  p.writeJson(file, {status: 'UNRUN'});
  assert.equal(JSON.parse(fs.readFileSync(file)).status, 'UNRUN');
  assert.throws(() => p.writeJson(file, {status: 'passed'}), /EEXIST/);
  assert.throws(() => p.writeJson(path.join(root, 'oversize.json'),
    {output: 'x'.repeat(p.LIMITS.artifactBytes)}), /byte budget/);
});

test('real request orchestration writes immutable-source UNRUN artifacts and exits non-green', async t => {
  const f = stageFixture(t);
  assert.equal(await p.runStage('pin', f.driverRoot, f.env, f.services), 0);
  assert.equal(fs.readFileSync(f.env.GITHUB_OUTPUT, 'utf8'), `source_sha=${f.request.source}\n`);
  assert.equal(f.read('pin').status, 'PINNED_UNRUN');
  assert.equal(await p.runStage('inspect', f.driverRoot, f.env, f.services), 78);
  const receipt = f.read('inspect');
  assert.equal(receipt.status, 'NOT_ADMITTED');
  assert.equal(receipt.source_verified, true);
  assert.equal(receipt.native_status, 'UNRUN');
  assert.equal(receipt.complete_aggregate, false);
  assert.equal(receipt.context.source, f.request.source);
  assert.notEqual(receipt.context.source, receipt.context.driver);
  assert.equal(receipt.source_before, receipt.source_after);
  assert.equal(receipt.freshness.source_still_current, true);
  assert.ok(receipt.guard_commands.length > 0);
  assert.ok(receipt.guard_commands.every(command => command.program === 'git'));
  assert.deepEqual(receipt.executed_commands, []);
  assert.equal(receipt.tests_passed, 0);
  const before = JSON.parse(fs.readFileSync(path.join(f.env.POSTHOC_ARTIFACTS, 'source-before.json')));
  const after = JSON.parse(fs.readFileSync(path.join(f.env.POSTHOC_ARTIFACTS, 'source-after.json')));
  assert.equal(before.head, f.request.source);
  assert.equal(after.sha256, before.sha256);
  assert.equal(before.sha256, receipt.source_before);
  assert.equal(receipt.watermark, null);
});

test('later target movement is recorded stale, without repinning or claiming native coverage', async t => {
  const f = stageFixture(t);
  await p.runStage('pin', f.driverRoot, f.env, f.services);
  assert.equal(await p.runStage('inspect', f.driverRoot, f.env,
    {...f.services, resolveHead: async () => base}), 78);
  const receipt = f.read('inspect');
  assert.equal(receipt.context.source, f.request.source);
  assert.equal(receipt.freshness.observed_head, base);
  assert.equal(receipt.freshness.source_still_current, false);
  assert.equal(receipt.status, 'NOT_ADMITTED');
  assert.equal(receipt.watermark, null);
});

test('moved initial head and provider failure retain partial non-admission receipts', async t => {
  for (const [resolveHead, expected] of [
    [async () => base, 'GUARD_FAILURE'],
    [async () => { throw new Error('HTTP403 fixture'); }, 'PROVIDER_LOOKUP_FAILURE_UNRUN'],
    [async () => { throw new Error('timeout fixture'); }, 'PROVIDER_LOOKUP_FAILURE_UNRUN'],
    [async () => { throw new Error('canceled lookup fixture'); }, 'PROVIDER_LOOKUP_FAILURE_UNRUN'],
  ]) {
    const f = stageFixture(t);
    assert.equal(await p.runStage('pin', f.driverRoot, f.env, {...f.services, resolveHead}), 1);
    const receipt = f.read('pin');
    assert.equal(receipt.status, expected);
    assert.equal(receipt.native_status, 'UNRUN');
    assert.equal(receipt.admitted, false);
    assert.ok(receipt.error);
    assert.equal(fs.existsSync(f.env.GITHUB_OUTPUT), false);
  }
});

test('source mutation on a failed inspection stays failed with before evidence retained', async t => {
  const f = stageFixture(t);
  await p.runStage('pin', f.driverRoot, f.env, f.services);
  const mutate = async () => {
    fs.writeFileSync(path.join(f.root, 'Cargo.lock'), 'mutated fixture\n');
    return f.request.source;
  };
  assert.equal(await p.runStage('inspect', f.driverRoot, f.env,
    {...f.services, resolveHead: mutate}), 1);
  const receipt = f.read('inspect');
  assert.equal(receipt.status, 'SOURCE_INTEGRITY_FAILURE');
  assert.equal(receipt.native_status, 'UNRUN');
  assert.match(receipt.source_before, /^[a-f0-9]{64}$/);
  assert.equal(receipt.source_after, undefined);
  assert.ok(fs.existsSync(path.join(f.env.POSTHOC_ARTIFACTS, 'source-before.json')));
  assert.equal(receipt.watermark, null);
});

test('stale or foreign pin artifacts fail dispatch/run/attempt binding, not silently refresh', async t => {
  for (const [key, value] of [['source', base], ['driver', source], ['run_attempt', '1'],
    ['repository', 'foreign/fork'], ['base', driver]]) {
    const f = stageFixture(t);
    await p.runStage('pin', f.driverRoot, f.env, f.services);
    const pin = f.read('pin');
    pin.context[key] = value;
    fs.writeFileSync(path.join(f.env.POSTHOC_ARTIFACTS, 'pin.json'), JSON.stringify(pin));
    assert.equal(await p.runStage('inspect', f.driverRoot, f.env, f.services), 1);
    assert.equal(f.read('inspect').status, 'GUARD_FAILURE');
    assert.equal(f.read('inspect').watermark, null);
  }
});

test('missing pin, missing checkout and unavailable freshness never produce a complete receipt', async t => {
  const missing = stageFixture(t);
  fs.mkdirSync(missing.env.POSTHOC_ARTIFACTS);
  assert.equal(await p.runStage('inspect', missing.driverRoot, missing.env, missing.services), 1);
  assert.equal(missing.read('inspect').status, 'GUARD_FAILURE');
  const failed = stageFixture(t);
  await p.runStage('pin', failed.driverRoot, failed.env, failed.services);
  assert.equal(await p.runStage('inspect', failed.driverRoot, failed.env,
    {...failed.services, resolveHead: async () => { throw new Error('unavailable fixture'); }}), 1);
  assert.equal(failed.read('inspect').status, 'PROVIDER_LOOKUP_FAILURE_UNRUN');
  assert.equal(failed.read('inspect').complete_aggregate, false);
  assert.equal(fs.existsSync(path.join(failed.env.POSTHOC_ARTIFACTS, 'source-before.json')), true);
  assert.throws(() => p.fixedRoots(failed.root, failed.workspace), /fixed isolated checkout/);
  assert.throws(() => p.fixedRoots(failed.driverRoot, path.join(failed.workspace, 'missing')));
});

test('untrusted proof, baseline and budget environment text cannot admit even a fixture request', async t => {
  for (const proof of [null, [], [{status: 'skipped', steps: 0}],
    [{status: 'passed', tests_passed: 0}], [{status: 'passed', tests_ignored: 99}],
    [{head: base, status: 'passed'}], [{head: source}, {head: source}],
    [{status: 'passed', filter: 'unmatched'}]]) {
    const f = stageFixture(t);
    f.env.NATIVE_RECEIPTS = JSON.stringify(proof);
    f.env.NATIVE_BASELINE = source;
    f.env.NATIVE_RESOURCE_POLICY = '{"enabled":true,"maxShards":999999}';
    await p.runStage('pin', f.driverRoot, f.env, f.services);
    assert.equal(await p.runStage('inspect', f.driverRoot, f.env, f.services), 78);
    assert.equal(f.read('inspect').admitted, false);
    assert.equal(f.read('inspect').watermark, null);
    assert.equal(f.read('inspect').tests_passed, 0);
  }
});

test('legacy CLI has no generic executor, baseline-proof intake, arbitrary path or untrusted receipt route', async () => {
  for (const argv of [[], ['run'], ['execute'], ['aggregate'], ['schedule'], ['pin', '--execute'],
    ['inspect', '--root', '/unsafe'], ['--baseline', 'forged.json']]) {
    await assert.rejects(p.main(argv, {}), /Usage/);
  }
  const result = spawnSync(process.execPath, [path.join(__dirname, 'rust-native-posthoc.cjs'), '--help'],
    {encoding: 'utf8', timeout: 10000, env: p.gitEnvironment()});
  assert.equal(result.status, 0);
  assert.match(result.stdout, /Inspect exits 78 \(NOT_ADMITTED\)/);
  assert.match(result.stdout, /native execution is UNRUN/);
});

test('workflow separates cheap PR contracts from genuine default-main finite partial native dispatch', () => {
  const workflow = fs.readFileSync(path.join(__dirname, '../workflows/rust-native-posthoc.yml'), 'utf8');
  const commands = [...workflow.matchAll(/^\s+(?:- )?run: (.+)$/gm)].map(match => match[1]);
  assert.deepEqual(commands, ['node --test .github/scripts/rust-native-posthoc.test.cjs .github/scripts/integration-smoke-recovery.test.cjs',
    ...['bootstrap-pin', 'bootstrap-inspect', 'bootstrap-refresh', 'bootstrap', 'bootstrap-finalize']
      .map(stage => `node driver/.github/scripts/rust-native-posthoc.cjs ${stage}`)]);
  assert.ok(commands.every(command => !command.includes('${{')));
  assert.equal((workflow.match(/persist-credentials: false/g) || []).length, 5);
  assert.equal((workflow.match(/^\s+contents: read$/gm) || []).length, 1);
  assert.match(workflow, /ref: \$\{\{ steps\.pin\.outputs\.source_sha \}\}\s+path: source/);
  assert.match(workflow, /if: always\(\)/);
  assert.match(workflow, /if-no-files-found: error/);
  assert.doesNotMatch(workflow, /^\s+(schedule|cron|repository_dispatch):/m);
  assert.doesNotMatch(workflow, /continue-on-error|secrets: inherit|cargo |rustc |--execute/);
  assert.match(workflow, /max-parallel: 2/);
  assert.match(workflow, /timeout-minutes: 15/);
  assert.match(workflow, /fail-fast: false/);
  assert.match(workflow, /actions: read/);
  assert.match(workflow, /ref: \$\{\{ needs\.prepare\.outputs\.source_sha \}\}/);
  for (const section of workflow.split(/\n  (?:prepare|native):/).slice(1)) {
    const header = section.split(/\n    steps:/)[0];
    assert.doesNotMatch(header, /\$\{\{ runner\./,
      'Runner context is step-only, not available in job-level env');
    const condition = header.match(/^    if: (.+)$/m)?.[1];
    assert.ok(condition?.includes('github.run_attempt == 1'), 'Gate before any native runner allocation');
    const admission = new Function('github', `return (${condition});`);
    assert.equal(admission({event_name: 'workflow_dispatch', ref: 'refs/heads/main', run_attempt: 1}), true);
    for (const run_attempt of [2, 3]) {
      assert.equal(admission({event_name: 'workflow_dispatch', ref: 'refs/heads/main', run_attempt}), false);
    }
  }
});

function bootstrapFixture() {
  const env = {...environment(), GITHUB_RUN_ATTEMPT: '1', REQUESTED_MODE: 'bootstrap',
    REQUESTED_DRIVER_SHA: driver};
  const ev = {...event(), inputs: {...inputs(), mode: 'bootstrap', workload_id: p.POLICY.id,
    driver_sha: driver, platforms: 'both', repair_of: ''}};
  return {env, ev, context: p.requestContext(env, ev, driver, true)};
}

test('fixed bootstrap context is a distinct policy, never affected/full or an input-defined budget', () => {
  const {env, ev, context: c} = bootstrapFixture();
  assert.deepEqual(c.platforms, ['linux', 'macos']);
  assert.equal(c.workload_id, 'bootstrap-v1');
  assert.equal(c.repair_of, null);
  assert.equal(p.POLICY.nativeBaseline, null);
  assert.throws(() => p.requestContext(env, ev, driver));
  for (const change of [{mode: 'affected'}, {mode: 'full'}, {platforms: 'windows'},
    {platforms: 'linux,macos'}, {workload_id: 'full'}, {maxTests: '999999'}, {repair_of: '0'}]) {
    assert.throws(() => p.requestContext(env, {...ev, inputs: {...ev.inputs, ...change}}, driver, true));
  }
  assert.throws(() => p.requestContext({...env, GITHUB_RUN_ATTEMPT: '2'}, ev, driver, true));
  const native = p.requestContext({...env, GITHUB_JOB: 'native'}, ev, driver, true);
  assert.deepEqual(p.portableContext(native), p.portableContext(c));
});

test('reviewed driver A cannot silently become dispatched default-main B in any stage', () => {
  const {env, ev, context: c} = bootstrapFixture();
  assert.equal(c.expected_driver, driver);
  for (const GITHUB_JOB of ['prepare', 'native']) {
    const moved = '8'.repeat(40);
    assert.throws(() => p.requestContext({...env, GITHUB_JOB,
      GITHUB_SHA: moved, GITHUB_WORKFLOW_SHA: moved}, ev, moved, true), /reviewed driver pin/);
    assert.throws(() => p.requestContext({...env, GITHUB_JOB, REQUESTED_DRIVER_SHA: moved},
      ev, driver, true), /driver input\/environment/);
    assert.throws(() => p.requestContext({...env, GITHUB_JOB},
      {...ev, inputs: {...ev.inputs, driver_sha: moved}}, driver, true));
  }
});

test('native admission checks actual host, architecture, hosted image and never accepts Windows/cross-host', () => {
  const env = {RUNNER_ENVIRONMENT: 'github-hosted', RUNNER_OS: 'Linux', RUNNER_ARCH: 'X64',
    POSTHOC_IMAGE: 'ubuntu-24.04', ImageVersion: 'offline-fixture-not-a-runner'};
  assert.equal(p.bootstrapHost('linux', env, {platform: 'linux', arch: 'x64'}).triple,
    'x86_64-unknown-linux-gnu');
  for (const change of [{RUNNER_ENVIRONMENT: 'self-hosted'}, {RUNNER_OS: 'macOS'},
    {RUNNER_ARCH: 'ARM64'}, {POSTHOC_IMAGE: 'ubuntu-latest'}, {ImageVersion: ''}]) {
    assert.throws(() => p.bootstrapHost('linux', {...env, ...change}, {platform: 'linux', arch: 'x64'}));
  }
  for (const actual of [{platform: 'win32', arch: 'x64'}, {platform: 'linux', arch: 'arm64'}]) {
    assert.throws(() => p.bootstrapHost('linux', env, actual));
  }
  assert.equal(p.bootstrapHost('macos', {...env, RUNNER_OS: 'macOS', RUNNER_ARCH: 'ARM64',
    POSTHOC_IMAGE: 'macos-15'}, {platform: 'darwin', arch: 'arm64'}).triple, 'aarch64-apple-darwin');
});

test('bootstrap inputs re-audit exact existing APIs and all unmodified core assertions independently of9ff', () => {
  const files = Object.entries(p.BOOTSTRAP_BLOBS).map(([name, blob]) => ({path: name, blob}));
  files.push(...['rust/Cargo.toml', 'rust/Cargo.lock', 'rust/rust-toolchain.toml',
    'rust/clients/cli/app/Cargo.toml'].map(name => ({path: name, blob: base})));
  p.verifyBootstrapInputs({files});
  assert.equal(Object.hasOwn(p.BOOTSTRAP_BLOBS, 'rust/scripts/impact-ci.cjs'), false);
  assert.throws(() => p.verifyBootstrapInputs({files: files.slice(1)}));
  assert.throws(() => p.verifyBootstrapInputs({files: files.map((item, index) =>
    index === 0 ? {...item, blob: base} : item)}), /re-audit/);
});

test('exact complete core suite rejects empty, list-only, duplicate, filtered, ignored and partial output', () => {
  const lines = p.CORE_TESTS.map(name => `test ${name} ... ok`).join('\n');
  const summary = `test result: ok. ${p.CORE_TESTS.length} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n`;
  const complete = `${lines}\n\n${summary}`;
  assert.equal(p.verifyCoreOutput(complete).passed, 21);
  for (const text of ['', summary, lines, complete.replace('0 ignored', '1 ignored'),
    complete.replace('0 filtered out', '1 filtered out'), complete.replace('0 failed', '1 failed'),
    complete.replace(lines.split('\n')[0], ''), `${complete}${summary}`,
    `${complete}test ${p.CORE_TESTS[0]} ... ok\n`, complete.replace(' ... ok', ' ... ignored')]) {
    assert.throws(() => p.verifyCoreOutput(text));
  }
});

test('bounded read-only provider facts fail on errors, malformed/oversize JSON and foreign redirects', async () => {
  assert.deepEqual(await p.providerJson('branches/main', 'offline-token', async (url, opts) => {
    assert.equal(url, `https://api.github.com/repos/${p.REPOSITORY}/branches/main`);
    assert.equal(opts.redirect, 'error');
    assert.equal(opts.method, 'GET');
    assert.equal(opts.headers.Authorization, 'Bearer offline-token');
    return {status: 200, text: async () => '{"name":"main"}'};
  }), {name: 'main'});
  for (const response of [{status: 403}, {status: 200, text: async () => 'not JSON'},
    {status: 200, text: async () => 'x'.repeat(p.LIMITS.artifactBytes + 1)}]) {
    await assert.rejects(p.providerJson('branches/main', 'offline-token', async () => response));
  }
});

function historyRun(context, now, overrides = {}) {
  return {id: 123, head_sha: driver, display_title: p.runTitle(context), run_attempt: 1,
    event: 'workflow_dispatch', head_branch: 'main', status: 'in_progress',
    created_at: new Date(now - 30000).toISOString(),
    updated_at: new Date(now - 10000).toISOString(), ...overrides};
}

function admit(context, runs, now) {
  return p.admitHistory(context, {total_count: runs.length, workflow_runs: runs}, now);
}

test('provider history enforces durable source/platform dedupe and conservative daily finite reservations', () => {
  const {context: c} = bootstrapFixture();
  const now = Date.parse('2026-11-17T06:00:00Z');
  const current = historyRun(c, now);
  const prior = {id: 122, head_sha: '8'.repeat(40), run_attempt: 1,
    created_at: new Date(now - 7 * 3600000).toISOString(), event: 'workflow_dispatch',
    updated_at: new Date(now - 6 * 3600000).toISOString(),
    head_branch: 'main', status: 'completed', conclusion: 'failure',
    display_title: `posthoc:${base}:bootstrap-v1:both:initial`};
  assert.equal(admit(c, [current], now).reserved_runner_minutes_24h, 30);
  assert.equal(admit(c, [current, prior], now).reserved_platform_jobs_24h, 4);
  for (const runs of [[], [current, current], [current, {...prior, created_at: 'malformed'}],
    [current, {...prior, created_at: new Date(now - 3600000).toISOString()}],
    [current, {...prior, display_title: p.runTitle(c)}],
    [current, prior, {...prior, id: 121}], [{...current, head_sha: base}]]) {
    assert.throws(() => admit(c, runs, now));
  }
  const oldDuplicate = {...prior, display_title: p.runTitle(c),
    created_at: new Date(now - 4 * 86400000).toISOString()};
  assert.throws(() => admit(c, [current, oldDuplicate], now), /Duplicate/);
  const repair = {...c, repair_of: '122'};
  const matching = {...prior, display_title: p.runTitle(c)};
  assert.throws(() => admit(repair,
    [{...current, display_title: p.runTitle(repair)}, matching], now), /Retries are disabled/);
});

test('truncated, inconsistent, duplicated or historical-attempt2 provider coverage fails before admission', () => {
  const {context: c} = bootstrapFixture();
  const now = Date.parse('2026-11-17T06:00:00Z');
  const current = historyRun(c, now);
  for (const history of [{total_count: 3, workflow_runs: [current]},
    {total_count: 0, workflow_runs: [current]}, {total_count: 1, workflow_runs: []},
    {total_count: 101, workflow_runs: [current]}, {workflow_runs: [current]},
    {total_count: 1, workflow_runs: [null]}, {total_count: 2, workflow_runs: [current, current]}]) {
    assert.throws(() => p.admitHistory(c, history, now));
  }
  const prior = historyRun(c, now, {id: 122, head_sha: '8'.repeat(40), run_attempt: 2,
    display_title: `posthoc:${base}:bootstrap-v1:both:initial`});
  assert.throws(() => admit(c, [current, prior], now), /Historical rerun allocation is unknown/);
  assert.throws(() => admit(c, [{...current, run_attempt: 2}], now), /Historical rerun/);
  assert.throws(() => admit(c, [{...current, run_attempt: undefined}], now), /Historical rerun/);
});

test('unrelated new main and a run-wide failure cannot authorize a repair or retry a successful platform', () => {
  const {env, ev, context: c} = bootstrapFixture();
  for (const platforms of ['both', 'linux', 'macos']) {
    assert.throws(() => p.requestContext(env,
      {...ev, inputs: {...ev.inputs, platforms, repair_of: '122'}}, driver, true), /Retries are disabled/);
    assert.throws(() => p.admitHistory({...c, repair_of: '122'}, {total_count: 1,
      workflow_runs: [historyRun(c, Date.now(), {head_sha: '8'.repeat(40), conclusion: 'failure'})]}),
    /Retries are disabled/);
  }
});

test('delayed acquisition retains queued reservations and24hours after provider completion, never creation', () => {
  const {context: c} = bootstrapFixture();
  const now = Date.parse('2026-11-17T06:00:00Z');
  const current = historyRun(c, now);
  const delayed = {id: 120, head_sha: '8'.repeat(40), run_attempt: 1, event: 'workflow_dispatch',
    head_branch: 'main', status: 'completed', conclusion: 'success',
    created_at: new Date(now - 25 * 3600000).toISOString(),
    updated_at: new Date(now - 3600000).toISOString(),
    display_title: `posthoc:${'9'.repeat(40)}:bootstrap-v1:both:initial`};
  assert.equal(admit(c, [current, delayed], now).reserved_platform_jobs_24h, 4);
  assert.equal(admit(c, [current, {...delayed, status: 'queued'}], now)
    .reserved_platform_jobs_24h, 4);
  const initial = {...delayed, id: 122, conclusion: 'failure',
    created_at: new Date(now - 7 * 3600000).toISOString(),
    updated_at: new Date(now - 6 * 3600000).toISOString()};
  assert.throws(() => admit(c, [current, initial, delayed], now), /reservation/);
  const expired = {...delayed, created_at: new Date(now - 30 * 3600000).toISOString(),
    updated_at: new Date(now - 25 * 3600000).toISOString()};
  assert.equal(admit(c, [current, expired], now).reserved_platform_jobs_24h, 2);
  for (const change of [{updated_at: undefined}, {updated_at: 'malformed'},
    {updated_at: new Date(now - 26 * 3600000).toISOString()}, {status: 'unknown'}]) {
    assert.throws(() => admit(c, [current, {...delayed, ...change}], now));
  }
});

test('source and driver integrity failures cannot bypass private cleanup or artifact budget finalization', t => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'posthoc-finalize-'));
  t.after(() => fs.rmSync(directory, {recursive: true, force: true}));
  const privateRoot = path.join(directory, 'owned-private');
  fs.mkdirSync(privateRoot);
  fs.writeFileSync(path.join(privateRoot, 'cold-fixture'), 'offline only');
  const report = {passed: false, status: 'FAIL_OR_CENSORED_PARTIAL', error: 'original command failure'};
  const calls = [];
  assert.equal(p.finalizeBootstrap(report, [
    ['source_integrity', () => { calls.push('source'); throw new Error('injected source mutation'); }],
    ['driver_integrity', () => { calls.push('driver'); throw new Error('injected driver failure'); }],
    ['private_cleanup', () => {
      calls.push('cleanup');
      fs.rmSync(privateRoot, {recursive: true});
      report.private_cleanup = {confirmed: !fs.existsSync(privateRoot)};
    }],
    ['artifact_budget', () => { calls.push('budget'); throw new Error('injected budget failure'); }],
    ['driver_hashes', () => { calls.push('hashes'); }],
  ]), false);
  assert.deepEqual(calls, ['source', 'driver', 'cleanup', 'budget', 'hashes']);
  assert.equal(report.private_cleanup.confirmed, true);
  assert.equal(report.error, 'original command failure');
  assert.equal(report.passed, false);
  assert.deepEqual(report.finalization_errors.map(error => error.phase),
    ['source_integrity', 'driver_integrity', 'artifact_budget']);
  const cleanupFailure = {};
  const continued = [];
  assert.equal(p.finalizeBootstrap(cleanupFailure, [
    ['private_cleanup', () => { throw new Error('cleanup refused'); }],
    ['artifact_budget', () => { continued.push('budget'); }],
  ]), false);
  assert.deepEqual(continued, ['budget']);
  assert.equal(cleanupFailure.finalization_errors[0].phase, 'private_cleanup');
});

test('native command environment is an allowlist, not a token/auth/config/identity fallback', () => {
  const env = p.commandEnvironment({PATH: '/tools', GH_TOKEN: 'never-forward', GITHUB_TOKEN: 'never-forward',
    AZURE_CLIENT_SECRET: 'never-forward', FAB_TOKEN: 'never-forward', NODE_OPTIONS: 'unsafe',
    RUSTC_WRAPPER: 'unsafe', CARGO_HOME: '/old', RUSTUP_HOME: '/global',
    GIT_CONFIG_COUNT: '1', GITHUB_SHA: source}, {HOME: '/private/home', CARGO_HOME: '/private/cargo',
    RUSTUP_HOME: '/private/rustup', CARGO_TARGET_DIR: '/private/target', TMPDIR: '/private/tmp'});
  assert.equal(env.CARGO_HOME, '/private/cargo');
  assert.equal(env.RUSTUP_HOME, '/private/rustup');
  assert.equal(env.CARGO_NET_RETRY, '1');
  for (const key of ['GH_TOKEN', 'GITHUB_TOKEN', 'AZURE_CLIENT_SECRET', 'FAB_TOKEN',
    'NODE_OPTIONS', 'RUSTC_WRAPPER', 'GIT_CONFIG_COUNT', 'GITHUB_SHA']) {
    assert.equal(Object.hasOwn(env, key), false);
  }
});

test('unexpected provider tokens are rejected without echoing their values into failure diagnostics', () => {
  p.requireNoProviderTokens({});
  const sentinel = 'OFFLINE_SENTINEL_NOT_A_CREDENTIAL';
  for (const key of ['GH_TOKEN', 'GITHUB_TOKEN']) {
    assert.throws(() => p.requireNoProviderTokens({[key]: sentinel}), error => {
      assert.match(error.message, /Provider token must not enter native stage/);
      assert.equal(error.message.includes(sentinel), false);
      return true;
    });
    assert.throws(() => p.requireNoProviderTokens({[key]: ''}));
  }
});

function ownedProcessFixture(t) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'posthoc-process-'));
  t.after(() => fs.rmSync(directory, {recursive: true, force: true}));
  const revision = '61c8221914152e231e5e64f7bd38418f66264999';
  for (const name of ['validation-process.cjs', 'validation-process.py',
    'validation_posix.py', 'native_acceptance.py']) {
    const result = spawnSync('git', ['show', `${revision}:rust/scripts/${name}`],
      {cwd: path.resolve(__dirname, '../..'), maxBuffer: 1024 ** 2, timeout: 10000,
        env: p.gitEnvironment()});
    assert.equal(result.status, 0, 'Exact offline source supervisor object must be locally available');
    fs.writeFileSync(path.join(directory, name), result.stdout);
  }
  const {executeOwned} = require(path.join(directory, 'validation-process.cjs'));
  const execute = (program, args, options) => executeOwned(program, args, {
    ...options, python: process.platform === 'win32' ? 'python' : 'python3',
  });
  const report = {commands: []};
  const options = {directory, deadline: Date.now() + 20000,
    env: p.gitEnvironment(), report, save: () => {}};
  const enablePipes = async () => {
    if (process.platform !== 'win32') {
      const pipes = ['stdout', 'stderr'].map(stream => path.join(directory, `01-${stream}.pipe`));
      const setup = await execute('python3', ['-B', '-c',
        'import os,sys; [os.mkfifo(p, 0o600) for p in sys.argv[1:]]', ...pipes],
      {cwd: directory, env: options.env, stdout: path.join(directory, 'pipe-setup-stdout.log'),
        stderr: path.join(directory, 'pipe-setup-stderr.log'), timeout: 5000});
      assert.equal(setup.status, 0);
      assert.equal(setup.cleanup_confirmed, true);
      options.transportRoot = directory;
    }
  };
  return {directory, execute, report, options, enablePipes};
}

test('real existing supervisor executes only harmless Node fixture and binds output/cleanup receipts', async t => {
  const f = ownedProcessFixture(t);
  await f.enablePipes();
  const result = await p.boundedCommand(f.execute, {id: 'offline-node-not-native',
    program: process.execPath, args: ['-e', 'process.stdout.write("offline fixture\\n")'],
    cwd: f.directory, timeout_ms: 5000}, f.options);
  assert.equal(result.stdout, 'offline fixture\n');
  assert.equal(f.report.commands[0].cleanup_confirmed, true);
  assert.equal(f.report.commands[0].status, 'COMPLETE');
  assert.equal(f.report.commands[0].stdout.bytes, 16);
  assert.match(f.report.commands[0].stdout.sha256, /^[a-f0-9]{64}$/);
});

test('real supervisor timeout and over-output remain incomplete/censored with finite retained bytes', async t => {
  for (const [script, timeout] of [['setInterval(() => {}, 100)', 200],
    [`process.stdout.write("x".repeat(${p.POLICY.outputBytes + 65536}))`, 5000]]) {
    const f = ownedProcessFixture(t);
    await f.enablePipes();
    await assert.rejects(p.boundedCommand(f.execute, {id: 'offline-failure-not-native',
      program: process.execPath, args: ['-e', script], cwd: f.directory, timeout_ms: timeout}, f.options));
    const entry = f.report.commands[0];
    assert.equal(entry.status, 'INCOMPLETE');
    assert.equal(entry.cleanup_confirmed, true);
    assert.ok(entry.timed_out || entry.censored);
    assert.ok(entry.stdout.bytes <= p.POLICY.outputBytes);
    assert.ok(entry.stderr.bytes <= p.POLICY.outputBytes);
  }
});

test('real child receives no ambient provider/identity tokens and cancellation confirms owned cleanup', async t => {
  const f = ownedProcessFixture(t);
  await f.enablePipes();
  const env = p.commandEnvironment({...process.env, GH_TOKEN: 'offline-provider-sentinel',
    GITHUB_TOKEN: 'offline-provider-sentinel', AZURE_CLIENT_SECRET: 'offline-identity-sentinel'}, {});
  const result = await p.boundedCommand(f.execute, {id: 'offline-env-not-native',
    program: process.execPath, args: ['-e',
      'process.stdout.write(JSON.stringify(Object.keys(process.env).filter(k => /TOKEN|SECRET|AZURE|FAB_/i.test(k))))'],
    cwd: f.directory, timeout_ms: 5000}, {...f.options, env});
  assert.equal(result.stdout, '[]');
  const cancelled = ownedProcessFixture(t);
  await cancelled.enablePipes();
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 200);
  try {
    await assert.rejects(p.boundedCommand(cancelled.execute, {id: 'offline-cancel-not-native',
      program: process.execPath, args: ['-e', 'setInterval(() => {}, 100)'],
      cwd: cancelled.directory, timeout_ms: 5000}, {...cancelled.options, signal: controller.signal}));
    assert.equal(cancelled.report.commands[0].cancelled, true);
    assert.equal(cancelled.report.commands[0].cleanup_confirmed, true);
    assert.equal(cancelled.report.commands[0].status, 'INCOMPLETE');
  } finally { clearTimeout(timer); }
});
